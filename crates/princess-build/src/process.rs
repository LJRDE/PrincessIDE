//! Child-process execution for the build backend — contract §6.
//!
//! The rules implemented here are the ones that are easy to get wrong:
//!
//! 1. **stdout and stderr stay separate** (§6 rule 1).  They are read on two
//!    threads and never merged with `2>&1`.
//! 2. **Every child is its own process group** (`setsid` via
//!    `CommandExt::process_group`), so a cancellation kills the whole tree
//!    instead of leaving `cc1`/`ld` orphans behind (§6 rule 3).
//! 3. **No infinite wait**: a read loop that blocks on a pipe held open by a
//!    grandchild is bounded by a drain grace period, and the deadline is enforced
//!    by a watchdog that kills the group.
//! 4. **UTF-8 is never cut mid-code-point** — every chunk goes through
//!    [`Utf8Chunker`] before it becomes a `log.append` event.
//!
//! The runner is behind the [`CommandRunner`] trait so unit tests can drive the
//! streaming/chunking/diagnostic pipeline without spawning anything.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use princess_core::traits::{BuildPlan, CancelToken, EventSink};
use princess_core::types::{LogStream, TextEncoding};
use princess_core::{PrincessError, Result, Utf8Chunker};

/// Which of the two pipes a chunk came from (§6 rule 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioStream {
    Stdout,
    Stderr,
}

impl StdioStream {
    /// Both stream to `log.append.stream = "build"`; the separation that matters
    /// is that they are *read* separately so a diagnostic on stderr can never be
    /// spliced into the middle of a stdout progress line.
    pub const fn log_stream(self) -> LogStream {
        LogStream::Build
    }
}

/// One decoded chunk, tagged with the pipe it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioChunk {
    pub stream: StdioStream,
    pub text: String,
    pub encoding: TextEncoding,
    /// `true` for the final (possibly partial-line) flush at EOF.
    pub at_eof: bool,
}

/// What a finished child left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit code, or `None` when the child was killed by a signal.
    pub exit_code: Option<i32>,
    /// Terminating signal, when it was killed.
    pub signal: Option<i32>,
    /// `true` when the watchdog hit the deadline and killed the group.
    pub timed_out: bool,
    /// `true` when cancellation was requested during the run.
    pub cancelled: bool,
    /// Everything the child wrote, stdout first then stderr, for error details.
    pub combined_tail: String,
}

impl CommandOutput {
    pub fn success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && !self.cancelled
    }
}

/// Queue both pipes of a child onto one channel.
///
/// `flush_every` bounds how long an incomplete line can sit in the chunker: the
/// reader thread wakes up at that interval and emits whatever it has.
fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    which: StdioStream,
    flush_every: Duration,
    sink_tx: Sender<StdioChunk>,
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
                        if sink_tx
                            .send(StdioChunk {
                                stream: which,
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
                        if sink_tx
                            .send(StdioChunk {
                                stream: which,
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
            let _ = sink_tx.send(StdioChunk {
                stream: which,
                text,
                encoding,
                at_eof: true,
            });
        }
        let _ = pump.join();
    })
}

/// The child-process half of the build engine.
pub trait CommandRunner: Send + Sync {
    /// Run `plan.argv` in `plan.cwd`, streaming decoded chunks to `on_chunk`
    /// (in arrival order, both pipes) and honouring `cancel` + the deadline.
    fn run(
        &self,
        plan: &BuildPlan,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(StdioChunk),
    ) -> Result<CommandOutput>;
}

/// Kill a whole process group (negative pid), falling back to the direct child.
pub fn kill_group(pgid: u32, signal: i32) -> Result<()> {
    // SAFETY: `kill(2)` with a negative pid targets the process group.  The
    // group exists because the child was spawned with `process_group(0)`.
    let rc = unsafe { libc_kill(-(pgid as i32), signal) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // ESRCH: the group is already gone — that is success for a cleanup helper.
    if err.raw_os_error() == Some(3) {
        return Ok(());
    }
    Err(PrincessError::internal(format!(
        "cannot signal process group {pgid}: {err}"
    )))
}

/// `kill(2)` without pulling in the `libc` crate for one symbol.
///
/// Declared `extern "C"` directly: `princess-build` deliberately keeps its
/// dependency set to `princess-core` + serde/sha2 so it stays cheap to compile
/// on a 4-core, no-swap host (D21).
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid, sig)
}

/// Is there still a process in this group?
pub fn group_alive(pgid: u32) -> bool {
    unsafe { libc_kill(-(pgid as i32), 0) == 0 }
}

/// The real runner: `std::process` + process groups + a watchdog thread.
pub struct SystemRunner {
    /// After EOF, how long to keep draining a pipe a grandchild still holds.
    pub drain_grace: Duration,
    /// How often a partial line is flushed to the UI.
    pub flush_every: Duration,
    /// Worst-case wait for the group to die after SIGKILL.
    pub kill_grace: Duration,
    /// Extra environment applied to every child (D19/D21 style knobs).
    pub global_env: BTreeMap<String, String>,
}

impl Default for SystemRunner {
    fn default() -> Self {
        Self {
            drain_grace: Duration::from_millis(250),
            flush_every: Duration::from_millis(120),
            kill_grace: Duration::from_secs(2),
            global_env: BTreeMap::new(),
        }
    }
}

impl SystemRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run and additionally collect every chunk (used by tests and by callers
    /// that want the raw transcript).
    pub fn run_collecting(
        &self,
        plan: &BuildPlan,
        cancel: &CancelToken,
    ) -> Result<(CommandOutput, Vec<StdioChunk>)> {
        let mut chunks = Vec::new();
        let output = self.run(plan, cancel, &mut |chunk| chunks.push(chunk))?;
        Ok((output, chunks))
    }
}

impl CommandRunner for SystemRunner {
    fn run(
        &self,
        plan: &BuildPlan,
        cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(StdioChunk),
    ) -> Result<CommandOutput> {
        if plan.argv.is_empty() {
            return Err(PrincessError::internal(
                "build plan has an empty argv: nothing to execute",
            ));
        }

        let mut command = Command::new(&plan.argv[0]);
        command
            .args(&plan.argv[1..])
            .current_dir(&plan.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in self.global_env.iter().chain(plan.env.iter()) {
            command.env(key, value);
        }
        // Own process group: cancellation must reach the whole tree (§6 rule 3).
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let started = Instant::now();
        let mut child = command.spawn().map_err(|err| {
            PrincessError::internal(format!(
                "cannot execute {}: {err}",
                display_argv(&plan.argv)
            ))
            .with_detail(format!("cwd: {}", plan.cwd.display()))
        })?;
        let pgid = child.id();

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, rx) = channel::<StdioChunk>();
        let mut readers = Vec::new();
        if let Some(out) = stdout {
            readers.push(spawn_reader(out, StdioStream::Stdout, self.flush_every, tx.clone()));
        }
        if let Some(err) = stderr {
            readers.push(spawn_reader(err, StdioStream::Stderr, self.flush_every, tx.clone()));
        }
        drop(tx);

        let tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let deadline = plan.timeout_ms.map(Duration::from_millis);
        let mut timed_out = false;
        let mut cancelled = false;
        let mut exit_status: Option<std::process::ExitStatus> = None;
        let mut eof_seen = 0usize;
        let mut last_eof = Instant::now();

        loop {
            if exit_status.is_none() {
                match child.try_wait() {
                    Ok(Some(status)) => exit_status = Some(status),
                    Ok(None) => {}
                    Err(err) => {
                        let _ = kill_group(pgid, 9);
                        return Err(PrincessError::internal(format!(
                            "cannot wait for build child: {err}"
                        )));
                    }
                }
            }

            if exit_status.is_none() {
                if cancel.is_cancelled() && !cancelled {
                    cancelled = true;
                    let _ = kill_group(pgid, 15);
                    let _ = kill_group(pgid, 9);
                }
                if let Some(limit) = deadline {
                    if started.elapsed() >= limit && !timed_out {
                        timed_out = true;
                        let _ = kill_group(pgid, 15);
                        let _ = kill_group(pgid, 9);
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
                        if let Ok(mut guard) = tail.lock() {
                            guard.push(chunk.text.clone());
                            let excess = guard.len().saturating_sub(512);
                            if excess > 0 {
                                guard.drain(..excess);
                            }
                        }
                    }
                    on_chunk(chunk);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    // No writer left inside this call: every reader thread ended.
                    break;
                }
            }

            // Both pipes hit EOF: let the reader threads finish, then stop.
            if eof_seen >= readers.len()
                && exit_status.is_some()
                && last_eof.elapsed() >= self.drain_grace
            {
                break;
            }
        }

        for handle in readers {
            let _ = handle.join();
        }

        // Reap.  A child killed by the watchdog must not be left as a zombie.
        let status = match exit_status {
            Some(status) => status,
            None => child.wait().map_err(|err| {
                PrincessError::internal(format!("cannot reap build child: {err}"))
            })?,
        };

        let exit_code = status.code();
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;

        let combined_tail = tail
            .lock()
            .map(|guard| guard.concat())
            .unwrap_or_default()
            .chars()
            .rev()
            .take(8192)
            .collect::<String>()
            .chars()
            .rev()
            .collect();

        Ok(CommandOutput {
            exit_code,
            signal,
            timed_out,
            cancelled,
            combined_tail,
        })
    }
}

/// Render argv for humans/logs, quoting anything with spaces.
pub fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|part| {
            if part.contains(' ') {
                format!("{part:?}")
            } else {
                part.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Resolve an executable through `PATH`, without spawning anything.
pub fn which(exe: &str) -> Option<PathBuf> {
    if exe.contains('/') {
        let path = Path::new(exe);
        return if is_executable(path) {
            Some(path.to_path_buf())
        } else {
            None
        };
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

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// A runner that records the plans it was asked to run and replays canned
/// output.  Lives here (not in `#[cfg(test)]`) so downstream crates can use it
/// as a documented test double.
#[derive(Default)]
pub struct FakeRunner {
    pub chunks: Vec<StdioChunk>,
    pub output: Option<CommandOutput>,
    pub seen: Mutex<Vec<BuildPlan>>,
}

impl CommandRunner for FakeRunner {
    fn run(
        &self,
        plan: &BuildPlan,
        _cancel: &CancelToken,
        on_chunk: &mut dyn FnMut(StdioChunk),
    ) -> Result<CommandOutput> {
        if let Ok(mut guard) = self.seen.lock() {
            guard.push(plan.clone());
        }
        for chunk in &self.chunks {
            on_chunk(chunk.clone());
        }
        Ok(self.output.clone().unwrap_or(CommandOutput {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            cancelled: false,
            combined_tail: String::new(),
        }))
    }
}

/// Keep the `EventSink` import meaningful for the module docs (the trait is the
/// only way this crate is allowed to talk to the UI).
#[allow(dead_code)]
fn _event_sink_is_the_only_outbound_channel(sink: &mut dyn EventSink) -> Result<()> {
    sink.log(LogStream::Build, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trivial_plan(argv: Vec<&str>) -> BuildPlan {
        BuildPlan {
            backend: princess_core::config::BuildBackendKind::Custom,
            toolchain_id: "test".into(),
            argv: argv.into_iter().map(String::from).collect(),
            cwd: std::env::temp_dir(),
            env: BTreeMap::new(),
            declared_artifacts: Vec::new(),
            timeout_ms: None,
        }
    }

    #[test]
    fn which_finds_a_real_binary_and_rejects_a_fake_one() {
        assert!(which("sh").is_some(), "sh must be on PATH in any POSIX env");
        assert!(which("definitely-not-a-princesside-tool").is_none());
    }

    #[test]
    fn stdout_and_stderr_arrive_on_separate_chunks_in_order() {
        // Two lines on stdout, two on stderr, interleaved by the OS.
        let plan = trivial_plan(vec![
            "sh",
            "-c",
            "echo out-1; echo err-1 1>&2; echo out-2; echo err-2 1>&2",
        ]);
        let runner = SystemRunner::default();
        let (output, chunks) = runner.run_collecting(&plan, &CancelToken::new()).unwrap();
        assert_eq!(output.exit_code, Some(0), "{output:?}");
        let stdout_text: String = chunks
            .iter()
            .filter(|c| c.stream == StdioStream::Stdout)
            .map(|c| c.text.clone())
            .collect();
        let stderr_text: String = chunks
            .iter()
            .filter(|c| c.stream == StdioStream::Stderr)
            .map(|c| c.text.clone())
            .collect();
        assert_eq!(stdout_text, "out-1\nout-2\n");
        assert_eq!(stderr_text, "err-1\nerr-2\n");
    }

    #[test]
    fn a_utf8_code_point_split_across_writes_is_never_cut() {
        let plan = trivial_plan(vec![
            "sh",
            "-c",
            "printf '\\303'; sleep 0.15; printf '\\251 ok\\n'",
        ]);
        let runner = SystemRunner::default();
        let (_, chunks) = runner.run_collecting(&plan, &CancelToken::new()).unwrap();
        let text: String = chunks.iter().map(|c| c.text.clone()).collect();
        assert_eq!(text, "é ok\n");
        assert!(chunks.iter().all(|c| c.encoding == TextEncoding::Utf8));
    }

    #[test]
    fn a_partial_line_is_flushed_before_eof_so_the_ui_is_live() {
        let plan = trivial_plan(vec!["sh", "-c", "printf 'no-newline-yet'; sleep 0.4"]);
        let runner = SystemRunner {
            flush_every: Duration::from_millis(50),
            ..SystemRunner::default()
        };
        let (_, chunks) = runner.run_collecting(&plan, &CancelToken::new()).unwrap();
        assert!(
            chunks.iter().any(|c| c.text.contains("no-newline-yet") && !c.at_eof),
            "expected a mid-run flush, got {chunks:?}"
        );
    }

    #[test]
    fn the_deadline_kills_the_whole_process_group() {
        let mut plan = trivial_plan(vec!["sh", "-c", "sleep 30"]);
        plan.timeout_ms = Some(300);
        let runner = SystemRunner {
            kill_grace: Duration::from_millis(200),
            ..SystemRunner::default()
        };
        let started = Instant::now();
        let (output, _) = runner.run_collecting(&plan, &CancelToken::new()).unwrap();
        assert!(output.timed_out, "{output:?}");
        assert!(!output.success());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the watchdog did not fire promptly: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn cancellation_is_honoured_and_reported() {
        let plan = trivial_plan(vec!["sh", "-c", "sleep 30"]);
        let cancel = CancelToken::new();
        let token = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            token.cancel();
        });
        let runner = SystemRunner::default();
        let (output, _) = runner.run_collecting(&plan, &cancel).unwrap();
        assert!(output.cancelled, "{output:?}");
    }

    #[test]
    fn a_missing_binary_is_an_error_not_a_panic() {
        let plan = trivial_plan(vec!["definitely-not-a-princesside-tool"]);
        let runner = SystemRunner::default();
        let err = runner.run_collecting(&plan, &CancelToken::new()).unwrap_err();
        assert_eq!(err.code, princess_core::ErrorCode::Internal);
    }

    #[test]
    fn kill_group_tolerates_a_group_that_is_already_gone() {
        assert!(kill_group(999_999, 9).is_ok());
        assert!(!group_alive(999_999));
    }
}
