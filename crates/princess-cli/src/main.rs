//! `princess-cli` — the headless acceptance vehicle for the P2 engine.
//!
//! It is deliberately thin: every decision that matters lives in
//! `princess-core` (event/error/config contracts) or in the backend modules
//! (`build.rs`, `run.rs`, `symbolize.rs`).  What this file owns is the CLI
//! contract itself:
//!
//! * **stdout is the NDJSON event stream**, one event per line, flushed per
//!   event.  Human-readable text goes to stderr, never to stdout.
//! * `--record <PATH>` tees the same stream to a file — that is how the P3
//!   replay fixture (`fixtures/events/refkernel-run.ndjson`) is produced.
//! * exit codes: `0` = the operation produced its event stream (the event's
//!   `status`/`reason` is authoritative), `1` = the operation could not run to
//!   completion, `2` = usage error.  `--strict` additionally fails on a
//!   non-`ok` status.
//!
//! `run` and `build` are async (they spawn and multiplex child processes with
//! `tokio::process`, contract §6 rule 1); the rest is plain synchronous code.

mod args;
mod build;
mod diagnostics;
mod env;
mod output;
mod proc;
mod project;
mod run;
mod serial;
mod symbolize;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use princess_core::event::{new_op_id, EventRecorder};
use princess_core::types::{ArtifactKind, DiagnosticSeverity, LogStream, TextEncoding};
use princess_core::{parse_ndjson, validate_stream, ErrorCode, PrincessError, Result};

use crate::args::{Args, Command, EventsArgs, SymbolicateArgs};
use crate::env::Toolchain;
use crate::output::EventOutput;
use crate::project::{discover_artifacts, Project};

/// Exit code for "the operation could not run to completion".
const EXIT_OPERATION_FAILED: u8 = 1;
/// Exit code for a usage error.
const EXIT_USAGE: u8 = 2;

#[tokio::main]
async fn main() -> ExitCode {
    let parsed = match args::parse(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("princess-cli: {err}\n");
            eprint!("{}", args::USAGE);
            return ExitCode::from(EXIT_USAGE);
        }
    };

    if parsed.help {
        print!("{}", args::USAGE);
        return ExitCode::SUCCESS;
    }
    let Some(command) = parsed.command.clone() else {
        eprint!("{}", args::USAGE);
        return ExitCode::from(EXIT_USAGE);
    };

    let output = match EventOutput::new(parsed.record.as_deref()) {
        Ok(output) => output,
        Err(err) => {
            eprintln!("princess-cli: {err}");
            return ExitCode::from(EXIT_OPERATION_FAILED);
        }
    };
    if let Some(path) = output.record_path() {
        let text = format!("recording events to {}", path.display());
        output::human(parsed.quiet, text);
    }
    let mut recorder = output.into_recorder();
    recorder.set_op_id(Some(parsed.op_id.clone().unwrap_or_else(new_op_id)));
    let op_id = recorder.op_id().unwrap_or("op-unknown").to_string();

    match dispatch(&command, &parsed, &mut recorder).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            // Fail loud, twice: once in the event stream (the UI sees it) and
            // once on stderr with the exit code (a shell script sees it).
            let _ = recorder.emit(princess_core::EventBody::LogAppend(
                princess_core::event::LogAppendPayload {
                    stream: LogStream::Ide,
                    chunk: format!("error [{op_id}] {}: {}\n", error.code, error.message),
                    encoding: TextEncoding::Utf8,
                },
            ));
            eprintln!("princess-cli: {error}");
            if let Some(detail) = &error.detail {
                eprintln!("--- detail ---\n{detail}\n--------------");
            }
            ExitCode::from(EXIT_OPERATION_FAILED)
        }
    }
}

async fn dispatch(
    command: &Command,
    parsed: &Args,
    recorder: &mut EventRecorder<EventOutput>,
) -> Result<u8> {
    match command {
        Command::Doctor => doctor(&parsed.project, recorder),
        Command::Build(build_args) => {
            let project = Project::open(&parsed.project)?;
            announce_project(parsed, recorder, &project)?;
            let cancel = princess_core::traits::CancelToken::new();
            let result = build::execute(
                &project,
                recorder,
                &cancel,
                &build::BuildOptions {
                    targets: (!build_args.targets.is_empty()).then(|| build_args.targets.clone()),
                    timeout_ms: build_args.timeout_ms,
                },
            )
            .await?;
            report_build(parsed, &result);
            finish(parsed, result.payload.status != princess_core::types::BuildStatus::Ok)
        }
        Command::Run(run_args) => {
            let project = Project::open(&parsed.project)?;
            announce_project(parsed, recorder, &project)?;
            let cancel = princess_core::traits::CancelToken::new();
            let result = run::execute(
                &project,
                recorder,
                &cancel,
                &run::RunOptions {
                    timeout_ms: run_args.timeout_ms,
                    cancel_after_ms: run_args.cancel_after_ms,
                    do_build: !run_args.no_build,
                    keep_qemu_log: run_args.keep_qemu_log,
                },
            )
            .await?;
            report_run(parsed, &result);
            finish(
                parsed,
                result.outcome.reason != princess_core::types::ExitReason::GuestShutdown,
            )
        }
        Command::Symbolicate(symbolicate_args) => {
            symbolicate(parsed, symbolicate_args, recorder)?;
            finish(parsed, false)
        }
        Command::Events(events_args) => {
            validate_events(parsed, events_args, recorder)?;
            finish(parsed, false)
        }
    }
}

/// `0` normally; `1` under `--strict` when the operation reported failure.
fn finish(parsed: &Args, failed: bool) -> Result<u8> {
    if failed && parsed.strict {
        return Ok(EXIT_OPERATION_FAILED);
    }
    Ok(0)
}

fn announce_project(
    parsed: &Args,
    recorder: &mut EventRecorder<EventOutput>,
    project: &Project,
) -> Result<()> {
    recorder.note(project.source_note())?;
    let text = format!("project: {} ({})", project.name(), project.source_note());
    output::human(parsed.quiet, text);
    Ok(())
}

fn report_build(parsed: &Args, result: &build::BuildResult) {
    let summary = format!(
        "build {} in {} ms (exit {:?}); {} artifact(s), {} diagnostic(s); argv: {}",
        result.payload.status.as_str(),
        result.payload.duration_ms,
        result.exit_code,
        result.payload.artifacts.len(),
        result.diagnostics.len(),
        result.plan.argv.join(" ")
    );
    output::human(parsed.quiet, summary);
    for artifact in &result.payload.artifacts {
        let sha = &artifact.sha256[..16.min(artifact.sha256.len())];
        output::human(
            parsed.quiet,
            format!(
                "  artifact: {} ({}, {} bytes, sha256 {}…)",
                artifact.path,
                artifact_kind_label(artifact.kind),
                artifact.size,
                sha
            ),
        );
    }
    if result.cancelled || result.timed_out {
        output::human(
            parsed.quiet,
            format!(
                "  build stopped early: cancelled={} timed_out={}",
                result.cancelled, result.timed_out
            ),
        );
    }
    if result.payload.status != princess_core::types::BuildStatus::Ok && !result.output_tail.is_empty() {
        output::human(parsed.quiet, "  --- last build output ---");
        for line in result.output_tail.lines().rev().take(20).collect::<Vec<_>>().into_iter().rev() {
            output::human(parsed.quiet, format!("  | {line}"));
        }
    }
    for diagnostic in result
        .diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .take(10)
    {
        let line = diagnostic
            .line
            .map(|line| line.to_string())
            .unwrap_or_else(|| "?".to_string());
        output::human(
            parsed.quiet,
            format!(
                "  error: {}:{}: {}",
                diagnostic.file.as_deref().unwrap_or("<no file>"),
                line,
                diagnostic.message
            ),
        );
    }
}

fn report_run(parsed: &Args, result: &run::RunResult) {
    output::human(
        parsed.quiet,
        format!(
            "run reason={} exit={:?} uptime={} ms (boot medium: {})",
            result.outcome.reason.as_str(),
            result.outcome.exit_code,
            result.outcome.uptime_ms,
            match &result.plan.boot_medium {
                princess_core::traits::BootMedium::Iso(path) => format!("iso {}", path.display()),
                princess_core::traits::BootMedium::Kernel(path) => {
                    format!("kernel {}", path.display())
                }
                princess_core::traits::BootMedium::Disk(path) => format!("disk {}", path.display()),
            }
        ),
    );
    output::human(parsed.quiet, format!("  qemu argv: {}", result.plan.argv.join(" ")));
    if let Some(build) = &result.build {
        output::human(
            parsed.quiet,
            format!(
                "  build before run: {} ({} artifact(s))",
                build.payload.status.as_str(),
                build.payload.artifacts.len()
            ),
        );
    }
    if let Some(fault) = &result.outcome.fault {
        let symbolicated = match &fault.symbolicated {
            Some(location) => format!(
                "{} at {}:{}",
                location.symbol, location.file, location.line
            ),
            None => "none".to_string(),
        };
        output::human(
            parsed.quiet,
            format!(
                "  fault: {} rip={} ({:#x}), symbolicated={}",
                fault.vector, fault.rip_text, fault.rip, symbolicated
            ),
        );
    }
    if let Some(serial_log) = &result.serial_log {
        output::human(parsed.quiet, format!("  serial log: {}", serial_log.display()));
    }
}

fn artifact_kind_label(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Elf => "elf",
        ArtifactKind::Iso => "iso",
        ArtifactKind::Image => "image",
        ArtifactKind::Object => "object",
        ArtifactKind::Archive => "archive",
        ArtifactKind::Other => "other",
    }
}

/// `doctor` — the toolchain table, as events (and on stderr for humans).
fn doctor(project_dir: &Path, recorder: &mut EventRecorder<EventOutput>) -> Result<u8> {
    let toolchain = Toolchain::discover(project_dir);
    recorder.note("PrincessIDE toolchain doctor")?;
    match toolchain.workspace_root() {
        Some(root) => {
            recorder.note(format!("workspace : {}", root.display()))?;
        }
        None => {
            recorder.note(
                "workspace : no scripts/env.sh found above the project; using the ambient PATH",
            )?;
        }
    }
    if let Some(data) = toolchain.qemu_data_dir() {
        recorder.note(format!("qemu data : {}", data.display()))?;
    }

    let table = toolchain.detect();
    for tool in &table {
        let status = if tool.available { "ok" } else { "MISSING" };
        let detail = match (&tool.path, &tool.version) {
            (Some(path), Some(version)) => format!("{version}   {path}"),
            (Some(path), None) => path.clone(),
            (None, _) => format!("(required: {})", tool.name),
        };
        recorder.note(format!("{:<22} {:<10} {}", tool.name, status, detail))?;
    }

    let missing: Vec<&str> = table
        .iter()
        .filter(|tool| tool.required && !tool.available)
        .map(|tool| tool.name.as_str())
        .collect();
    let resolved = table.iter().filter(|t| t.available).count();
    if missing.is_empty() {
        recorder.note(format!(
            "doctor: all required tools present ({resolved} resolved)."
        ))?;
        Ok(0)
    } else {
        Err(PrincessError::new(
            ErrorCode::ToolchainMissing,
            format!(
                "doctor: MISSING REQUIRED: {} (run `bash scripts/bootstrap-toolchain.sh`)",
                missing.join(" ")
            ),
        ))
    }
}

/// `symbolicate` — map a fault RIP to symbol/file/line.
fn symbolicate(
    parsed: &Args,
    command: &SymbolicateArgs,
    recorder: &mut EventRecorder<EventOutput>,
) -> Result<()> {
    let project = Project::open(&parsed.project)?;
    announce_project(parsed, recorder, &project)?;
    let artifacts = discover_artifacts(
        &project.resolved.root,
        &project.resolved.declared_artifacts,
        None,
    );

    let elf = match &command.elf {
        Some(path) => path.clone(),
        None => project.kernel_elf(&artifacts).ok_or_else(|| {
            PrincessError::not_found(
                "no ELF to symbolicate: pass --elf <path> or build the project first",
            )
        })?,
    };
    let symbolizer = symbolize::Symbolizer::new(&elf, &project.toolchain)?;
    recorder.note(format!("elf: {}", elf.display()))?;

    let address = match &command.rip {
        Some(text) => serial::parse_hex_u64(text).ok_or_else(|| {
            PrincessError::invalid_config(format!("--rip {text} is not a hex address"))
        })?,
        None => {
            let log_path = match &command.from_log {
                Some(path) => Some(path.clone()),
                None => project
                    .resolved
                    .serial_tee_to_file
                    .clone()
                    .filter(|path| path.is_file()),
            };
            let path = log_path.ok_or_else(|| {
                PrincessError::not_found(
                    "no RIP to symbolicate: pass --rip <0x...> or --from-log <serial log>",
                )
            })?;
            let text = std::fs::read_to_string(&path).map_err(|err| {
                PrincessError::not_found(format!("cannot read {}: {err}", path.display()))
            })?;
            recorder.note(format!("rip source: {}", path.display()))?;
            serial::fault_rip_in_log(&text).ok_or_else(|| {
                PrincessError::not_found(format!("no FAULT_RIP=0x... line in {}", path.display()))
            })?
        }
    };

    let raw = symbolizer.raw_lookup(address)?;
    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        recorder.note(format!("addr2line: {}", line.trim()))?;
    }

    match symbolizer.lookup(address)? {
        Some(location) => {
            let line = format!(
                "symbolicate: {} -> {}:{} ({})",
                symbolize::hex(address),
                location.file,
                location.line,
                location.symbol
            );
            recorder.note(line.clone())?;
            output::human(parsed.quiet, line);
            Ok(())
        }
        None => Err(PrincessError::new(
            ErrorCode::NotFound,
            format!(
                "addr2line could not map {} to a source location in {}",
                symbolize::hex(address),
                elf.display()
            ),
        )),
    }
}

/// `events` — validate a recorded NDJSON stream against the event contract.
fn validate_events(
    parsed: &Args,
    command: &EventsArgs,
    recorder: &mut EventRecorder<EventOutput>,
) -> Result<()> {
    let path = command
        .file
        .clone()
        .ok_or_else(|| PrincessError::invalid_config("`events` requires --file <PATH>"))?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| PrincessError::not_found(format!("cannot read {}: {err}", path.display())))?;

    let events = parse_ndjson(&text)?;
    validate_stream(&events)?;

    let mut counts: Vec<(princess_core::EventKind, usize)> = Vec::new();
    for event in &events {
        match counts.iter_mut().find(|(kind, _)| *kind == event.kind()) {
            Some((_, count)) => *count += 1,
            None => counts.push((event.kind(), 1)),
        }
    }

    recorder.note(format!(
        "events: {} event(s) parsed and validated from {}",
        events.len(),
        path.display()
    ))?;
    recorder.note(format!(
        "events: model version {}, seq {}..={} (strictly increasing)",
        events.first().map(|e| e.v).unwrap_or(0),
        events.first().map(|e| e.seq).unwrap_or(0),
        events.last().map(|e| e.seq).unwrap_or(0)
    ))?;
    for (kind, count) in counts {
        recorder.note(format!("events: {:<26} {}", kind.as_str(), count))?;
    }

    let op_ids: Vec<String> = {
        let mut ids: Vec<String> = events
            .iter()
            .filter_map(|event| event.op_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    };
    recorder.note(format!("events: opId(s): {}", op_ids.join(", ")))?;

    let summary = format!(
        "{}: OK — {} events, seq {}..={}",
        path.display(),
        events.len(),
        events.first().map(|e| e.seq).unwrap_or(0),
        events.last().map(|e| e.seq).unwrap_or(0)
    );
    output::human(parsed.quiet, summary);
    Ok(())
}

/// The default serial log location, shared with the run path.
pub fn default_serial_log(project: &Project) -> Option<PathBuf> {
    project.resolved.serial_tee_to_file.clone()
}
