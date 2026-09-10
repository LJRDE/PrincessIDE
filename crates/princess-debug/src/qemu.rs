//! QEMU lifecycle for the debug target: start the machine with a GDB stub,
//! and — the part that actually bites — **never leave an orphan**.
//!
//! # The orphan problem
//!
//! `qemu-system-x86_64` is not a single process.  When the probes for this
//! phase started a machine and later refused to shut it down (e.g. the debugger
//! session hangs, or the acceptance script is killed by its own `timeout`), the
//! machine keeps running, keeps holding `:1234`, and holds a few hundred MiB.
//! On a 3.8 GiB host with a 4 GiB swapfile that is not a theoretical concern:
//! leaked QEMUs are what turn the *next* run into an unexplainable failure.
//!
//! Two things prevent it, and both are required:
//!
//! 1. **`setsid` (via `process_group(0)` / `start_new_session`)** — QEMU is put
//!    in its own process group, so the whole group can be signalled at once and
//!    the engine's own group is never hit by the teardown.
//! 2. **The engine always reaps the group**, on every exit path: normal stop,
//!    error return, cancellation, and `Drop`.  A `Drop` impl is the only way to
//!    cover the "the caller returned early" path that a manual `stop()` misses.
//!
//! The caller is *also* expected to wrap the whole acceptance run in the shell
//! `timeout` command (see the acceptance script): if the process is SIGKILLed
//! from outside, even `Drop` does not run, and `timeout`'s own
//! `--foreground`/kill semantics are the only thing left.  Defence in depth is
//! deliberate here — this is the difference between a flaky suite and a suite
//! that fails the same way twice.
//!
//! # `-s -S` and why the symbols still have to be loaded by hand
//!
//! `-S` freezes the vCPU before the first instruction; `-s` opens the gdbserver
//! on `:1234` (it is shorthand for `-gdb tcp::1234`).  Because this kernel boots
//! through GRUB from an ISO, QEMU never sees the ELF as a *kernel*, so the
//! gdbstub has **no symbol table at all** — the P4 probe measured
//! `hbreak paging_fault_probe` coming back `pending` until `symbol-file <elf>`
//! was issued.  Loading symbols is therefore part of the session setup, not an
//! optional nicety (research C §5 "无 OS 下的符号加载").

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use princess_core::{ErrorCode, PrincessError, Result};

/// Everything needed to boot one debuggable machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QemuCommand {
    /// `argv[0]` first — normally `qemu-system-x86_64`.
    pub argv: Vec<String>,
    /// Working directory for the child.
    pub cwd: PathBuf,
    /// Absolute path of the ELF whose symbols the session will load.
    pub symbols: Option<PathBuf>,
    /// How long to wait for the stub port to accept connections.
    pub ready_timeout: Duration,
}

impl QemuCommand {
    /// Render the argv the way the acceptance report needs it: copy-pasteable.
    pub fn display(&self) -> String {
        let mut parts = Vec::with_capacity(self.argv.len() + 2);
        parts.push(format!("cd {}", shell_quote(&self.cwd.to_string_lossy())));
        parts.extend(self.argv.iter().map(|a| shell_quote(a)));
        parts.join(" ")
    }
}

/// Quote one argument for `sh` so the reported command can be re-run verbatim.
pub fn shell_quote(text: &str) -> String {
    if !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/:@=+,".contains(c))
    {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// A running machine, owned by the engine.
///
/// Dropping this **kills the whole process group and waits for it**, so a
/// `?`/early return in a test can never leak a machine.
pub struct QemuProcess {
    child: Child,
    command: QemuCommand,
    stopped: bool,
}

impl std::fmt::Debug for QemuProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QemuProcess")
            .field("pid", &self.child.id())
            .field("argv", &self.command.argv)
            .field("stopped", &self.stopped)
            .finish()
    }
}

impl QemuProcess {
    /// Start QEMU in its own session, then wait until the GDB stub answers.
    ///
    /// Waiting for the stub *before* returning is what makes the subsequent
    /// `attach` deterministic; a plain `sleep` is what produced the flaky
    /// `connection refused` in the first P4 probe.
    pub fn start(command: QemuCommand) -> Result<Self> {
        if command.argv.is_empty() {
            return Err(PrincessError::new(
                ErrorCode::QemuFailed,
                "QEMU command is empty",
            ));
        }

        let mut cmd = Command::new(&command.argv[0]);
        cmd.args(&command.argv[1..])
            .current_dir(&command.cwd)
            // QEMU writes to the serial log we configure; keep its own stdio
            // quiet but *captured* so a startup failure has evidence.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Own process group / session: teardown signals the whole group and
        // cannot reach the engine's own.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let child = cmd.spawn().map_err(|err| {
            let code = match err.kind() {
                std::io::ErrorKind::NotFound => ErrorCode::ToolchainMissing,
                _ => ErrorCode::QemuFailed,
            };
            PrincessError::new(code, format!("could not start {}", command.argv[0]))
                .with_detail(format!("{}\n{err}", command.display()))
        })?;

        let mut process = QemuProcess {
            child,
            command,
            stopped: false,
        };
        process.wait_until_ready()?;
        Ok(process)
    }

    /// Poll the GDB stub port until it accepts, or the process dies, or we run
    /// out of patience.
    fn wait_until_ready(&mut self) -> Result<()> {
        let Some(port) = self.stub_port() else {
            // No stub requested (`-s` absent): nothing to wait for.
            return Ok(());
        };
        let deadline = Instant::now() + self.command.ready_timeout;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(self.startup_failure(format!("QEMU exited before the GDB stub opened ({status})")));
            }
            match std::net::TcpStream::connect_timeout(
                &format!("127.0.0.1:{port}").parse().expect("literal socket addr"),
                Duration::from_millis(200),
            ) {
                Ok(_) => return Ok(()),
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(err) => {
                    return Err(self.startup_failure(format!(
                        "GDB stub on 127.0.0.1:{port} never accepted a connection within {:?}: {err}",
                        self.command.ready_timeout
                    )));
                }
            }
        }
    }

    /// Build the "QEMU refused to start" error, including whatever the process
    /// already printed — never a bare "failed to connect".
    fn startup_failure(&mut self, message: String) -> PrincessError {
        let captured = self.drain_output();
        let _ = self.terminate_group();
        PrincessError::new(ErrorCode::QemuFailed, "QEMU did not come up for debugging")
            .with_detail(format!("{}\n{message}\n{captured}", self.command.display()))
    }

    /// Best-effort read of whatever QEMU printed before we kill it.
    fn drain_output(&mut self) -> String {
        use std::io::Read;
        let mut text = String::new();
        if let Some(stderr) = self.child.stderr.as_mut() {
            let mut buf = Vec::new();
            let _ = stderr.take(64 * 1024).read_to_end(&mut buf);
            text.push_str(&String::from_utf8_lossy(&buf));
        }
        if let Some(stdout) = self.child.stdout.as_mut() {
            let mut buf = Vec::new();
            let _ = stdout.take(64 * 1024).read_to_end(&mut buf);
            let extra = String::from_utf8_lossy(&buf);
            if !extra.trim().is_empty() {
                text.push_str(&extra);
            }
        }
        text
    }

    /// The TCP port from `-s` / `-gdb tcp::PORT`, when one was requested.
    pub fn stub_port(&self) -> Option<u16> {
        let argv = &self.command.argv;
        for (index, arg) in argv.iter().enumerate() {
            if arg == "-s" || arg == "-gdb" {
                if arg == "-s" {
                    // `-s` is documented as `-gdb tcp::1234`.
                    return Some(1234);
                }
                if let Some(value) = argv.get(index + 1) {
                    if let Some(port) = parse_gdb_spec(value) {
                        return Some(port);
                    }
                }
            }
            // `-gdb tcp::1234` as one token is not a thing; `-gdb=tcp::1234` is.
            if let Some(rest) = arg.strip_prefix("-gdb=") {
                if let Some(port) = parse_gdb_spec(rest) {
                    return Some(port);
                }
            }
        }
        None
    }

    /// The exact command that was run.
    pub fn command(&self) -> &QemuCommand {
        &self.command
    }

    /// Process id of the emulator.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Whether the machine is still alive.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Kill the process group and reap it.  Idempotent.
    pub fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        self.terminate_group()?;
        self.stopped = true;
        Ok(())
    }

    fn terminate_group(&mut self) -> Result<()> {
        #[cfg(unix)]
        {
            let pgid = self.child.id() as i32;
            // SIGTERM first: QEMU flushes its block devices (the serial log is
            // already on disk, but a clean exit keeps the ISO/QCOW state sane).
            unsafe {
                libc::killpg(pgid, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    _ => {
                        // SIGKILL the whole group: this is the guarantee.
                        unsafe {
                            libc::killpg(pgid, libc::SIGKILL);
                        }
                        let _ = self.child.wait();
                        break;
                    }
                }
            }
            // Belt and braces: a group member that outlived the leader (QEMU
            // spawns helper threads, not processes, but a future `-chardev`
            // setup might not) is still addressed by the same pgid.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        Ok(())
    }
}

impl Drop for QemuProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Extract the port from a GDB stub spec such as `tcp::1234`, `:1234`,
/// `localhost:1234` or `1234`.
fn parse_gdb_spec(spec: &str) -> Option<u16> {
    let tail = spec.rsplit(':').next()?;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Build the standard debug-boot argv for a GRUB ISO target.
///
/// The flags mirror the fixture's own `run.sh` (which is the verified-good
/// invocation) and add only the two the debugger needs:
///
/// * `-s` — gdbserver on `tcp::1234`
/// * `-S` — freeze the CPU before the first instruction, so a breakpoint set
///   afterwards cannot be missed
///
/// Serial goes to a **file**, not `stdio`: the engine must hold no pipe it does
/// not read, because a full pipe blocks QEMU (contract §6 rule 2 — "必须保证有
/// 读者，否则 QEMU 会因管道写满而阻塞").  A file has no reader requirement and
/// keeps the boot log inspectable after the run.
#[allow(clippy::too_many_arguments)]
pub fn debug_boot_argv(
    qemu: &str,
    qemu_data_dir: Option<&Path>,
    iso: &Path,
    memory: &str,
    serial_log: &Path,
    stub_port: u16,
    frozen_at_start: bool,
) -> Vec<String> {
    let mut argv = vec![qemu.to_string()];
    if let Some(dir) = qemu_data_dir {
        argv.push("-L".into());
        argv.push(dir.to_string_lossy().into_owned());
    }
    argv.extend(
        [
            "-m",
            memory,
            "-cdrom",
        ]
        .iter()
        .map(|s| (*s).to_string()),
    );
    argv.push(iso.to_string_lossy().into_owned());
    argv.extend(["-boot", "d", "-display", "none", "-serial"].iter().map(|s| (*s).to_string()));
    argv.push(format!("file:{}", serial_log.to_string_lossy()));
    argv.extend(["-monitor", "none", "-no-reboot"].iter().map(|s| (*s).to_string()));
    argv.push("-gdb".into());
    argv.push(format!("tcp::{}", stub_port));
    if frozen_at_start {
        argv.push("-S".into());
    }
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_port_is_found_for_every_spelling_of_the_flag() {
        let cases: [(&[&str], Option<u16>); 5] = [
            (&["qemu-system-x86_64", "-s", "-S"], Some(1234)),
            (&["qemu-system-x86_64", "-gdb", "tcp::1235", "-S"], Some(1235)),
            (&["qemu-system-x86_64", "-gdb", ":4321"], Some(4321)),
            (&["qemu-system-x86_64", "-gdb=tcp::9999"], Some(9999)),
            (&["qemu-system-x86_64", "-m", "256M"], None),
        ];
        for (argv, expected) in cases {
            let command = QemuCommand {
                argv: argv.iter().map(|s| (*s).to_string()).collect(),
                cwd: PathBuf::from("/"),
                symbols: None,
                ready_timeout: Duration::from_secs(1),
            };
            let process = QemuProcess {
                // Never spawned: the port parser is pure, this only gives us a
                // receiver to call it on.
                child: Command::new("/bin/true").spawn().expect("spawn /bin/true"),
                command,
                stopped: true,
            };
            assert_eq!(process.stub_port(), expected, "argv: {argv:?}");
        }
    }

    #[test]
    fn debug_boot_argv_matches_the_verified_fixture_invocation() {
        let argv = debug_boot_argv(
            "qemu-system-x86_64",
            Some(Path::new("/qemu/data")),
            Path::new("/fixtures/paging-kernel/build/pagingkernel.iso"),
            "256M",
            Path::new("/tmp/serial.log"),
            1234,
            true,
        );
        // The three flags that make the debug boot work.
        assert!(argv.contains(&"-cdrom".to_string()), "{argv:?}");
        assert!(argv.contains(&"-boot".to_string()), "{argv:?}");
        assert_eq!(argv[argv.iter().position(|a| a == "-boot").unwrap() + 1], "d");
        assert!(argv.contains(&"tcp::1234".to_string()), "{argv:?}");
        assert!(argv.contains(&"-S".to_string()), "{argv:?}");
        // D9: never -nographic, never `-monitor stdio`.
        assert!(!argv.iter().any(|a| a == "-nographic"), "{argv:?}");
        assert!(!argv.windows(2).any(|w| w[0] == "-monitor" && w[1] == "stdio"), "{argv:?}");
        // Serial goes to a file so no unread pipe can block QEMU.
        let serial = argv[argv.iter().position(|a| a == "-serial").unwrap() + 1].clone();
        assert!(serial.starts_with("file:"), "{serial}");
    }

    #[test]
    fn shell_quoting_round_trips_through_sh() {
        let annoying = "/a path/with spaces/it's.log";
        let quoted = shell_quote(annoying);
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf %s {quoted}"))
            .output()
            .expect("run sh");
        assert_eq!(String::from_utf8_lossy(&output.stdout), annoying);
        // Simple tokens stay readable in the report.
        assert_eq!(shell_quote("qemu-system-x86_64"), "qemu-system-x86_64");
    }

    #[test]
    fn display_is_a_runnable_command_line() {
        let command = QemuCommand {
            argv: vec!["qemu-system-x86_64".into(), "-s".into(), "-S".into()],
            cwd: PathBuf::from("/tmp/my dir"),
            symbols: None,
            ready_timeout: Duration::from_secs(1),
        };
        assert_eq!(command.display(), "cd '/tmp/my dir' qemu-system-x86_64 -s -S");
    }

    /// The whole point of this module: a dropped process must not survive.
    #[test]
    fn dropping_the_process_kills_the_whole_group() {
        let dir = std::env::temp_dir().join(format!("princess-debug-qemu-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let pid_file = dir.join("child.pid");

        // Stand in for QEMU: a shell that spawns a grandchild and then sleeps.
        // If teardown only killed the leader, the grandchild would survive and
        // this test would report it.
        let command = QemuCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!(
                    "sh -c 'echo $$ > {grand}; exec sleep 300' & echo $$ > {leader}; wait",
                    grand = pid_file.with_extension("grand").display(),
                    leader = pid_file.display()
                ),
            ],
            cwd: dir.clone(),
            symbols: None,
            // No stub in the argv, so `start` returns immediately.
            ready_timeout: Duration::from_millis(500),
        };
        let process = QemuProcess::start(command).expect("start stand-in");
        let leader = process.pid();
        std::thread::sleep(Duration::from_millis(300));
        assert!(process_exists(leader), "the stand-in should be running");
        drop(process);

        // Give the kernel a moment to reap.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!process_exists(leader), "the process group leader survived drop()");

        if let Ok(text) = std::fs::read_to_string(pid_file.with_extension("grand")) {
            if let Ok(grand) = text.trim().parse::<u32>() {
                assert!(
                    !process_exists(grand),
                    "a process-group member (pid {grand}) survived teardown"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `stop()` must be idempotent: a caller that stops explicitly and then
    /// drops must not double-signal or error.
    #[test]
    fn stop_is_idempotent() {
        let command = QemuCommand {
            argv: vec!["/bin/sleep".into(), "300".into()],
            cwd: std::env::temp_dir(),
            symbols: None,
            ready_timeout: Duration::from_millis(200),
        };
        let mut process = QemuProcess::start(command).expect("start sleep");
        process.stop().expect("first stop");
        process.stop().expect("second stop");
        assert!(!process.is_running());
    }

    /// A machine that dies instantly must produce a real error naming the argv,
    /// not a silent "ready".
    #[test]
    fn a_machine_that_exits_immediately_is_a_qemu_failure() {
        let command = QemuCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "echo 'qemu: could not load kernel' >&2; exit 1".into(),
                "-s".into(),
            ],
            cwd: std::env::temp_dir(),
            symbols: None,
            ready_timeout: Duration::from_secs(2),
        };
        let err = QemuProcess::start(command).unwrap_err();
        assert_eq!(err.code, ErrorCode::QemuFailed);
        let detail = err.detail.as_deref().unwrap();
        assert!(detail.contains("could not load kernel"), "stderr must be kept: {detail}");
    }

    #[test]
    fn missing_qemu_binary_is_a_toolchain_error() {
        let command = QemuCommand {
            argv: vec!["/nonexistent/qemu-system-x86_64-does-not-exist".into()],
            cwd: std::env::temp_dir(),
            symbols: None,
            ready_timeout: Duration::from_millis(200),
        };
        let err = QemuProcess::start(command).unwrap_err();
        assert_eq!(err.code, ErrorCode::ToolchainMissing);
    }

    fn process_exists(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}
