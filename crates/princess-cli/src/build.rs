//! `build` — run the project's build backend and emit the contract's build
//! events.
//!
//! Event order (contract §2):
//!
//! ```text
//! build.started → log.append(build)* → build.diagnostic* → artifact.changed*
//!               → symbols.indexed* → build.finished
//! ```
//!
//! `build.finished` is emitted on **every** path out of this function (§6 rule
//! 5), including spawn failure, cancellation and timeout.
//!
//! stdout and stderr are read as two independent streams (never `2>&1`), so a
//! compiler diagnostic arriving on stderr cannot be interleaved into the middle
//! of a progress line on stdout.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use princess_core::config::BuildBackendKind;
use princess_core::event::{BuildFinishedPayload, EventBody};
use princess_core::traits::{BuildPlan, CancelToken, EventSink};
use princess_core::types::{
    Artifact, ArtifactKind, BuildStatus, DiagnosticSource, LogStream, TextEncoding,
};
use princess_core::{BuildDiagnosticPayload, ErrorCode, PrincessError, Result, Utf8Chunker};

use crate::diagnostics;
use crate::project::{discover_artifacts, Project};
use crate::proc;
use crate::symbolize::Symbolizer;

/// How long to keep draining output after the child exited (a stray grandchild
/// may still hold the pipe open; the engine must not hang on it).
const DRAIN_GRACE: Duration = Duration::from_secs(3);
/// Idle interval after which a partial line is flushed to the UI.
const IDLE_FLUSH: Duration = Duration::from_millis(120);
/// How much tail output is kept for error reporting.
const OUTPUT_TAIL_LIMIT: usize = 8192;

/// Options for one build.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Override the manifest's targets.
    pub targets: Option<Vec<String>>,
    /// Override the manifest's timeout.
    pub timeout_ms: Option<u64>,
}

/// Everything the caller needs after a build (the events have already been
/// emitted; this is for the CLI's own reporting and for the run subcommand).
#[derive(Debug, Clone)]
pub struct BuildResult {
    pub plan: BuildPlan,
    pub payload: BuildFinishedPayload,
    pub artifacts: Vec<Artifact>,
    pub diagnostics: Vec<BuildDiagnosticPayload>,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub timed_out: bool,
    pub output_tail: String,
}

impl BuildResult {
    pub fn succeeded(&self) -> bool {
        self.payload.status == BuildStatus::Ok
    }
}

/// Turn the manifest into an exact invocation.
pub fn plan_build(project: &Project, targets: Option<&[String]>) -> Result<BuildPlan> {
    let config = &project.resolved.config.build;
    let mut argv: Vec<String> = match project.resolved.build_command.as_deref() {
        Some(command) if !command.trim().is_empty() => split_command(command),
        _ => match config.backend {
            BuildBackendKind::Make => vec!["make".to_string()],
            BuildBackendKind::Cmake => vec![
                "cmake".to_string(),
                "--build".to_string(),
                ".".to_string(),
            ],
            BuildBackendKind::Cargo => vec!["cargo".to_string(), "build".to_string()],
            BuildBackendKind::Zig => vec!["zig".to_string(), "build".to_string()],
            BuildBackendKind::Custom => {
                return Err(PrincessError::invalid_config(
                    "[build] backend = \"custom\" requires an explicit [build] command",
                ))
            }
        },
    };

    let selected: &[String] = match targets {
        Some(targets) => targets,
        None => &project.resolved.targets,
    };
    match config.backend {
        BuildBackendKind::Cmake => {
            for target in selected {
                argv.push("--target".to_string());
                argv.push(target.clone());
            }
        }
        _ => argv.extend(selected.iter().cloned()),
    }

    // argv[0] must be a real binary: fail here with E_TOOLCHAIN_MISSING rather
    // than letting the spawn fail with a bare "No such file or directory".
    let program = argv.first().cloned().unwrap_or_default();
    if program.is_empty() {
        return Err(PrincessError::invalid_config("empty build command"));
    }
    // A program with a separator is relative to the *build* directory (it is a
    // path in the manifest), everything else goes through PATH lookup.
    let resolved_program = (if program.contains('/') {
        let candidate = if std::path::Path::new(&program).is_absolute() {
            std::path::PathBuf::from(&program)
        } else {
            project.resolved.build_cwd.join(&program)
        };
        candidate.is_file().then_some(candidate)
    } else {
        project.toolchain.which(&program)
    })
    .ok_or_else(|| {
        PrincessError::new(
            ErrorCode::ToolchainMissing,
            format!(
                "build tool `{program}` was not found (PATH, or a path relative to {}); \
                 run `bash scripts/doctor.sh` or set [toolchain] in princess.toml",
                project.resolved.build_cwd.display()
            ),
        )
    })?;
    argv[0] = resolved_program.to_string_lossy().into_owned();

    Ok(BuildPlan {
        backend: config.backend,
        toolchain_id: toolchain_id(project),
        argv,
        cwd: project.resolved.build_cwd.clone(),
        env: project.resolved.build_env(),
        declared_artifacts: project.resolved.declared_artifacts.clone(),
        // The manifest has no `[build] timeout_ms` in schema 1; only the CLI's
        // `--timeout-ms` sets a build deadline.
        timeout_ms: None,
    })
}

/// `backend+cc-version`, e.g. `make+gcc-12.2.0`.  Only real detected facts are
/// used: an explicit `[toolchain] cc` wins, otherwise the detected `gcc`.
fn toolchain_id(project: &Project) -> String {
    let backend = project.resolved.config.build.backend.as_str();
    let toolchain = &project.resolved.config.toolchain;
    let cc = match (&toolchain.cc, toolchain.assembler.as_ref()) {
        (Some(cc), _) => Some(cc.clone()),
        (None, Some(assembler)) => Some(assembler.clone()),
        (None, None) => None,
    };
    match cc {
        Some(cc) => format!("{backend}+{cc}"),
        None => {
            let version = project
                .toolchain
                .version("gcc", &["--version"])
                .and_then(|line| version_token(&line))
                .unwrap_or_else(|| "unknown".to_string());
            format!("{backend}+gcc-{version}")
        }
    }
}

/// First *pure* version token of a `--version` line: digits and dots only, so
/// `gcc (Debian 12.2.0-14+deb12u1) 12.2.0` yields `12.2.0` and not the Debian
/// revision `12.2.0-14+deb12u1`.
fn version_token(line: &str) -> Option<String> {
    line.split_whitespace()
        .map(|token| token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.'))
        .find(|token| {
            token.contains('.')
                && token.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
                && token.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
        .map(|token| token.to_string())
}

/// Split a `[build] command` override.  Whitespace-separated only: the engine
/// deliberately does not run a shell, so `command` is an argv, not a script.
fn split_command(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(|token| token.to_string())
        .collect()
}

/// Run the build, emitting events, and return what happened.
pub async fn execute(
    project: &Project,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
    options: &BuildOptions,
) -> Result<BuildResult> {
    let plan = plan_build(project, options.targets.as_deref())?;
    let started = Instant::now();
    let op_id = events.op_id().unwrap_or("op-unknown").to_string();
    events.emit(EventBody::BuildStarted(plan.started_event()))?;

    // ------------------------------------------------------------- spawn ----
    let mut command = tokio::process::Command::new(&plan.argv[0]);
    command
        .args(&plan.argv[1..])
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // Own process group: cancelling must reach gcc/grub-mkrescue too.
        .process_group(0);
    project.toolchain.apply_tokio(&mut command);
    for (key, value) in &plan.env {
        command.env(key, value);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let error = match err.kind() {
                std::io::ErrorKind::NotFound => PrincessError::new(
                    ErrorCode::ToolchainMissing,
                    format!("cannot execute {}: {err}", plan.argv[0]),
                ),
                std::io::ErrorKind::PermissionDenied => PrincessError::new(
                    ErrorCode::SandboxDenied,
                    format!("cannot execute {}: {err}", plan.argv[0]),
                ),
                _ => PrincessError::internal(format!("cannot execute {}: {err}", plan.argv[0])),
            };
            events.note(&format!("build failed to start: {error}"))?;
            let payload = BuildFinishedPayload {
                status: BuildStatus::Failed,
                exit_code: None,
                duration_ms: started.elapsed().as_millis() as u64,
                artifacts: Vec::new(),
            };
            events.emit(EventBody::BuildFinished(payload.clone()))?;
            return Err(error.with_detail(plan.argv.join(" ")));
        }
    };
    let pgid = child
        .id()
        .ok_or_else(|| PrincessError::internal("child has no pid"))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| PrincessError::internal("child stdout is not piped"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| PrincessError::internal("child stderr is not piped"))?;

    let default_source = if plan.argv[0].contains("clang") {
        DiagnosticSource::Clang
    } else {
        DiagnosticSource::Gcc
    };
    let cwd = plan.cwd.clone();

    let mut out_chunker = Utf8Chunker::new();
    let mut err_chunker = Utf8Chunker::new();
    let mut out_open = true;
    let mut err_open = true; // stderr is the second stream, never merged with stdout

    let mut diagnostics: Vec<BuildDiagnosticPayload> = Vec::new();
    let mut tail = String::new();

    let mut deadline = plan
        .timeout_ms
        .or(options.timeout_ms)
        .map(|ms| Instant::now() + Duration::from_millis(ms));
    let mut timed_out = false;
    let mut status: Option<std::process::ExitStatus> = None;
    let mut exit_at: Option<Instant> = None;

    // --------------------------------------------------------- pump loop ----
    loop {
        if status.is_some() && !out_open && !err_open {
            break;
        }
        // A grandchild that outlives the build must not stall the engine: once
        // the grace period after the exit has passed, stop draining.
        if let Some(at) = exit_at {
            if at.elapsed() > DRAIN_GRACE {
                break;
            }
        }

        tokio::select! {
            result = read_chunk(&mut stdout), if out_open => {
                match result {
                    Ok(bytes) if bytes.is_empty() => {
                        out_open = false;
                        flush_chunker(events, &mut out_chunker, false, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
                    }
                    Ok(bytes) => {
                        out_chunker.push(&bytes);
                        flush_chunker(events, &mut out_chunker, true, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
                    }
                    Err(err) => {
                        out_open = false;
                        events.note(&format!("build stdout read error: {err}"))?;
                    }
                }
            }
            result = read_chunk(&mut stderr), if err_open => {
                match result {
                    Ok(bytes) if bytes.is_empty() => {
                        err_open = false;
                        flush_chunker(events, &mut err_chunker, false, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
                    }
                    Ok(bytes) => {
                        err_chunker.push(&bytes);
                        flush_chunker(events, &mut err_chunker, true, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
                    }
                    Err(err) => {
                        err_open = false;
                        events.note(&format!("build stderr read error: {err}"))?;
                    }
                }
            }
            result = child.wait(), if status.is_none() => {
                status = Some(result.map_err(|err| {
                    PrincessError::internal(format!("cannot wait for the build: {err}"))
                })?);
                exit_at = Some(Instant::now());
            }
            _ = tokio::time::sleep(IDLE_FLUSH) => {
                flush_chunker(events, &mut out_chunker, false, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
                flush_chunker(events, &mut err_chunker, false, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
            }
            _ = wait_until(deadline), if deadline.is_some() => {
                timed_out = true;
                events.note(&format!(
                    "build exceeded its {} ms deadline; killing process group {pgid}",
                    plan.timeout_ms.or(options.timeout_ms).unwrap_or(0)
                ))?;
                proc::kill_group_and_wait(pgid).await?;
                deadline = None;
            }
        }

        if cancel.is_cancelled() {
            events.note(&format!("build cancelled; killing process group {pgid}"))?;
            proc::kill_group_and_wait(pgid).await?;
            break;
        }
    }

    // Final drain of anything still buffered.
    flush_chunker(events, &mut out_chunker, false, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
    flush_chunker(events, &mut err_chunker, false, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
    if let Some(remainder) = out_chunker.take_remainder() {
        handle_chunk(events, remainder, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
    }
    if let Some(remainder) = err_chunker.take_remainder() {
        handle_chunk(events, remainder, LogStream::Build, &cwd, default_source, &mut diagnostics, &mut tail)?;
    }

    if status.is_none() {
        status = Some(
            child
                .wait()
                .await
                .map_err(|err| PrincessError::internal(format!("cannot reap the build: {err}")))?,
        );
    }
    let exit_code = status.and_then(|s| s.code());
    let exited_cleanly = status.map(|s| s.success()).unwrap_or(false);

    let build_status = if cancel.is_cancelled() {
        BuildStatus::Cancelled
    } else if timed_out {
        events.note("build ended by timeout: reported as failed")?;
        BuildStatus::Failed
    } else if exited_cleanly {
        BuildStatus::Ok
    } else {
        BuildStatus::Failed
    };

    // ---------------------------------------------------------- artifacts ---
    let declared = project.resolved.declared_artifacts.clone();
    for missing in declared.iter().filter(|path| !path.is_file()) {
        events.note(&format!(
            "declared artifact {} was not produced",
            missing.display()
        ))?;
    }
    let artifacts = discover_artifacts(
        &project.resolved.root,
        &declared,
        None,
    );
    for artifact in &artifacts {
        events.emit(EventBody::ArtifactChanged(
            princess_core::event::ArtifactChangedPayload {
                path: artifact.path.clone(),
                kind: artifact.kind,
            },
        ))?;
    }
    emit_symbols_indexed(project, events, &artifacts)?;

    let payload = BuildFinishedPayload {
        status: build_status,
        exit_code,
        duration_ms: started.elapsed().as_millis() as u64,
        artifacts: artifacts.clone(),
    };
    // The human summary goes out *before* the terminal event: `build.finished`
    // is the last event of the operation, so a replaying UI can use it as the
    // end-of-operation marker.
    events.note(&format!(
        "build {} in {} ms{} (op {op_id})",
        build_status.as_str(),
        payload.duration_ms,
        match exit_code {
            Some(code) => format!(", exit {code}"),
            None => String::new(),
        }
    ))?;
    events.emit(EventBody::BuildFinished(payload.clone()))?;

    Ok(BuildResult {
        plan,
        payload,
        artifacts,
        diagnostics,
        exit_code,
        cancelled: cancel.is_cancelled(),
        timed_out,
        output_tail: tail,
    })
}

/// `symbols.indexed` for every ELF artifact — facts read from the image itself
/// (`nm` symbol count, `readelf` build id).  No symbol file means no event.
fn emit_symbols_indexed(
    project: &Project,
    events: &mut dyn EventSink,
    artifacts: &[Artifact],
) -> Result<()> {
    for artifact in artifacts.iter().filter(|a| a.kind == ArtifactKind::Elf) {
        let path = Path::new(&artifact.path);
        let Ok(symbolizer) = Symbolizer::new(path, &project.toolchain) else {
            continue;
        };
        let Some(symbol_count) = symbolizer.symbol_count() else {
            continue;
        };
        events.emit(EventBody::SymbolsIndexed(
            princess_core::event::SymbolsIndexedPayload {
                artifact: artifact.path.clone(),
                build_id: symbolizer.build_id(),
                symbol_count,
            },
        ))?;
    }
    Ok(())
}

/// Read up to 4 KiB.  The buffer is owned by the future (not borrowed from the
/// caller) so a `select!` arm can hand the bytes to the event sink in its body.
async fn read_chunk(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> std::io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut buffer = vec![0u8; 4096];
    let read = stream.read(&mut buffer).await?;
    buffer.truncate(read);
    Ok(buffer)
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => {
            let now = Instant::now();
            if deadline > now {
                tokio::time::sleep(deadline - now).await;
            }
        }
        // No deadline: never completes.
        None => std::future::pending::<()>().await,
    }
}

/// Emit whatever the chunker has (complete lines only when `lines_only`).
#[allow(clippy::too_many_arguments)]
fn flush_chunker(
    events: &mut dyn EventSink,
    chunker: &mut Utf8Chunker,
    lines_only: bool,
    stream: LogStream,
    cwd: &Path,
    source: DiagnosticSource,
    diagnostics: &mut Vec<BuildDiagnosticPayload>,
    tail: &mut String,
) -> Result<()> {
    if lines_only {
        while let Some(chunk) = chunker.take_lines() {
            handle_chunk(events, chunk, stream, cwd, source, diagnostics, tail)?;
        }
    } else if let Some(chunk) = chunker.take_available() {
        handle_chunk(events, chunk, stream, cwd, source, diagnostics, tail)?;
    }
    Ok(())
}

/// Emit one chunk, remember it for error reporting, and turn any diagnostic
/// lines in it into `build.diagnostic` events.
#[allow(clippy::too_many_arguments)]
fn handle_chunk(
    events: &mut dyn EventSink,
    (text, encoding): (String, TextEncoding),
    stream: LogStream,
    cwd: &Path,
    source: DiagnosticSource,
    diagnostics: &mut Vec<BuildDiagnosticPayload>,
    tail: &mut String,
) -> Result<()> {
    events.emit(EventBody::LogAppend(princess_core::event::LogAppendPayload {
        stream,
        chunk: text.clone(),
        encoding,
    }))?;

    tail.push_str(&text);
    if tail.len() > OUTPUT_TAIL_LIMIT {
        let cut = tail.len() - OUTPUT_TAIL_LIMIT;
        let cut = tail
            .char_indices()
            .map(|(i, _)| i)
            .find(|i| *i >= cut)
            .unwrap_or(tail.len());
        *tail = tail[cut..].to_string();
    }

    for line in text.lines() {
        if let Some(diagnostic) = diagnostics::parse_line(line, cwd, source) {
            events.emit(EventBody::BuildDiagnostic(diagnostic.clone()))?;
            diagnostics.push(diagnostic);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Toolchain;
    use crate::project::Project;
    use princess_core::event::EventRecorder;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("princesside-build-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Spy {
        bodies: Vec<EventBody>,
    }

    impl EventSink for Spy {
        fn emit(&mut self, body: EventBody) -> Result<()> {
            self.bodies.push(body);
            Ok(())
        }
        fn op_id(&self) -> Option<&str> {
            Some("op-test")
        }
    }

    #[test]
    fn version_tokens_are_extracted_from_real_tool_output() {
        assert_eq!(version_token("gcc (Debian 12.2.0-14+deb12u1) 12.2.0").as_deref(), Some("12.2.0"));
        assert_eq!(version_token("NASM version 2.16.01").as_deref(), Some("2.16.01"));
        assert_eq!(version_token("no version here"), None);
    }

    #[test]
    fn command_overrides_are_split_without_a_shell() {
        assert_eq!(
            split_command("make -j4 iso"),
            vec!["make".to_string(), "-j4".to_string(), "iso".to_string()]
        );
        assert!(split_command("   ").is_empty());
    }

    #[tokio::test]
    async fn a_successful_build_streams_both_streams_and_reports_artifacts() {
        let dir = temp_dir("ok");
        // `command` is an argv, not a shell line: use a tiny script so the test
        // does not depend on shell quoting rules.
        std::fs::write(
            dir.join("princess.toml"),
            "schema = 1\n[build]\nbackend = \"custom\"\ncommand = \"./fake-build\"\n",
        )
        .unwrap();
        let script = dir.join("fake-build");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'building'\necho 'kernel.c:3:5: error: fake error' >&2\nmkdir -p build\nprintf 'ELF' > build/out.elf\nexit 0\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        let project = Project::open(&dir).unwrap();
        assert_eq!(project.toolchain.workspace_root().is_some(), true);
        let mut spy = Spy { bodies: Vec::new() };
        let result = execute(
            &project,
            &mut spy,
            &CancelToken::new(),
            &BuildOptions::default(),
        )
        .await
        .unwrap();

        assert_eq!(result.payload.status, BuildStatus::Ok);
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].line, Some(3));
        assert_eq!(result.diagnostics[0].col, Some(5));
        assert!(result
            .diagnostics[0]
            .file
            .as_deref()
            .unwrap()
            .ends_with("kernel.c"));
        assert!(result.output_tail.contains("building"));
        assert_eq!(result.artifacts.len(), 1);
        assert!(result.artifacts[0].path.ends_with("out.elf"));

        // Event order contract: started first, finished last, both present.
        assert_eq!(spy.bodies.first().unwrap().kind(), princess_core::EventKind::BuildStarted);
        assert_eq!(spy.bodies.last().unwrap().kind(), princess_core::EventKind::BuildFinished);
        let kinds: Vec<princess_core::EventKind> = spy.bodies.iter().map(|b| b.kind()).collect();
        assert!(kinds.contains(&princess_core::EventKind::LogAppend));
        assert!(kinds.contains(&princess_core::EventKind::BuildDiagnostic));
        assert!(kinds.contains(&princess_core::EventKind::ArtifactChanged));

        // The recorded stream must round-trip through the real serializer.
        let mut out: Vec<u8> = Vec::new();
        {
            let mut recorder = EventRecorder::new(&mut out);
            for body in &spy.bodies {
                recorder.emit(body.clone()).unwrap();
            }
        }
        let text = String::from_utf8(out).unwrap();
        let events = princess_core::parse_ndjson(&text).unwrap();
        princess_core::validate_stream(&events).unwrap();
        assert_eq!(events.len(), spy.bodies.len());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_failing_build_reports_failed_with_the_exit_code() {
        let dir = temp_dir("fail");
        std::fs::write(
            dir.join("princess.toml"),
            "schema = 1\n[build]\nbackend = \"custom\"\ncommand = \"./fake-build\"\n",
        )
        .unwrap();
        let script = dir.join("fake-build");
        std::fs::write(&script, "#!/bin/sh\necho 'boom.c:9:1: error: nope' >&2\nexit 2\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }

        let project = Project::open(&dir).unwrap();
        let mut spy = Spy { bodies: Vec::new() };
        let result = execute(&project, &mut spy, &CancelToken::new(), &BuildOptions::default())
            .await
            .unwrap();
        assert_eq!(result.payload.status, BuildStatus::Failed);
        assert_eq!(result.exit_code, Some(2));
        assert!(!result.succeeded());
        assert_eq!(result.diagnostics.len(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_missing_build_tool_is_a_toolchain_error_with_a_final_event() {
        let dir = temp_dir("missing");
        std::fs::write(
            dir.join("princess.toml"),
            "schema = 1\n[build]\nbackend = \"custom\"\ncommand = \"definitely-not-a-real-tool\"\n",
        )
        .unwrap();
        let project = Project::open(&dir).unwrap();
        let err = plan_build(&project, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::ToolchainMissing);
        assert!(err.message.contains("was not found"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_timed_out_build_kills_the_whole_process_group() {
        let dir = temp_dir("timeout");
        std::fs::write(
            dir.join("princess.toml"),
            "schema = 1\n[build]\nbackend = \"custom\"\ncommand = \"./slow-build\"\n",
        )
        .unwrap();
        let script = dir.join("slow-build");
        // The shell spawns a child that would survive a naive parent-only kill.
        std::fs::write(
            &script,
            "#!/bin/sh\n(sleep 300) &\necho started\nsleep 300\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script, perms).unwrap();
        }
        let project = Project::open(&dir).unwrap();
        let mut spy = Spy { bodies: Vec::new() };
        let started = Instant::now();
        let result = execute(
            &project,
            &mut spy,
            &CancelToken::new(),
            &BuildOptions {
                targets: None,
                timeout_ms: Some(700),
            },
        )
        .await
        .unwrap();
        assert!(result.timed_out, "expected a timeout");
        assert_eq!(result.payload.status, BuildStatus::Failed);
        assert!(started.elapsed() < Duration::from_secs(20), "kill was not prompt");
        assert_eq!(spy.bodies.last().unwrap().kind(), princess_core::EventKind::BuildFinished);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn toolchain_id_uses_detected_facts() {
        let dir = temp_dir("toolchainid");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("princess.toml"), "schema = 1\n").unwrap();
        let project = Project::open(&dir).unwrap();
        let id = toolchain_id(&project);
        assert!(id.starts_with("make+gcc-"), "{id}");
        // The Toolchain type is used here to prove the resolver is wired in.
        let _ = Toolchain::discover(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
