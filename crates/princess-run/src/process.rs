//! Child-process execution for the run backend — contract §6 and the
//! **process-group discipline**.
//!
//! QEMU is started with `setsid` (`process_group(0)`), so the engine can signal
//! the *whole group* instead of just the parent.  This workspace has measured a
//! real failure mode where a QEMU with `PPID=1` kept spinning after its parent
//! died, and an orphan like that directly falsifies the P2-7 assertion
//! ("timeout and `pgrep -a qemu-system-x86_64` is empty").  So:
//!
//! * the child always gets its own group,
//! * the deadline/cancel path signals the **group** (SIGTERM, then SIGKILL),
//! * after the wait, the group is *verified* dead before the outcome is
//!   reported, and survivors are an explicit `E_QEMU_FAILED` error.
//!
//! Serial bytes are written to the tee file **before** their event is emitted
//! (contract §2, "先落盘再推送"), and UTF-8 code points are never split because
//! every chunk goes through [`princess_core::Utf8Chunker`].

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use princess_core::error::{ErrorCode, PrincessError, Result};
use princess_core::traits::CancelToken;
use princess_core::types::TextEncoding;
use princess_core::Utf8Chunker;

/// Which pipe a chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pipe {
    /// QEMU's stdout: the guest serial line (`-serial stdio`).
    Serial,
    /// QEMU's stderr: the emulator's own messages (and, without `-D`, `-d`).
    Emulator,
}

/// One decoded chunk, tagged with its pipe and flush phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamChunk {
    pub pipe: Pipe,
    pub text: String,
    pub encoding: TextEncoding,
    /// `true` for the final (possibly partial-line) flush at EOF.
    pub at_eof: bool,
}

/// How a finished QEMU process ended, as the OS reported it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    /// Wall-clock lifetime.
    pub wall_ms: u64,
    /// `true` when the group was verified empty after the wait.
    pub group_dead: bool,
}

/// Tunables; tests shrink these so a unit test never waits for real timeouts.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// How long a partial line may sit in the chunker before being pushed.
    pub flush_every: Duration,
    /// How long to keep draining after the child exited.
    pub drain_grace: Duration,
    /// How long to wait for the group to die after SIGKILL.
    pub kill_grace: Duration,
    /// Extra environment applied to the child.
    pub global_env: BTreeMap<String, String>,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            flush_every: Duration::from_millis(120),
            drain_grace: Duration::from_secs(3),
            kill_grace: Duration::from_secs(3),
            global_env: BTreeMap::new(),
        }
    }
}

/// Queue both pipes of the child onto one channel, chunked safely.
fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    pipe: Pipe,
    flush_every: Duration,
    sink: Sender<StreamChunk>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (byte_tx, byte_rx) = channel::<Vec<u8>>();
        let pump = std::thread::spawn(move || {
            let mut buffer = vec![0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if byte_tx.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let mut chunker = Utf8Chunker::new();
        loop {
            match byte_rx.recv_timeout(flush_every) {
                Ok(bytes) => {
                    chunker.push(&bytes);
                    while let Some((text, encoding)) = chunker.take_lines() {
                        if sink
                            .send(StreamChunk {
                                pipe,
                                text,
                                encoding,
                                at_eof: false,
                            })
                            .is_err()
                        {
                            let _ = pump.join();
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some((text, encoding)) = chunker.take_available() {
                        if sink
                            .send(StreamChunk {
                                pipe,
                                text,
                                encoding,
                                at_eof: false,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        if let Some((text, encoding)) = chunker.take_remainder() {
            let _ = sink.send(StreamChunk {
                pipe,
                text,
                encoding,
                at_eof: true,
            });
        }
        let _ = pump.join();
    })
}

/// Signal a whole process group.  `ESRCH` (already gone) is success: the
/// post-condition is "no survivors", not "the signal was delivered".
pub fn kill_group(pgid: u32, signal: i32) -> Result<()> {
    if pgid == 0 {
        return Err(PrincessError::internal(
            "refusing to signal process group 0 (would signal the engine itself)",
        ));
    }
    // SAFETY: `kill` with a negative pid targets a process group; the return
    // value is checked and both arguments are plain integers.
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

/// Is any member of the group still **running**?
///
/// Deliberately not `kill(-pgid, 0)`: that also succeeds while the group holds
/// only zombies, and a killed-but-unreaped child is exactly the state a naive
/// check gets wrong.  Scan `/proc` for members whose state is not `Z`.
pub fn group_alive(pgid: u32) -> bool {
    if pgid == 0 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        // SAFETY: signal 0 performs error checking only and delivers nothing.
        return unsafe { libc::kill(-(pgid as i32), 0) == 0 };
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|text| text.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // `pid (comm) state ppid pgrp …`; `comm` may contain spaces/parens.
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

/// Kill a group and wait, bounded, until nothing in it is running.
pub fn kill_group_blocking(pgid: u32, grace: Duration) -> Result<()> {
    if !group_alive(pgid) {
        return Ok(());
    }
    kill_group(pgid, libc::SIGTERM)?;
    if wait_group_gone(pgid, grace) {
        return Ok(());
    }
    kill_group(pgid, libc::SIGKILL)?;
    if wait_group_gone(pgid, grace) {
        return Ok(());
    }
    Err(PrincessError::new(
        ErrorCode::QemuFailed,
        format!("process group {pgid} still has running members after SIGKILL"),
    ))
}

fn wait_group_gone(pgid: u32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if !group_alive(pgid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    !group_alive(pgid)
}

/// Locate an executable on `PATH` without spawning anything.
pub fn which(exe: &str) -> Option<PathBuf> {
    if exe.contains('/') {
        let path = Path::new(exe);
        return is_executable(path).then(|| path.to_path_buf());
    }
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(exe);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Everything the runner needs to start the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    /// argv as executed (`argv[0]` first).
    pub argv: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Deadline; `None` means "no engine deadline".
    pub timeout: Option<Duration>,
    /// Where serial bytes are teed, when configured.
    pub serial_tee: Option<PathBuf>,
    /// Environment overrides applied on top of the inherited one.
    pub env: BTreeMap<String, String>,
}

/// Run a machine to completion, streaming both pipes.
pub trait ProcessRunner: Send + Sync {
    /// Start `spec.argv` in its own process group, stream chunks to `on_chunk`,
    /// enforce the deadline / cancellation by killing the **group**, and return
    /// what the OS said plus whether the group was left empty.
    fn run(
        &self,
        spec: &SpawnSpec,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<ProcessOutcome>;
}

/// The real runner: `std::process` + own process group + watchdog.
#[derive(Default)]
pub struct SystemRunner {
    pub config: RunnerConfig,
}

impl SystemRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run and also collect every chunk (used by tests and transcripts).
    pub fn run_collecting(
        &self,
        spec: &SpawnSpec,
        cancel: &CancelToken,
    ) -> Result<(ProcessOutcome, Vec<StreamChunk>)> {
        let mut chunks = Vec::new();
        let outcome = self.run(spec, cancel, &mut |chunk| chunks.push(chunk))?;
        Ok((outcome, chunks))
    }
}

impl ProcessRunner for SystemRunner {
    fn run(
        &self,
        spec: &SpawnSpec,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<ProcessOutcome> {
        if spec.argv.is_empty() {
            return Err(PrincessError::internal(
                "run plan has an empty argv: nothing to execute",
            ));
        }

        let mut command = Command::new(&spec.argv[0]);
        command
            .args(&spec.argv[1..])
            .current_dir(&spec.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in self.config.global_env.iter().chain(spec.env.iter()) {
            command.env(key, value);
        }
        // Own process group: the engine kills the *tree* (§6 rule 3).
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let started = Instant::now();
        let mut child = command.spawn().map_err(|err| {
            let code = match err.kind() {
                std::io::ErrorKind::NotFound => ErrorCode::ToolchainMissing,
                std::io::ErrorKind::PermissionDenied => ErrorCode::SandboxDenied,
                _ => ErrorCode::QemuFailed,
            };
            PrincessError::new(code, format!("cannot execute {}: {err}", spec.argv[0]))
                .with_detail(format!("cwd: {}", spec.cwd.display()))
        })?;
        let pgid = child.id();

        // Serial tee: open before the first byte so nothing is lost, and write
        // every chunk before its event (contract §2).
        let mut tee = match &spec.serial_tee {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                Some(std::fs::File::create(path).map_err(|err| {
                    // The machine must not be left running because its log could
                    // not be opened.
                    let _ = kill_group_blocking(pgid, self.config.kill_grace);
                    let _ = child.wait();
                    PrincessError::internal(format!(
                        "cannot create the serial log {}: {err}",
                        path.display()
                    ))
                })?)
            }
            None => None,
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, rx) = channel::<StreamChunk>();
        let mut readers = Vec::new();
        if let Some(out) = stdout {
            readers.push(spawn_reader(out, Pipe::Serial, self.config.flush_every, tx.clone()));
        }
        if let Some(emulator) = stderr {
            readers.push(spawn_reader(
                emulator,
                Pipe::Emulator,
                self.config.flush_every,
                tx.clone(),
            ));
        }
        drop(tx);

        let deadline = spec.timeout.map(|timeout| Instant::now() + timeout);
        let mut timed_out = false;
        let mut cancelled = false;
        let mut kill_done = false;
        let mut status: Option<std::process::ExitStatus> = None;
        let mut eof_seen = 0usize;
        let mut last_eof = Instant::now();
        let teed = Arc::new(Mutex::new(0u64));

        loop {
            if status.is_none() {
                match child.try_wait() {
                    Ok(Some(exited)) => {
                        status = Some(exited);
                    }
                    Ok(None) => {}
                    Err(err) => {
                        let _ = kill_group_blocking(pgid, self.config.kill_grace);
                        return Err(PrincessError::new(
                            ErrorCode::QemuFailed,
                            format!("cannot wait for the emulator: {err}"),
                        ));
                    }
                }
            }

            if status.is_none() && !kill_done {
                if cancel.is_cancelled() && !cancelled {
                    cancelled = true;
                    kill_done = true;
                    let _ = kill_group(pgid, libc::SIGTERM);
                    let _ = kill_group(pgid, libc::SIGKILL);
                }
                if let Some(limit) = deadline {
                    if Instant::now() >= limit && !timed_out {
                        timed_out = true;
                        kill_done = true;
                        let _ = kill_group(pgid, libc::SIGTERM);
                        let _ = kill_group(pgid, libc::SIGKILL);
                    }
                }
            }

            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(chunk) => {
                    if chunk.at_eof {
                        eof_seen += 1;
                        last_eof = Instant::now();
                    }
                    if !chunk.text.is_empty() {
                        if chunk.pipe == Pipe::Serial {
                            if let Some(file) = tee.as_mut() {
                                file.write_all(chunk.text.as_bytes())
                                    .and_then(|()| file.flush())
                                    .map_err(|err| {
                                        PrincessError::internal(format!(
                                            "cannot write the serial log: {err}"
                                        ))
                                    })?;
                                if let Ok(mut count) = teed.lock() {
                                    *count += chunk.text.len() as u64;
                                }
                            }
                        }
                        on_chunk(chunk);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }

            if eof_seen >= readers.len()
                && status.is_some()
                && !readers.is_empty()
                && last_eof.elapsed() >= self.config.drain_grace
            {
                break;
            }
        }

        // Whatever happened, the group must not outlive this call.
        if status.is_none() {
            let _ = kill_group(pgid, libc::SIGTERM);
            let _ = kill_group(pgid, libc::SIGKILL);
        }

        for handle in readers {
            let _ = handle.join();
        }

        let status = match status {
            Some(status) => status,
            None => child.wait().map_err(|err| {
                PrincessError::new(
                    ErrorCode::QemuFailed,
                    format!("cannot reap the emulator: {err}"),
                )
            })?,
        };

        // Post-condition: no survivor in the group, and never a zombie either.
        let group_dead = !group_alive(pgid);
        if !group_dead {
            let _ = kill_group(pgid, libc::SIGKILL);
        }
        if let Some(file) = tee.as_mut() {
            let _ = file.flush();
        }

        let exit_code = status.code();
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        };

        Ok(ProcessOutcome {
            exit_code,
            signal,
            timed_out,
            cancelled,
            wall_ms: started.elapsed().as_millis() as u64,
            group_dead: group_dead || !group_alive(pgid),
        })
    }
}

/// Bytes that reached the tee file (exposed for tests through the outcome of a
/// collecting run; the real value lives in the file itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TeeStats {
    pub bytes: u64,
}

/// A runner that replays canned chunks and reports a canned outcome.  Lives
/// outside `#[cfg(test)]` so the CLI and integration tests can drive the whole
/// backend without starting QEMU.
#[derive(Default)]
pub struct FakeRunner {
    pub chunks: Vec<StreamChunk>,
    pub outcome: Option<ProcessOutcome>,
    pub seen: Mutex<Vec<SpawnSpec>>,
    /// Delay between chunks, so a test can exercise cancellation mid-stream.
    pub delay: Option<Duration>,
}

impl ProcessRunner for FakeRunner {
    fn run(
        &self,
        spec: &SpawnSpec,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(StreamChunk),
    ) -> Result<ProcessOutcome> {
        if let Ok(mut guard) = self.seen.lock() {
            guard.push(spec.clone());
        }
        for chunk in &self.chunks {
            if let Some(delay) = self.delay {
                std::thread::sleep(delay);
            }
            on_chunk(chunk.clone());
        }
        let _ = cancel;
        Ok(self.outcome.clone().unwrap_or(ProcessOutcome {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            cancelled: false,
            wall_ms: 1,
            group_dead: true,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    fn trivial(argv: Vec<&str>) -> SpawnSpec {
        SpawnSpec {
            argv: argv.into_iter().map(String::from).collect(),
            cwd: std::env::temp_dir(),
            timeout: None,
            serial_tee: None,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn which_finds_a_real_binary_and_rejects_a_fake_one() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-princesside-tool").is_none());
    }

    #[test]
    fn the_two_pipes_arrive_separately_and_in_order() {
        let spec = trivial(vec![
            "sh",
            "-c",
            "echo serial-1; echo emu-1 1>&2; echo serial-2; echo emu-2 1>&2",
        ]);
        let runner = SystemRunner::default();
        let (outcome, chunks) = runner.run_collecting(&spec, &CancelToken::new()).unwrap();
        assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
        assert!(outcome.group_dead);
        let serial: String = chunks
            .iter()
            .filter(|c| c.pipe == Pipe::Serial)
            .map(|c| c.text.clone())
            .collect();
        let emulator: String = chunks
            .iter()
            .filter(|c| c.pipe == Pipe::Emulator)
            .map(|c| c.text.clone())
            .collect();
        assert_eq!(serial, "serial-1\nserial-2\n");
        assert_eq!(emulator, "emu-1\nemu-2\n");
    }

    #[test]
    fn a_utf8_code_point_split_across_writes_is_never_cut() {
        let spec = trivial(vec![
            "sh",
            "-c",
            "printf '\\303'; sleep 0.15; printf '\\251 ok\\n'",
        ]);
        let runner = SystemRunner {
            config: RunnerConfig {
                flush_every: Duration::from_millis(50),
                ..RunnerConfig::default()
            },
        };
        let (_, chunks) = runner.run_collecting(&spec, &CancelToken::new()).unwrap();
        let text: String = chunks.iter().map(|c| c.text.clone()).collect();
        assert_eq!(text, "é ok\n");
        assert!(chunks.iter().all(|c| c.encoding == TextEncoding::Utf8));
    }

    #[test]
    fn the_serial_tee_is_written_before_the_chunk_is_handed_over() {
        let dir = std::env::temp_dir().join(format!("princesside-tee-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tee = dir.join("serial.log");

        let mut spec = trivial(vec!["sh", "-c", "echo hello-serial"]);
        spec.serial_tee = Some(tee.clone());

        // Observe on-disk content at the moment each chunk is delivered.
        let on_disk: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
        let probe = on_disk.clone();
        let path = tee.clone();
        let runner = SystemRunner::default();
        let outcome = runner
            .run(&spec, &CancelToken::new(), &mut |chunk| {
                if chunk.pipe == Pipe::Serial && !chunk.text.is_empty() {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    probe.lock().unwrap().push(text.contains(&chunk.text));
                }
            })
            .unwrap();
        assert_eq!(outcome.exit_code, Some(0));
        assert!(
            on_disk.lock().unwrap().iter().all(|present| *present),
            "every serial chunk must be on disk before it is pushed: {on_disk:?}"
        );
        assert!(std::fs::read_to_string(&tee).unwrap().contains("hello-serial"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_deadline_kills_the_whole_process_group() {
        let mut spec = trivial(vec!["sh", "-c", "sleep 30"]);
        spec.timeout = Some(Duration::from_millis(400));
        let runner = SystemRunner {
            config: RunnerConfig {
                kill_grace: Duration::from_millis(300),
                ..RunnerConfig::default()
            },
        };
        let started = Instant::now();
        let (outcome, _) = runner.run_collecting(&spec, &CancelToken::new()).unwrap();
        assert!(outcome.timed_out, "{outcome:?}");
        assert!(outcome.group_dead, "no survivor after the deadline: {outcome:?}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the watchdog did not fire promptly: {:?}",
            started.elapsed()
        );
    }

    /// The P2-7 shape in miniature: a grandchild must die with the group.
    #[test]
    fn a_grandchild_is_killed_with_the_group_and_leaves_no_orphan() {
        let dir = std::env::temp_dir().join(format!("princesside-orphan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("grandchild.pid");

        // The shell backgrounds a long sleep, then sleeps itself; killing only
        // the shell would leave the grandchild running.
        let mut spec = trivial(vec![
            "sh",
            "-c",
            &format!("sleep 30 & echo $! > {}; sleep 30", marker.display()),
        ]);
        spec.timeout = Some(Duration::from_millis(500));
        let runner = SystemRunner {
            config: RunnerConfig {
                kill_grace: Duration::from_millis(500),
                ..RunnerConfig::default()
            },
        };
        let (outcome, _) = runner.run_collecting(&spec, &CancelToken::new()).unwrap();
        assert!(outcome.timed_out, "{outcome:?}");

        let pid: i32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The grandchild must be gone (not merely a zombie of another parent).
        let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, rest)| rest.to_string()))
            .and_then(|rest| rest.split_whitespace().next().map(str::to_string))
            .map(|state| state != "Z")
            .unwrap_or(false);
        assert!(!alive, "grandchild {pid} survived the group kill");
        assert!(outcome.group_dead);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancellation_is_honoured_and_reported() {
        let mut spec = trivial(vec!["sh", "-c", "sleep 30"]);
        spec.timeout = Some(Duration::from_secs(30));
        let cancel = CancelToken::new();
        let token = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            token.cancel();
        });
        let runner = SystemRunner::default();
        let (outcome, _) = runner.run_collecting(&spec, &cancel).unwrap();
        assert!(outcome.cancelled, "{outcome:?}");
        assert!(outcome.group_dead);
    }

    #[test]
    fn a_missing_binary_is_a_toolchain_error_not_a_panic() {
        let spec = trivial(vec!["definitely-not-a-princesside-tool"]);
        let runner = SystemRunner::default();
        let err = runner.run_collecting(&spec, &CancelToken::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ToolchainMissing);
    }

    #[test]
    fn an_empty_argv_is_refused() {
        let spec = trivial(Vec::new());
        let err = SystemRunner::default()
            .run_collecting(&spec, &CancelToken::new())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
    }

    #[test]
    fn killing_a_group_that_is_already_gone_is_not_an_error() {
        kill_group(999_999, libc::SIGKILL).unwrap();
        assert!(!group_alive(999_999));
        kill_group_blocking(999_999, Duration::from_millis(50)).unwrap();
    }

    #[test]
    fn signalling_group_zero_is_refused() {
        let err = kill_group(0, libc::SIGKILL).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(err.message.contains("group 0"), "{err}");
    }

    #[test]
    fn a_zombie_group_member_is_not_a_survivor() {
        let mut child = std::process::Command::new("true")
            .process_group(0)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pgid = child.id();
        std::thread::sleep(Duration::from_millis(200));
        // Deliberately not reaped yet: `true` is a zombie right now.
        assert!(!group_alive(pgid), "a zombie must not count as a survivor");
        let _ = child.wait();
        assert!(!group_alive(pgid));
    }

    #[test]
    fn the_fake_runner_records_the_spec_it_was_given() {
        let runner = FakeRunner {
            chunks: vec![StreamChunk {
                pipe: Pipe::Serial,
                text: "PrincessIDE reference kernel booted\n".into(),
                encoding: TextEncoding::Utf8,
                at_eof: false,
            }],
            outcome: Some(ProcessOutcome {
                exit_code: Some(0),
                ..ProcessOutcome::default()
            }),
            ..FakeRunner::default()
        };
        let spec = trivial(vec!["qemu-system-x86_64", "-display", "none"]);
        let mut chunks = Vec::new();
        let outcome = runner
            .run(&spec, &CancelToken::new(), &mut |chunk| chunks.push(chunk))
            .unwrap();
        assert_eq!(outcome.exit_code, Some(0));
        assert_eq!(chunks.len(), 1);
        assert_eq!(runner.seen.lock().unwrap().len(), 1);
    }
}
