//! Process-group helpers (contract §6 rule 3: "杀进程组，不能只杀父进程留孤儿").
//!
//! Every child the engine starts gets its own process group
//! (`Command::process_group(0)`), so a single `kill(-pgid, SIG)` reaches the
//! child *and* everything it spawned — `make` spawns `gcc`, `grub-mkrescue`
//! spawns `xorriso`/`mformat`, and killing only the parent would leave those
//! writing into `build/` after the operation was reported as cancelled.

use princess_core::{ErrorCode, PrincessError, Result};

/// Send `signal` to the whole process group led by `pgid`.
///
/// `ESRCH` (the group is already gone) is success: the post-condition the engine
/// promises is "no survivors", not "the signal was delivered".
pub fn kill_group(pgid: u32, signal: i32) -> Result<()> {
    if pgid == 0 {
        return Err(PrincessError::internal(
            "refusing to signal process group 0 (would signal this process)",
        ));
    }
    // SAFETY: `kill` with a negative pid signals the process group; both
    // arguments are plain integers and the return value is checked.
    let rc = unsafe { libc::kill(-(pgid as i32), signal) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        return Ok(());
    }
    Err(PrincessError::new(
        ErrorCode::Internal,
        format!("cannot signal process group {pgid}: {err}"),
    ))
}

/// Whether any process in the group is still **running**.
///
/// Deliberately not `kill(-pgid, 0)`: that also succeeds while the group only
/// contains zombies, and a killed-but-not-yet-reaped child is exactly the state
/// the engine is in when it checks.  `/proc` is scanned for group members whose
/// state is not `Z`, which is the post-condition that matters ("no orphan was
/// left behind", not "the kernel still remembers the pids").
pub fn group_alive(pgid: u32) -> bool {
    if pgid == 0 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        // Without /proc, fall back to the coarse check.
        // SAFETY: signal 0 performs error checking only and delivers nothing.
        return unsafe { libc::kill(-(pgid as i32), 0) == 0 };
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|text| text.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // `pid (comm) state ppid pgrp ...`; `comm` may contain spaces and
        // parentheses, so split after the *last* ')'.
        let Some((_, rest)) = stat.rsplit_once(") ") else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let state = fields[0];
        let Ok(process_group) = fields[2].parse::<u32>() else {
            continue;
        };
        if process_group == pgid && state != "Z" {
            return true;
        }
    }
    false
}

/// Kill a group and wait (bounded) until it is really gone.
///
/// Returns `Ok(())` when no member of the group survives.
pub async fn kill_group_and_wait(pgid: u32) -> Result<()> {
    kill_group(pgid, libc::SIGTERM)?;
    for _ in 0..20 {
        if !group_alive(pgid) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    kill_group(pgid, libc::SIGKILL)?;
    for _ in 0..40 {
        if !group_alive(pgid) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    Err(PrincessError::internal(format!(
        "process group {pgid} still has survivors after SIGKILL"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    #[test]
    fn a_zombie_group_member_is_not_a_survivor() {
        // Start a child in its own group that exits immediately: once it is a
        // zombie its group must no longer count as alive.
        let mut child = std::process::Command::new("true")
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pgid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(150));
        // Not reaped yet on purpose: `true` is a zombie right now.
        assert!(!group_alive(pgid), "a zombie must not count as a survivor");
        let _ = child.wait();
        assert!(!group_alive(pgid));
    }

    #[test]
    fn a_live_group_member_is_detected() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pgid = child.id();
        assert!(group_alive(pgid));
        kill_group(pgid, libc::SIGKILL).unwrap();
        let _ = child.wait();
        assert!(!group_alive(pgid));
    }

    #[test]
    fn signalling_a_dead_group_is_not_an_error() {
        // 0 is refused (it would signal the whole session).
        let err = kill_group(0, libc::SIGKILL).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(err.message.contains("group 0"), "{err}");

        // A pgid that cannot exist: post-condition already satisfied.
        kill_group(999_999_999, libc::SIGKILL).unwrap();
        assert!(!group_alive(999_999_999));
    }

}
