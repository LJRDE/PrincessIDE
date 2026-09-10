//! `run` — boot the kernel under QEMU, nothing headless-visible is faked.
//!
//! Contract points this file is responsible for:
//!
//! * §6 rule 1 — stdout (guest serial) and stderr (QEMU's own channel) are read
//!   as two independent streams, never `2>&1`.
//! * §6 rule 2 — the serial device always has a reader, or QEMU blocks when the
//!   pipe fills.
//! * §6 rule 3 — the child runs in its own process group and is killed as a
//!   group, so no orphan `qemu-system-x86_64` survives a timeout or a cancel.
//! * §6 rule 4 — `run.exited.reason` is attributed from evidence
//!   ([`ExitFacts`]), never guessed by the caller.
//! * §6 rule 5 — `run.exited` is emitted on every path out of this function.
//! * §2 — every serial byte is written to the tee file *before* its event.

use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use princess_core::event::{EventBody, RunFaultPayload, RunStartedPayload};
use princess_core::traits::{
    classify_exit, BootMedium, CancelToken, EventSink, ExitFacts, RunOutcome, RunPlan,
};
use princess_core::types::{ExitReason, LogStream, SerialCapture, TextEncoding};
use princess_core::{ErrorCode, PrincessError, Result, Utf8Chunker};

use crate::build::{self, BuildOptions, BuildResult};
use crate::project::Project;
use crate::proc;
use crate::serial::{SerialObservation, SerialObserver};
use crate::symbolize::Symbolizer;

/// How long to keep draining output after QEMU exited.
const DRAIN_GRACE: Duration = Duration::from_secs(3);
/// Idle interval after which a partial serial line is pushed.
const IDLE_FLUSH: Duration = Duration::from_millis(120);
/// Upper bound on `run.fault` events per run (a fault storm must not flood the
/// UI; the truncation is reported in the stream).
const MAX_FAULT_EVENTS: usize = 16;

/// Options for one run.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Override `[run] timeout_ms`.
    pub timeout_ms: Option<u64>,
    /// Cancel the run after this many milliseconds (produces `reason=killed`).
    pub cancel_after_ms: Option<u64>,
    /// Build before booting (the CLI's default).
    pub do_build: bool,
    /// Keep the QEMU debug log used for triple-fault detection.
    pub keep_qemu_log: bool,
}

/// Result of one run.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub plan: RunPlan,
    pub outcome: RunOutcome,
    pub serial_log: Option<PathBuf>,
    /// The build that preceded this run, when one was performed.
    pub build: Option<BuildResult>,
}

/// QEMU flags the engine owns; whatever the manifest says about them is dropped
/// (with an `ide` note) rather than silently duplicated.
const ENGINE_OWNED_FLAGS: [&str; 10] = [
    "-serial",
    "-monitor",
    "-display",
    "-nographic",
    "-d",
    "-D",
    "-cdrom",
    "-boot",
    "-kernel",
    "-no-reboot",
];

/// Split manifest args into the ones the engine keeps and the ones it owns.
fn partition_args(args: &[String]) -> (Vec<String>, Vec<String>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if ENGINE_OWNED_FLAGS.contains(&token.as_str()) {
            dropped.push(token.clone());
            // Drop the flag's value too, when it has one.
            if index + 1 < args.len() && !args[index + 1].starts_with('-') {
                dropped.push(args[index + 1].clone());
                index += 1;
            }
        } else {
            kept.push(token.clone());
        }
        index += 1;
    }
    (kept, dropped)
}

/// Build the exact QEMU argv.
///
/// The boot medium is chosen by the engine ([`Project::boot_medium`]): a GRUB ISO
/// is the only way to boot a 64-bit multiboot2 kernel under QEMU, and QEMU
/// rejects ELF64 on `-kernel` ("Cannot load x86-64 image, give a 32bit one" —
/// measured in `docs/p0-toolchain-report.md` §4.2).
pub fn plan_run(project: &Project, medium: &BootMedium, options: &RunOptions) -> Result<RunPlan> {
    let qemu = project.toolchain.which("qemu-system-x86_64").ok_or_else(|| {
        PrincessError::new(
            ErrorCode::ToolchainMissing,
            "qemu-system-x86_64 was not found on PATH; run `bash scripts/bootstrap-toolchain.sh`",
        )
    })?;

    let mut argv = vec![qemu.to_string_lossy().into_owned()];
    if let Some(data) = project.toolchain.qemu_data_dir() {
        argv.push("-L".to_string());
        argv.push(data.to_string_lossy().into_owned());
    }

    let (user_args, _dropped) = partition_args(&project.resolved.config.run.args);
    argv.extend(user_args);

    match medium {
        BootMedium::Iso(path) => {
            argv.push("-cdrom".to_string());
            argv.push(path.to_string_lossy().into_owned());
            argv.push("-boot".to_string());
            argv.push("d".to_string());
        }
        BootMedium::Kernel(path) => {
            argv.push("-kernel".to_string());
            argv.push(path.to_string_lossy().into_owned());
        }
        BootMedium::Disk(path) => {
            argv.push("-drive".to_string());
            argv.push(format!("file={},format=raw,if=floppy", path.display()));
        }
    }

    // Engine-owned capture flags (§6 rule 2): the serial device must always have
    // a reader, and stdout is that reader.
    argv.push("-display".to_string());
    argv.push("none".to_string());
    argv.push("-monitor".to_string());
    argv.push("none".to_string());
    argv.push("-serial".to_string());
    argv.push("stdio".to_string());
    argv.push("-no-reboot".to_string());
    // Exception/reset logging: the only honest way to see a triple fault, since
    // the guest never gets to print anything in that case.
    argv.push("-d".to_string());
    argv.push("int,cpu_reset,guest_errors".to_string());
    argv.push("-D".to_string());
    argv.push(qemu_debug_log(project).to_string_lossy().into_owned());

    let timeout_ms = options
        .timeout_ms
        .unwrap_or(project.resolved.config.run.timeout_ms);

    // `-s` opens the gdb stub at `[debug] stub`'s port; report it instead of
    // pretending there is no stub (contract §2 lets `gdbStub` be null, not wrong).
    let gdb_stub = if project.resolved.config.run.args.iter().any(|arg| arg == "-s") {
        Some(princess_core::types::GdbStub {
            host: project.resolved.config.debug.stub.host.clone(),
            port: project.resolved.config.debug.stub.port,
            mode: project.resolved.config.debug.stub.mode,
        })
    } else {
        None
    };

    Ok(RunPlan {
        backend: project.resolved.config.run.backend.as_str().to_string(),
        argv,
        cwd: project.resolved.build_cwd.clone(),
        boot_medium: medium.clone(),
        boot: project.resolved.config.run.boot,
        timeout_ms,
        serial: SerialCapture {
            device: project.resolved.config.run.serial.device.clone(),
            tee_to_file: project
                .resolved
                .serial_tee_to_file
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        },
        gdb_stub,
    })
}

/// Where QEMU writes its `-d` log.
fn qemu_debug_log(project: &Project) -> PathBuf {
    project
        .resolved
        .root
        .join("build")
        .join("qemu-debug.log")
}

/// Boot the kernel: build first when asked, then run QEMU to completion.
pub async fn execute(
    project: &Project,
    events: &mut dyn EventSink,
    cancel: &CancelToken,
    options: &RunOptions,
) -> Result<RunResult> {
    // ------------------------------------------------------------- build ----
    let mut build_result = None;
    let artifacts = if options.do_build {
        let result = build::execute(
            project,
            events,
            cancel,
            &BuildOptions {
                targets: None,
                timeout_ms: None,
            },
        )
        .await?;
        let artifacts = result.artifacts.clone();
        let ok = result.succeeded();
        build_result = Some(result);
        if !ok {
            events.note("build did not succeed; booting the artifacts that exist anyway")?;
        }
        artifacts
    } else {
        crate::project::discover_artifacts(&project.resolved.root, &project.resolved.declared_artifacts, None)
    };

    let medium = project.boot_medium(&artifacts)?;
    let (_, dropped_args) = partition_args(&project.resolved.config.run.args);
    if !dropped_args.is_empty() {
        events.note(&format!(
            "[run] args contains emulator flags the engine owns and ignores: {}",
            dropped_args.join(" ")
        ))?;
    }
    let plan = plan_run(project, &medium, options)?;

    events.emit(EventBody::RunStarted(RunStartedPayload {
        qemu_argv: plan.argv.clone(),
        gdb_stub: plan.gdb_stub.as_ref().map(|stub| stub.endpoint()),
    }))?;

    let serial_log = plan.serial.tee_to_file.as_ref().map(PathBuf::from);
    let mut serial_file = match &serial_log {
        Some(path) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            Some(std::fs::File::create(path).map_err(|err| {
                PrincessError::internal(format!("cannot create serial log {}: {err}", path.display()))
            })?)
        }
        None => None,
    };

    // -------------------------------------------------------------- spawn ----
    let debug_log = qemu_debug_log(project);
    let mut command = tokio::process::Command::new(&plan.argv[0]);
    command
        .args(&plan.argv[1..])
        .current_dir(&plan.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    project.toolchain.apply_tokio(&mut command);

    let started = Instant::now();
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
                _ => PrincessError::new(
                    ErrorCode::QemuFailed,
                    format!("cannot execute {}: {err}", plan.argv[0]),
                ),
            };
            events.note(&format!("run failed to start: {error}"))?;
            let outcome = RunOutcome {
                exit_code: None,
                reason: ExitReason::Killed,
                uptime_ms: started.elapsed().as_millis() as u64,
                fault: None,
                serial_log: serial_log.clone(),
            };
            events.emit(EventBody::RunExited(outcome.exited_event()))?;
            return Err(error.with_detail(plan.argv.join(" ")));
        }
    };
    let pgid = child
        .id()
        .ok_or_else(|| PrincessError::internal("qemu child has no pid"))?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| PrincessError::internal("qemu stdout is not piped"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| PrincessError::internal("qemu stderr is not piped"))?;

    let mut serial_chunker = Utf8Chunker::new();
    let mut monitor_chunker = Utf8Chunker::new();
    let mut serial_open = true;
    let mut monitor_open = true;

    let mut observer = SerialObserver::new();
    let mut fault_payloads: Vec<RunFaultPayload> = Vec::new();
    let mut symbolizer = project
        .kernel_elf(&artifacts)
        .and_then(|elf| Symbolizer::new(&elf, &project.toolchain).ok());
    if symbolizer.is_none() {
        events.note("no ELF with debug info: faults will be reported unsymbolicated")?;
    }

    let mut timed_out = false;
    let mut status: Option<std::process::ExitStatus> = None;
    let mut exit_at: Option<Instant> = None;
    let mut monitor_text = String::new();

    // One-shot deadlines: a fired timer must be disarmed, or the branch would
    // complete instantly on every later iteration and spin the loop.
    let mut deadline = Some(Instant::now() + Duration::from_millis(plan.timeout_ms));
    let mut cancel_deadline = options
        .cancel_after_ms
        .map(|ms| Instant::now() + Duration::from_millis(ms));
    let mut cancel_kill_done = false;

    // ------------------------------------------------------------ pump ------
    loop {
        if status.is_some() && !serial_open && !monitor_open {
            break;
        }
        if let Some(at) = exit_at {
            if at.elapsed() > DRAIN_GRACE {
                break;
            }
        }

        tokio::select! {
            result = read_chunk(&mut stdout), if serial_open => {
                match result {
                    Ok(bytes) if bytes.is_empty() => {
                        serial_open = false;
                        drain_serial(events, &mut serial_chunker, false, &mut serial_file, &mut observer, &mut fault_payloads, &mut symbolizer)?;
                    }
                    Ok(bytes) => {
                        serial_chunker.push(&bytes);
                        drain_serial(events, &mut serial_chunker, true, &mut serial_file, &mut observer, &mut fault_payloads, &mut symbolizer)?;
                    }
                    Err(err) => {
                        serial_open = false;
                        events.note(&format!("serial read error: {err}"))?;
                    }
                }
            }
            result = read_chunk(&mut stderr), if monitor_open => {
                match result {
                    Ok(bytes) if bytes.is_empty() => {
                        monitor_open = false;
                        drain_monitor(events, &mut monitor_chunker, false, &mut monitor_text)?;
                    }
                    Ok(bytes) => {
                        monitor_chunker.push(&bytes);
                        drain_monitor(events, &mut monitor_chunker, true, &mut monitor_text)?;
                    }
                    Err(err) => {
                        monitor_open = false;
                        events.note(&format!("qemu stderr read error: {err}"))?;
                    }
                }
            }
            result = child.wait(), if status.is_none() => {
                status = Some(result.map_err(|err| {
                    PrincessError::new(ErrorCode::QemuFailed, format!("cannot wait for qemu: {err}"))
                })?);
                exit_at = Some(Instant::now());
            }
            _ = tokio::time::sleep(IDLE_FLUSH) => {
                drain_serial(events, &mut serial_chunker, false, &mut serial_file, &mut observer, &mut fault_payloads, &mut symbolizer)?;
                drain_monitor(events, &mut monitor_chunker, false, &mut monitor_text)?;
            }
            _ = wait_until(deadline), if deadline.is_some() => {
                deadline = None;
                timed_out = true;
                cancel_kill_done = true;
                events.note(&format!(
                    "run exceeded its {} ms deadline; killing process group {pgid}",
                    plan.timeout_ms
                ))?;
                proc::kill_group_and_wait(pgid).await?;
            }
            _ = wait_until(cancel_deadline), if cancel_deadline.is_some() => {
                cancel_deadline = None;
                cancel_kill_done = true;
                events.note(&format!(
                    "run cancelled after {} ms (--cancel-after-ms); killing process group {pgid}",
                    options.cancel_after_ms.unwrap_or(0)
                ))?;
                cancel.cancel();
                proc::kill_group_and_wait(pgid).await?;
            }
        }

        // A cancel that arrived from outside this loop (op:cancel / user Ctrl-C).
        if cancel.is_cancelled() && !cancel_kill_done && status.is_none() {
            cancel_kill_done = true;
            events.note(&format!("run cancelled; killing process group {pgid}"))?;
            proc::kill_group_and_wait(pgid).await?;
        }
        if cancel_kill_done && status.is_none() {
            status = Some(child.wait().await.map_err(|err| {
                PrincessError::new(ErrorCode::QemuFailed, format!("cannot reap qemu: {err}"))
            })?);
            exit_at = Some(Instant::now());
        }
    }

    // Final drain.
    drain_serial(events, &mut serial_chunker, false, &mut serial_file, &mut observer, &mut fault_payloads, &mut symbolizer)?;
    drain_monitor(events, &mut monitor_chunker, false, &mut monitor_text)?;
    if let Some(remainder) = serial_chunker.take_remainder() {
        push_serial(events, remainder, &mut serial_file, &mut observer, &mut fault_payloads, &mut symbolizer)?;
    }
    if let Some(remainder) = monitor_chunker.take_remainder() {
        events.emit(EventBody::LogAppend(princess_core::event::LogAppendPayload {
            stream: LogStream::QemuMonitor,
            chunk: remainder.0,
            encoding: remainder.1,
        }))?;
    }

    if status.is_none() {
        status = Some(child.wait().await.map_err(|err| {
            PrincessError::new(ErrorCode::QemuFailed, format!("cannot reap qemu: {err}"))
        })?);
    }
    // Never leave the machine running behind us (§6 rule 3).
    proc::kill_group_and_wait(pgid).await.ok();
    if let Some(file) = serial_file.as_mut() {
        let _ = file.flush();
    }

    let uptime_ms = started.elapsed().as_millis() as u64;
    let exit_code = status.and_then(|s| s.code());
    let signal = status.and_then(|s| {
        use std::os::unix::process::ExitStatusExt;
        s.signal()
    });

    let qemu_debug = std::fs::read_to_string(&debug_log).unwrap_or_default();
    let triple_fault = detect_triple_fault(&qemu_debug, &monitor_text);
    if triple_fault {
        events.note("guest reset without a recovery path (triple fault) observed in the QEMU log")?;
    }

    let facts = ExitFacts {
        exit_code,
        signal,
        timed_out,
        cancelled: cancel.is_cancelled() && !timed_out,
        // A clean exit with no reset means the guest asked the machine to stop;
        // QEMU prints nothing else on an ACPI power-off.
        guest_powered_off: !timed_out && !cancel.is_cancelled() && !triple_fault && exit_code == Some(0),
        triple_fault,
    };
    let reason = classify_exit(&facts);

    let outcome = RunOutcome {
        exit_code,
        reason,
        uptime_ms,
        fault: fault_payloads.last().cloned(),
        serial_log: serial_log.clone(),
    };
    // The banner/panic state is part of the run's evidence: a run that never
    // printed the banner did not reach the kernel at all.
    events.note(&format!(
        "serial: banner_seen={} panic_seen={} faults={}{}",
        observer.banner_seen(),
        observer.panic_seen(),
        fault_payloads.len(),
        match observer.fault() {
            Some(fault) => format!(" last_fault={} {}", fault.rip_text, fault.vector),
            None => String::new(),
        }
    ))?;
    // Summary before the terminal event, so `run.exited` stays the last event of
    // the operation (contract §6 rule 5).
    events.note(&format!(
        "run ended: reason={}, exit={exit_code:?}, uptime={uptime_ms} ms{}",
        reason.as_str(),
        match &serial_log {
            Some(path) => format!(", serial log {}", path.display()),
            None => String::new(),
        }
    ))?;
    events.emit(EventBody::RunExited(outcome.exited_event()))?;

    if !options.keep_qemu_log {
        let _ = std::fs::remove_file(&debug_log);
    }

    Ok(RunResult {
        plan,
        outcome,
        serial_log,
        build: build_result,
    })
}

/// Is the QEMU log evidence of an unrecoverable CPU reset?
///
/// QEMU prints `Triple fault` (and a CPU reset record) on the `-d int,cpu_reset`
/// channel when the guest faults with no usable handler — the guest itself
/// cannot report it, so this log is the only evidence available.
pub fn detect_triple_fault(qemu_debug: &str, monitor_text: &str) -> bool {
    let haystacks = [qemu_debug, monitor_text];
    haystacks.iter().any(|text| {
        text.lines().any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("triple fault")
        })
    })
}

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
        None => std::future::pending::<()>().await,
    }
}

fn drain_serial(
    events: &mut dyn EventSink,
    chunker: &mut Utf8Chunker,
    lines_only: bool,
    serial_file: &mut Option<std::fs::File>,
    observer: &mut SerialObserver,
    faults: &mut Vec<RunFaultPayload>,
    symbolizer: &mut Option<Symbolizer>,
) -> Result<()> {
    if lines_only {
        while let Some(chunk) = chunker.take_lines() {
            push_serial(events, chunk, serial_file, observer, faults, symbolizer)?;
        }
    } else if let Some(chunk) = chunker.take_available() {
        push_serial(events, chunk, serial_file, observer, faults, symbolizer)?;
    }
    Ok(())
}

fn drain_monitor(
    events: &mut dyn EventSink,
    chunker: &mut Utf8Chunker,
    lines_only: bool,
    monitor_text: &mut String,
) -> Result<()> {
    let chunk = if lines_only {
        chunker.take_lines()
    } else {
        chunker.take_available()
    };
    if let Some((text, encoding)) = chunk {
        monitor_text.push_str(&text);
        events.emit(EventBody::LogAppend(princess_core::event::LogAppendPayload {
            stream: LogStream::QemuMonitor,
            chunk: text,
            encoding,
        }))?;
    }
    Ok(())
}

/// Persist a serial chunk, then emit it, then turn its lines into events.
///
/// The write-then-emit order is the contract's "先落盘再推送": a UI that misses
/// the event can always recover the text from the log.
fn push_serial(
    events: &mut dyn EventSink,
    (text, encoding): (String, TextEncoding),
    serial_file: &mut Option<std::fs::File>,
    observer: &mut SerialObserver,
    faults: &mut Vec<RunFaultPayload>,
    symbolizer: &mut Option<Symbolizer>,
) -> Result<()> {
    if let Some(file) = serial_file.as_mut() {
        file.write_all(text.as_bytes()).and_then(|()| file.flush()).map_err(
            |err| PrincessError::internal(format!("cannot write the serial log: {err}")),
        )?;
    }

    events.emit(EventBody::LogAppend(princess_core::event::LogAppendPayload {
        stream: LogStream::SerialCom1,
        chunk: text.clone(),
        encoding,
    }))?;

    for line in text.lines() {
        for observation in observer.feed(line) {
            match observation {
                SerialObservation::Banner => {
                    events.note("guest banner observed on serial.com1")?;
                }
                SerialObservation::Panic => {
                    events.note("guest panic observed on serial.com1")?;
                }
                SerialObservation::Fault(raw) => {
                    let symbolicated = symbolizer
                        .as_ref()
                        .and_then(|symbolizer| symbolizer.lookup(raw.rip).ok().flatten());
                    match &symbolicated {
                        Some(location) => events.note(&format!(
                            "fault {:#x} symbolicated as {} at {}:{}",
                            raw.rip, location.symbol, location.file, location.line
                        ))?,
                        None => events.note(&format!(
                            "fault {:#x} could not be symbolicated (no debug info covers it)",
                            raw.rip
                        ))?,
                    }
                    if faults.len() >= MAX_FAULT_EVENTS {
                        events.note(&format!(
                            "more than {MAX_FAULT_EVENTS} faults in this run; suppressing further run.fault events"
                        ))?;
                        continue;
                    }
                    let payload = raw.into_payload(symbolicated);
                    events.emit(EventBody::RunFault(payload.clone()))?;
                    faults.push(payload);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Toolchain;
    use crate::project::Project;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("princesside-run-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn engine_owned_flags_are_stripped_from_manifest_args() {
        let args: Vec<String> = ["-m", "512M", "-serial", "stdio", "-display", "none", "-no-reboot"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (kept, dropped) = partition_args(&args);
        assert_eq!(kept, vec!["-m".to_string(), "512M".to_string()]);
        assert_eq!(
            dropped,
            vec![
                "-serial".to_string(),
                "stdio".to_string(),
                "-display".to_string(),
                "none".to_string(),
                "-no-reboot".to_string()
            ]
        );
    }

    #[test]
    fn a_plain_memory_flag_survives() {
        let args: Vec<String> = ["-m", "256M", "-smp", "2"].iter().map(|s| s.to_string()).collect();
        let (kept, dropped) = partition_args(&args);
        assert_eq!(kept.len(), 4);
        assert!(dropped.is_empty());
    }

    #[test]
    fn qemu_argv_is_complete_and_engine_owned_flags_come_last() {
        let dir = temp_dir("argv");
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::write(dir.join("princess.toml"), "schema = 1\n").unwrap();
        std::fs::write(dir.join("build/k.elf"), b"elf").unwrap();
        std::fs::write(dir.join("build/k.iso"), b"iso").unwrap();
        let project = Project::open(&dir).unwrap();
        let plan = plan_run(
            &project,
            &BootMedium::Iso(dir.join("build/k.iso")),
            &RunOptions::default(),
        )
        .unwrap();

        let argv = plan.argv.join(" ");
        assert!(argv.contains("-cdrom"), "{argv}");
        assert!(argv.contains("-boot d"), "{argv}");
        assert!(argv.contains("-serial stdio"), "{argv}");
        assert!(argv.contains("-monitor none"), "{argv}");
        assert!(argv.contains("-display none"), "{argv}");
        assert!(argv.contains("-no-reboot"), "{argv}");
        assert!(argv.contains("-d int,cpu_reset,guest_errors"), "{argv}");
        assert!(argv.contains("-L"), "{argv}");
        assert_eq!(plan.timeout_ms, princess_core::DEFAULT_RUN_TIMEOUT_MS);
        assert_eq!(plan.serial.device, "com1");
        assert!(plan.serial.tee_to_file.as_deref().unwrap().ends_with("build/serial.log"));
        assert!(plan.gdb_stub.is_none());
        assert!(Toolchain::discover(&dir).workspace_root().is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn triple_fault_detection_reads_the_qemu_log() {
        assert!(detect_triple_fault(
            "Servicing hardware interrupt 0\nTriple fault\nCPU Reset (CPU 0)\n",
            ""
        ));
        assert!(detect_triple_fault("", "qemu: TRIPLE FAULT\n"));
        assert!(!detect_triple_fault(
            "v=06 e=0000 i=0 cpl=0 IP=0008:0000000000100b3d\n",
            ""
        ));
        assert!(!detect_triple_fault("", ""));
    }
}
