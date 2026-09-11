//! The `RunBackend` implementation — contract §5/§6.
//!
//! One run, start to finish:
//!
//! ```text
//! plan()   → RunPlan (argv pinned by D2/D9)
//! launch() → run.started
//!          → log.append(serial.com1)   (each chunk written to disk first)
//!          → run.fault*                (from what the guest printed)
//!          → log.append(qemu.monitor)  (QEMU's own stderr)
//!          → run.exited                (always, on every path out)
//! ```
//!
//! `run.exited` is emitted on **every** exit path including a failed spawn
//! (§6 rule 5), and its `reason` is attributed by
//! [`princess_core::classify_exit`] from evidence — never from the exit code
//! alone (D9, see [`crate::exit`]).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use princess_core::config::BootProtocol;
use princess_core::error::{ErrorCode, PrincessError, Result};
use princess_core::event::{EventBody, LogAppendPayload, RunStartedPayload};
use princess_core::traits::{
    BootMedium, CancelToken, EventSink, RunBackend, RunOutcome, RunPlan,
};
use princess_core::types::{ExitReason, GdbStub, LogStream, SerialCapture, SymbolicatedLocation, TextEncoding};

use crate::exit::{triple_fault_in_file, QemuEvidence};
use crate::process::{Pipe, ProcessOutcome, ProcessRunner, SpawnSpec, SystemRunner};
use crate::qemu::{check_argv_contract, default_stub, plan_run, PlanOptions, PlanInputs};
use crate::serial::{fault_rip_in_log, PageFaultGate, SerialObservation, SerialObserver};

/// Upper bound on `run.fault` events per run: a fault storm (a guest that keeps
/// re-faulting) must not flood the event stream.  The suppression is itself
/// reported, so the UI is never silently misled.
pub const MAX_FAULT_EVENTS: usize = 16;

/// A symbolizer hook.  This crate does **not** parse DWARF (that is B3/P5); it
/// accepts a closure so the engine can decorate `run.fault` without the run
/// backend depending on the symbol crate.
///
/// `Send + Sync` because the run backend is held as `Box<dyn RunBackend>`, and
/// that trait requires both.
pub type SymbolizeFn<'a> = dyn Fn(u64) -> Option<SymbolicatedLocation> + Send + Sync + 'a;

/// Everything a run needs, resolved by the caller.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    /// Where QEMU lives and how to plan the argv.
    pub plan: PlanOptions,
    /// Environment applied to the QEMU child (workspace toolchain `PATH`, …).
    pub env: BTreeMap<String, String>,
    /// Keep the `-D` debug log after the run (it is evidence; off by default so
    /// a normal run does not accumulate files).
    pub keep_debug_log: bool,
    /// `[debug] stub` defaults, used when `plan.gdb_stub` is unset but the
    /// caller still wants a stub.
    pub stub_defaults: Option<(String, u16)>,
}

/// The QEMU run backend.
///
/// The process runner is injected so unit tests can drive the whole event
/// pipeline without starting an emulator, while the real E2E path uses
/// [`SystemRunner`].
pub struct QemuBackend<R: ProcessRunner = SystemRunner> {
    runner: R,
    options: RunOptions,
}

impl QemuBackend<SystemRunner> {
    /// The production backend.
    pub fn new(options: RunOptions) -> Self {
        Self {
            runner: SystemRunner::new(),
            options,
        }
    }
}

impl<R: ProcessRunner> QemuBackend<R> {
    /// A backend with an injected runner (tests, and the CLI when it wants to
    /// simulate a run).
    pub fn with_runner(runner: R, options: RunOptions) -> Self {
        Self { runner, options }
    }

    /// Build the plan for a resolved project.
    pub fn plan_for(
        &self,
        inputs: &PlanInputs,
        medium: &BootMedium,
    ) -> Result<RunPlan> {
        plan_with_stub(inputs, medium, &self.options)
    }

    /// Execute an already-built [`RunPlan`].
    pub fn launch_plan(
        &self,
        plan: &RunPlan,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
        symbols: Option<&SymbolizeFn<'_>>,
    ) -> Result<RunOutcome> {
        // Defend the D9 contract at the point of execution, so a hand-built plan
        // cannot smuggle `-nographic` past the engine.
        check_argv_contract(&plan.argv)?;

        events.emit(EventBody::RunStarted(RunStartedPayload {
            qemu_argv: plan.argv.clone(),
            gdb_stub: plan.gdb_stub.as_ref().map(GdbStub::endpoint),
        }))?;

        let started = Instant::now();
        let spec = SpawnSpec {
            argv: plan.argv.clone(),
            cwd: plan.cwd.clone(),
            timeout: Some(std::time::Duration::from_millis(plan.timeout_ms)),
            serial_tee: plan.serial.tee_to_file.as_ref().map(PathBuf::from),
            env: self.options.env.clone(),
        };
        let debug_log = self
            .options
            .plan
            .debug_log
            .clone()
            .or_else(|| debug_log_from_argv(&plan.argv));

        let mut observer = SerialObserver::new();
        let mut faults: Vec<princess_core::event::RunFaultPayload> = Vec::new();
        let mut suppressed = 0usize;
        // `ProcessRunner::run` takes an infallible callback, but emitting an
        // event *can* fail (a closed UI sink).  Record the first failure here and
        // stop emitting, rather than panicking inside the reader.
        let emit_error: std::sync::Arc<std::sync::Mutex<Option<PrincessError>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let emit_error_slot = emit_error.clone();

        let outcome = {
            let observer = &mut observer;
            let faults = &mut faults;
            let suppressed = &mut suppressed;
            let events = &mut *events;
            let mut emit = |chunk: crate::process::StreamChunk| {
                if emit_error_slot.lock().map(|slot| slot.is_some()).unwrap_or(true) {
                    return;
                }
                if let Err(err) = handle_chunk(
                    chunk,
                    events,
                    observer,
                    faults,
                    suppressed,
                    symbols,
                ) {
                    if let Ok(mut slot) = emit_error_slot.lock() {
                        *slot = Some(err);
                    }
                }
            };
            self.runner.run(&spec, cancel, &mut emit)
        };

        // A sink failure is reported after the machine has been cleaned up.
        if let Ok(mut slot) = emit_error.lock() {
            if let Some(err) = slot.take() {
                return Err(err);
            }
        }

        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(err) => {
                // §6 rule 5: the terminal event is still emitted, so the UI is
                // never left waiting for a run that already failed.  A runner
                // error means the machine never reported a status at all, which
                // is exactly `killed` (no clean shutdown evidence exists).
                let terminal = RunOutcome {
                    exit_code: None,
                    reason: ExitReason::Killed,
                    uptime_ms: started.elapsed().as_millis() as u64,
                    fault: faults.last().cloned(),
                    serial_log: plan.serial.tee_to_file.as_ref().map(PathBuf::from),
                };
                events.note(&format!("run failed before the machine stopped: {err}"))?;
                events.emit(EventBody::RunExited(terminal.exited_event()))?;
                return Err(err);
            }
        };

        let evidence = self.evidence_from(plan, &outcome, debug_log.as_deref(), cancel);

        // Faults that arrived before the machine died are the most useful thing
        // a run can report; keep the last one for `run.fault`-adjacent tooling.
        let last_fault = faults.last().cloned();
        let uptime_ms = started.elapsed().as_millis() as u64;

        // Evidence line: the banner/panic/fault summary, so a run that never
        // reached the kernel is distinguishable from one that did.
        events.note(&format!(
            "serial: banner_seen={} panic_seen={} faults={}{}{}",
            observer.banner_seen(),
            observer.panic_seen(),
            faults.len(),
            if suppressed > 0 {
                format!(" (suppressed={suppressed})")
            } else {
                String::new()
            },
            match observer.fault() {
                Some(fault) => format!(" last_fault={} {}", fault.rip_text, fault.vector),
                None => String::new(),
            }
        ))?;
        events.note(&format!(
            "run ended: reason={}, exit={:?}, uptime={uptime_ms} ms ({}){}",
            evidence.reason().as_str(),
            outcome.exit_code,
            evidence.justification(),
            match plan.serial.tee_to_file.as_ref() {
                Some(path) => format!(", serial log {path}"),
                None => String::new(),
            }
        ))?;

        let terminal = RunOutcome {
            exit_code: outcome.exit_code,
            reason: evidence.reason(),
            uptime_ms,
            fault: last_fault,
            serial_log: plan.serial.tee_to_file.as_ref().map(PathBuf::from),
        };
        // Terminal event last: nothing may follow `run.exited` (§6 rule 5).
        events.emit(EventBody::RunExited(terminal.exited_event()))?;

        if !self.options.keep_debug_log {
            if let Some(path) = &debug_log {
                let _ = std::fs::remove_file(path);
            }
        }

        Ok(terminal)
    }

    /// Assemble the evidence and let `core` attribute the reason.
    fn evidence_from(
        &self,
        plan: &RunPlan,
        outcome: &ProcessOutcome,
        debug_log: Option<&std::path::Path>,
        cancel: &CancelToken,
    ) -> QemuEvidence {
        // D9: a triple fault is known only from the `-d int,cpu_reset` log.
        let triple_fault = debug_log.map(triple_fault_in_file).unwrap_or(false);

        // A clean emulator exit is only a shutdown when the guest's own output
        // agrees: the reference/paging fixtures print their banner, and an ACPI
        // power-off is the only path that exits 0 without `-no-shutdown`.
        let guest_shutdown = outcome.exit_code == Some(0)
            && outcome.signal.is_none()
            && !outcome.timed_out
            && !outcome.cancelled
            && !triple_fault;

        QemuEvidence {
            exit_code: outcome.exit_code,
            signal: outcome.signal,
            timed_out: outcome.timed_out,
            cancelled: outcome.cancelled || cancel.is_cancelled(),
            triple_fault,
            guest_shutdown: guest_shutdown && plan.serial.device == "com1",
        }
    }
}

/// Turn one streamed chunk into the events it carries.
///
/// Extracted from the run loop so the callback given to [`ProcessRunner::run`]
/// can be infallible: it records the first failure instead of propagating it
/// through a signature that cannot carry one.
fn handle_chunk(
    chunk: crate::process::StreamChunk,
    events: &mut dyn EventSink,
    observer: &mut SerialObserver,
    faults: &mut Vec<princess_core::event::RunFaultPayload>,
    suppressed: &mut usize,
    symbols: Option<&SymbolizeFn<'_>>,
) -> Result<()> {
    match chunk.pipe {
        Pipe::Serial => {
            // The runner has already written this chunk to the tee file; push it
            // to the UI next (§2 "先落盘再推送").
            events.emit(EventBody::LogAppend(LogAppendPayload {
                stream: LogStream::SerialCom1,
                chunk: chunk.text.clone(),
                encoding: chunk.encoding,
            }))?;
            for line in chunk.text.lines() {
                for observation in observer.feed(line) {
                    match observation {
                        SerialObservation::Banner(banner) => {
                            events.note(&format!(
                                "guest banner observed on serial.com1: {banner}"
                            ))?;
                        }
                        SerialObservation::PagingState { enabled } => {
                            events.note(&format!(
                                "guest paging state: CR0.PG={}",
                                u8::from(enabled)
                            ))?;
                        }
                        SerialObservation::Panic => {
                            events.note("guest panic observed on serial.com1")?;
                        }
                        SerialObservation::Fault(raw) => {
                            // D9: a #PF is only asserted as a page fault once
                            // paging is known to be on.
                            if raw.is_page_fault() {
                                match observer.page_fault_gate() {
                                    Some(PageFaultGate::PagingConfirmed) => {}
                                    Some(PageFaultGate::PagingDisabled) => {
                                        events.note(
                                            "guest reported #PF but the stream showed CR0.PG=0: not attributing a page fault",
                                        )?;
                                    }
                                    _ => events.note(
                                        "guest reported #PF; CR0.PG=1 was not observed on the serial stream, so paging is unproven",
                                    )?,
                                }
                            }
                            let symbolicated = symbols.and_then(|lookup| lookup(raw.rip));
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
                                *suppressed += 1;
                                if *suppressed == 1 {
                                    events.note(&format!(
                                        "more than {MAX_FAULT_EVENTS} faults in this run; suppressing further run.fault events"
                                    ))?;
                                }
                                continue;
                            }
                            let payload = raw.into_payload(symbolicated);
                            events.emit(EventBody::RunFault(payload.clone()))?;
                            faults.push(payload);
                        }
                    }
                }
            }
        }
        Pipe::Emulator => {
            events.emit(EventBody::LogAppend(LogAppendPayload {
                stream: LogStream::QemuMonitor,
                chunk: chunk.text,
                encoding: chunk.encoding,
            }))?;
        }
    }
    Ok(())
}

/// Recover `-D <path>` from an argv (so a plan made elsewhere still leaves its
/// evidence where the backend can read it).
pub fn debug_log_from_argv(argv: &[String]) -> Option<PathBuf> {
    let index = argv.iter().position(|token| token == "-D")?;
    argv.get(index + 1).map(PathBuf::from)
}

/// The QEMU backend as the core trait sees it.
///
/// `RunBackend::plan` takes only `(&ResolvedProject, &BootMedium)`, so the
/// engine's resolved inputs are carried in this adapter's fields.
pub struct ResolvedQemuBackend<R: ProcessRunner = SystemRunner> {
    inner: QemuBackend<R>,
    inputs: PlanInputs,
    /// Optional symbolizer consulted for `run.fault.symbolicated`.
    symbols: Option<Box<SymbolizeFn<'static>>>,
}

impl<R: ProcessRunner> ResolvedQemuBackend<R> {
    pub fn new(inner: QemuBackend<R>, inputs: PlanInputs) -> Self {
        Self {
            inner,
            inputs,
            symbols: None,
        }
    }

    /// Attach a symbolizer (the engine wires B3's `source_line_for_address` in).
    pub fn with_symbolizer(mut self, symbols: Box<SymbolizeFn<'static>>) -> Self {
        self.symbols = Some(symbols);
        self
    }

    /// Launch with the plan the engine will actually run.
    pub fn launch_with(
        &self,
        plan: &RunPlan,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Result<RunOutcome> {
        self.inner
            .launch_plan(plan, events, cancel, self.symbols.as_deref())
    }
}

impl<R: ProcessRunner> RunBackend for ResolvedQemuBackend<R> {
    fn id(&self) -> &str {
        "qemu"
    }

    fn plan(&self, _project: &princess_core::ResolvedProject, medium: &BootMedium) -> Result<RunPlan> {
        self.inner.plan_for(&self.inputs, medium)
    }

    fn launch(
        &self,
        plan: &RunPlan,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Result<RunOutcome> {
        self.inner
            .launch_plan(plan, events, cancel, self.symbols.as_deref())
    }

    fn serial(&self, plan: &RunPlan) -> SerialCapture {
        plan.serial.clone()
    }

    fn stop(&self, op_id: &str) -> Result<()> {
        Err(PrincessError::new(
            ErrorCode::Internal,
            format!(
                "princess-run cannot stop op {op_id} by id alone: the caller must hold the \
                 CancelToken and the process group (contract §6 rule 3)"
            ),
        ))
    }

    fn classify_exit(&self, facts: &princess_core::ExitFacts) -> ExitReason {
        // The engine-wide implementation; never a second copy (D9).
        princess_core::classify_exit(facts)
    }
}

/// Convenience: plan a run for a project-shaped input set, defaulting the GDB
/// stub when `[debug]` asked for one.
pub fn plan_with_stub(
    inputs: &PlanInputs,
    medium: &BootMedium,
    options: &RunOptions,
) -> Result<RunPlan> {
    let mut plan_options = options.plan.clone();
    if plan_options.gdb_stub.is_none() {
        if let Some((host, port)) = &options.stub_defaults {
            plan_options.gdb_stub = Some(default_stub(host, *port, Default::default()));
        }
    }
    plan_run(inputs, medium, &plan_options)
}

/// Serializable summary of one run, for the CLI/report tooling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub reason: ExitReason,
    pub exit_code: Option<i32>,
    pub uptime_ms: u64,
    pub banner: Option<String>,
    pub faults: u64,
    pub rip_text: Option<String>,
    pub vector: Option<String>,
}

/// Extract a summary from a plan + outcome + the serial log it teed.
pub fn summarize(outcome: &RunOutcome, serial_log: Option<&std::path::Path>) -> RunSummary {
    let text = serial_log
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    let banner = crate::serial::banner_in_log(&text);
    let fault = outcome.fault.clone();
    RunSummary {
        reason: outcome.reason,
        exit_code: outcome.exit_code,
        uptime_ms: outcome.uptime_ms,
        banner,
        faults: u64::from(fault.is_some()),
        rip_text: fault.as_ref().map(|f| f.rip_text.clone()),
        vector: fault.map(|f| f.vector),
    }
}

/// Helper for the tests and the CLI: does this serial text prove the guest
/// reached its kernel?
pub fn serial_reached_kernel(serial_text: &str) -> bool {
    crate::serial::banner_in_log(serial_text).is_some()
}

/// The rip of the last fault in a captured log, if any.
pub fn last_fault_rip(serial_text: &str) -> Option<u64> {
    fault_rip_in_log(serial_text)
}

/// Kept so the module docs can point at the trait the CLI and Tauri shell use.
#[allow(dead_code)]
fn _plan_is_the_only_argv_source(_plan: &RunPlan, _boot: BootProtocol) {}

#[allow(dead_code)]
fn _encoding_is_part_of_every_chunk(_encoding: TextEncoding) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{FakeRunner, StreamChunk};
    use princess_core::event::{parse_ndjson, Event, EventBody, EventKind};
    use princess_core::types::TextEncoding;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    /// An in-memory event sink; the crate's only outbound channel is `EventSink`.
    #[derive(Default, Clone)]
    struct Spy {
        bodies: Arc<Mutex<Vec<EventBody>>>,
    }

    impl Spy {
        fn kinds(&self) -> Vec<EventKind> {
            self.bodies
                .lock()
                .unwrap()
                .iter()
                .map(EventBody::kind)
                .collect()
        }

        fn log_chunks(&self, stream: LogStream) -> String {
            self.bodies
                .lock()
                .unwrap()
                .iter()
                .filter_map(|body| match body {
                    EventBody::LogAppend(payload) if payload.stream == stream => {
                        Some(payload.chunk.clone())
                    }
                    _ => None,
                })
                .collect()
        }

        fn notes(&self) -> String {
            self.log_chunks(LogStream::Ide)
        }

        fn exited(&self) -> Option<princess_core::event::RunExitedPayload> {
            self.bodies.lock().unwrap().iter().find_map(|body| match body {
                EventBody::RunExited(payload) => Some(payload.clone()),
                _ => None,
            })
        }

        fn fault(&self) -> Option<princess_core::event::RunFaultPayload> {
            self.bodies.lock().unwrap().iter().find_map(|body| match body {
                EventBody::RunFault(payload) => Some(payload.clone()),
                _ => None,
            })
        }
    }

    impl EventSink for Spy {
        fn emit(&mut self, body: EventBody) -> Result<()> {
            self.bodies.lock().unwrap().push(body);
            Ok(())
        }
        fn op_id(&self) -> Option<&str> {
            Some("op-test")
        }
    }

    fn chunk(pipe: Pipe, text: &str) -> StreamChunk {
        StreamChunk {
            pipe,
            text: text.to_string(),
            encoding: TextEncoding::Utf8,
            at_eof: false,
        }
    }

    fn plan() -> RunPlan {
        crate::qemu::plan_run(
            &PlanInputs {
                root: PathBuf::from("/proj"),
                cwd: PathBuf::from("/proj"),
                args: Vec::new(),
                boot: BootProtocol::Multiboot2,
                timeout_ms: 15_000,
                serial_device: "com1".to_string(),
                serial_tee: None,
            },
            &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")),
            &PlanOptions {
                qemu_bin: PathBuf::from("/usr/bin/qemu-system-x86_64"),
                ..PlanOptions::default()
            },
        )
        .unwrap()
    }

    fn backend_with(chunks: Vec<StreamChunk>, outcome: ProcessOutcome) -> QemuBackend<FakeRunner> {
        // A non-empty emulator path: the plan builder refuses an empty one (that
        // is the real E_TOOLCHAIN_MISSING guard), and these tests are about the
        // event pipeline, not toolchain discovery.
        let options = RunOptions {
            plan: PlanOptions {
                qemu_bin: PathBuf::from("/usr/bin/qemu-system-x86_64"),
                ..PlanOptions::default()
            },
            ..RunOptions::default()
        };
        QemuBackend::with_runner(
            FakeRunner {
                chunks,
                outcome: Some(outcome),
                ..FakeRunner::default()
            },
            options,
        )
    }

    #[test]
    fn a_reference_run_emits_the_contract_event_sequence_and_attributes_shutdown() {
        let chunks = vec![
            chunk(Pipe::Serial, "PrincessIDE reference kernel booted\n"),
            chunk(Pipe::Serial, "[refkernel] EXCEPTION: vector=0x06 (#UD invalid opcode)\n"),
            chunk(
                Pipe::Serial,
                "[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 rflags=0x46 error=0x0\n",
            ),
            chunk(Pipe::Serial, "[refkernel] PANIC: unhandled CPU exception, halting\n"),
            chunk(Pipe::Emulator, "qemu: some emulator note\n"),
        ];
        let backend = backend_with(
            chunks,
            ProcessOutcome {
                exit_code: Some(0),
                group_dead: true,
                ..ProcessOutcome::default()
            },
        );
        let mut spy = Spy::default();
        let outcome = backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), None)
            .unwrap();

        assert_eq!(outcome.reason, ExitReason::GuestShutdown);
        // run.started first, run.exited last.
        let kinds = spy.kinds();
        assert_eq!(kinds.first(), Some(&EventKind::RunStarted));
        assert_eq!(kinds.last(), Some(&EventKind::RunExited));

        // P2-3: the banner arrives on serial.com1 as a log.append event.
        let serial = spy.log_chunks(LogStream::SerialCom1);
        assert!(serial.contains("PrincessIDE reference kernel booted"), "{serial}");
        // QEMU's own channel is separate, never merged (§6 rule 1).
        assert!(spy
            .log_chunks(LogStream::QemuMonitor)
            .contains("qemu: some emulator note"));

        // P2-4 shape: run.fault carries rip + verbatim ripText.
        let fault = spy.fault().unwrap();
        assert_eq!(fault.vector, "#UD");
        assert_eq!(fault.rip, 0x10_0b3d);
        assert_eq!(fault.rip_text, "0x0000000000100b3d");
        assert_eq!(fault.regs.get("CS").unwrap(), "0x0008");
        assert!(fault.symbolicated.is_none(), "no symbolizer was supplied");

        let notes = spy.notes();
        assert!(notes.contains("guest banner observed"), "{notes}");
        assert!(notes.contains("guest panic observed"), "{notes}");
        assert!(notes.contains("run ended: reason=guest-shutdown"), "{notes}");
    }

    #[test]
    fn a_symbolizer_decorates_run_fault() {
        let backend = backend_with(
            vec![
                chunk(Pipe::Serial, "PrincessIDE reference kernel booted\n"),
                chunk(
                    Pipe::Serial,
                    "[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 error=0x0\n",
                ),
            ],
            ProcessOutcome {
                exit_code: Some(0),
                group_dead: true,
                ..ProcessOutcome::default()
            },
        );
        let lookup = |rip: u64| {
            (rip == 0x10_0b3d).then(|| SymbolicatedLocation {
                symbol: "refkernel_fault_probe".into(),
                file: "/proj/kernel.c".into(),
                line: 100,
            })
        };
        let mut spy = Spy::default();
        backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), Some(&lookup))
            .unwrap();
        let fault = spy.fault().unwrap();
        let symbolicated = fault.symbolicated.unwrap();
        assert_eq!(symbolicated.symbol, "refkernel_fault_probe");
        assert_eq!(symbolicated.line, 100);
    }

    #[test]
    fn a_timeout_is_attributed_as_timeout_and_the_run_still_reports() {
        let backend = backend_with(
            vec![chunk(Pipe::Serial, "PrincessIDE paging kernel booted\n")],
            ProcessOutcome {
                exit_code: None,
                signal: Some(9),
                timed_out: true,
                group_dead: true,
                ..ProcessOutcome::default()
            },
        );
        let mut spy = Spy::default();
        let outcome = backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), None)
            .unwrap();
        assert_eq!(outcome.reason, ExitReason::Timeout);
        assert_eq!(spy.exited().unwrap().reason, ExitReason::Timeout);
        assert!(spy.notes().contains("reason=timeout"), "{}", spy.notes());
    }

    #[test]
    fn a_triple_fault_is_attributed_even_though_the_exit_code_is_zero() {
        let dir = std::env::temp_dir().join(format!("princesside-tfrun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("qemu-debug.log");
        std::fs::write(&log, "check_exception old: 0x8 new 0xd\nTriple fault\nCPU Reset (CPU 0)\n")
            .unwrap();

        let mut options = RunOptions {
            plan: PlanOptions {
                qemu_bin: PathBuf::from("/usr/bin/qemu-system-x86_64"),
                ..PlanOptions::default()
            },
            ..RunOptions::default()
        };
        options.plan.debug_log = Some(log.clone());
        let backend = QemuBackend::with_runner(
            FakeRunner {
                chunks: vec![chunk(Pipe::Serial, "PrincessIDE reference kernel booted\n")],
                outcome: Some(ProcessOutcome {
                    exit_code: Some(0),
                    group_dead: true,
                    ..ProcessOutcome::default()
                }),
                ..FakeRunner::default()
            },
            options,
        );
        let mut spy = Spy::default();
        let outcome = backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), None)
            .unwrap();
        assert_eq!(outcome.reason, ExitReason::TripleFault);
        assert!(spy.notes().contains("reason=triple-fault"), "{}", spy.notes());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cancel_is_attributed_as_killed() {
        let backend = backend_with(
            Vec::new(),
            ProcessOutcome {
                exit_code: None,
                signal: Some(15),
                cancelled: true,
                group_dead: true,
                ..ProcessOutcome::default()
            },
        );
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut spy = Spy::default();
        let outcome = backend.launch_plan(&plan(), &mut spy, &cancel, None).unwrap();
        assert_eq!(outcome.reason, ExitReason::Killed);
    }

    #[test]
    fn a_utf8_lossy_chunk_keeps_its_encoding_marker() {
        let mut bad = chunk(Pipe::Serial, "ok\u{FFFD}\n");
        bad.encoding = TextEncoding::Utf8Lossy;
        let backend = backend_with(
            vec![bad],
            ProcessOutcome {
                exit_code: Some(0),
                group_dead: true,
                ..ProcessOutcome::default()
            },
        );
        let mut spy = Spy::default();
        backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), None)
            .unwrap();
        let marked = spy.bodies.lock().unwrap().iter().any(|body| {
            matches!(body, EventBody::LogAppend(p) if p.encoding == TextEncoding::Utf8Lossy)
        });
        assert!(marked, "the lossy marker must survive to the event stream");
    }

    #[test]
    fn a_fault_storm_is_capped_and_the_suppression_is_reported() {
        let mut chunks = vec![chunk(Pipe::Serial, "PrincessIDE reference kernel booted\n")];
        for _ in 0..(MAX_FAULT_EVENTS + 5) {
            chunks.push(chunk(
                Pipe::Serial,
                "[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 error=0x0\n",
            ));
        }
        let backend = backend_with(
            chunks,
            ProcessOutcome {
                exit_code: Some(0),
                group_dead: true,
                ..ProcessOutcome::default()
            },
        );
        let mut spy = Spy::default();
        backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), None)
            .unwrap();
        let fault_events = spy
            .kinds()
            .iter()
            .filter(|kind| **kind == EventKind::RunFault)
            .count();
        assert_eq!(fault_events, MAX_FAULT_EVENTS);
        assert!(spy.notes().contains("suppressing further run.fault"), "{}", spy.notes());
        assert!(spy.notes().contains("suppressed=5"), "{}", spy.notes());
    }

    #[test]
    fn a_run_that_never_printed_a_banner_says_so() {
        let backend = backend_with(
            vec![chunk(Pipe::Emulator, "qemu: could not load the boot medium\n")],
            ProcessOutcome {
                exit_code: Some(1),
                group_dead: true,
                ..ProcessOutcome::default()
            },
        );
        let mut spy = Spy::default();
        let outcome = backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), None)
            .unwrap();
        assert_eq!(outcome.reason, ExitReason::Killed);
        assert!(spy.notes().contains("banner_seen=false"), "{}", spy.notes());
    }

    #[test]
    fn launch_defends_the_d9_argv_contract() {
        let mut bad = plan();
        bad.argv.push("-nographic".to_string());
        let backend = backend_with(Vec::new(), ProcessOutcome::default());
        let mut spy = Spy::default();
        let err = backend
            .launch_plan(&bad, &mut spy, &CancelToken::new(), None)
            .unwrap_err();
        assert!(err.message.contains("-nographic"), "{err}");
        // A plan rejected before startup must not emit run.started either.
        assert!(!spy.kinds().contains(&EventKind::RunStarted));
    }

    #[test]
    fn the_backend_is_object_safe_as_the_core_trait_requires() {
        let inputs = PlanInputs {
            root: PathBuf::from("/proj"),
            cwd: PathBuf::from("/proj"),
            args: Vec::new(),
            boot: BootProtocol::Multiboot2,
            timeout_ms: 15_000,
            serial_device: "com1".to_string(),
            serial_tee: None,
        };
        let backend: Box<dyn RunBackend> = Box::new(ResolvedQemuBackend::new(
            backend_with(Vec::new(), ProcessOutcome::default()),
            inputs,
        ));
        assert_eq!(backend.id(), "qemu");
        let project = princess_core::ProjectConfig::default().resolve("/proj");
        let planned = backend
            .plan(&project, &BootMedium::Iso(PathBuf::from("/proj/build/k.iso")))
            .unwrap();
        assert_eq!(backend.serial(&planned).device, "com1");

        // classify_exit goes through core's implementation.
        use princess_core::ExitFacts;
        assert_eq!(
            backend.classify_exit(&ExitFacts {
                timed_out: true,
                ..ExitFacts::default()
            }),
            ExitReason::Timeout
        );
    }

    #[test]
    fn stop_by_op_id_alone_is_an_explicit_error_not_a_silent_noop() {
        let inputs = PlanInputs {
            root: PathBuf::from("/proj"),
            cwd: PathBuf::from("/proj"),
            args: Vec::new(),
            boot: BootProtocol::Multiboot2,
            timeout_ms: 15_000,
            serial_device: "com1".to_string(),
            serial_tee: None,
        };
        let backend = ResolvedQemuBackend::new(
            backend_with(Vec::new(), ProcessOutcome::default()),
            inputs,
        );
        let err = backend.stop("op-1234").unwrap_err();
        assert!(err.message.contains("op-1234"), "{err}");
    }

    #[test]
    fn debug_log_can_be_recovered_from_an_argv() {
        let argv: Vec<String> = ["qemu", "-d", "int,cpu_reset", "-D", "/tmp/q.log"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(debug_log_from_argv(&argv), Some(PathBuf::from("/tmp/q.log")));
        assert_eq!(debug_log_from_argv(&["qemu".to_string()]), None);
    }

    /// The stream produced by a run must satisfy the *contract's* own parser —
    /// this is the P2-8-adjacent guarantee that the payloads are well-formed.
    #[test]
    fn the_emitted_bodies_round_trip_through_the_contract_event_model() {
        let backend = backend_with(
            vec![
                chunk(Pipe::Serial, "PrincessIDE reference kernel booted\n"),
                chunk(
                    Pipe::Serial,
                    "[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 error=0x0\n",
                ),
            ],
            ProcessOutcome {
                exit_code: Some(0),
                group_dead: true,
                ..ProcessOutcome::default()
            },
        );
        let mut spy = Spy::default();
        backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), None)
            .unwrap();

        // Re-serialise every body through the frozen envelope and parse it back.
        let bodies = spy.bodies.lock().unwrap().clone();
        assert!(!bodies.is_empty());
        let mut ndjson = String::new();
        for (index, body) in bodies.into_iter().enumerate() {
            let event: Event = Event::new(
                index as u64 + 1,
                chrono_like_timestamp(),
                Some("op-test".into()),
                body,
            );
            ndjson.push_str(&event.to_ndjson().unwrap());
        }
        let parsed = parse_ndjson(&ndjson).unwrap();
        princess_core::validate_stream(&parsed).unwrap();
        assert!(parsed.iter().any(|e| e.kind() == EventKind::RunFault));
        assert_eq!(parsed.last().unwrap().kind(), EventKind::RunExited);
    }

    /// `chrono` is not a dependency of this crate; build a fixed UTC timestamp
    /// through the contract's own type.
    fn chrono_like_timestamp() -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc.with_ymd_and_hms(2026, 5, 5, 12, 0, 0).unwrap()
    }

    #[test]
    fn summarize_reads_the_banner_and_fault_from_the_serial_log() {
        let dir = std::env::temp_dir().join(format!("princesside-sum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("serial.log");
        std::fs::write(
            &log,
            "PrincessIDE reference kernel booted\n[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008\n",
        )
        .unwrap();

        let backend = backend_with(
            vec![
                chunk(Pipe::Serial, "PrincessIDE reference kernel booted\n"),
                chunk(
                    Pipe::Serial,
                    "[refkernel] EXCEPTION: vector=0x06 (#UD invalid opcode)\n",
                ),
                chunk(
                    Pipe::Serial,
                    "[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 rflags=0x46 error=0x0\n",
                ),
            ],
            ProcessOutcome {
                exit_code: Some(0),
                group_dead: true,
                ..ProcessOutcome::default()
            },
        );
        let mut spy = Spy::default();
        let outcome = backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), None)
            .unwrap();
        let summary = summarize(&outcome, Some(&log));
        assert_eq!(summary.banner.as_deref(), Some("PrincessIDE reference kernel booted"));
        assert_eq!(summary.rip_text.as_deref(), Some("0x0000000000100b3d"));
        assert_eq!(summary.vector.as_deref(), Some("#UD"));
        assert!(serial_reached_kernel(&std::fs::read_to_string(&log).unwrap()));
        assert_eq!(last_fault_rip(&std::fs::read_to_string(&log).unwrap()), Some(0x10_0b3d));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_overrides_reach_the_spawn_spec() {
        let mut env = BTreeMap::new();
        env.insert("PRINCESSIDE_TEST".to_string(), "1".to_string());
        let options = RunOptions {
            env,
            plan: PlanOptions {
                qemu_bin: PathBuf::from("/usr/bin/qemu-system-x86_64"),
                ..PlanOptions::default()
            },
            ..RunOptions::default()
        };
        let backend = QemuBackend::with_runner(
            FakeRunner {
                outcome: Some(ProcessOutcome {
                    exit_code: Some(0),
                    group_dead: true,
                    ..ProcessOutcome::default()
                }),
                ..FakeRunner::default()
            },
            options,
        );
        let mut spy = Spy::default();
        backend
            .launch_plan(&plan(), &mut spy, &CancelToken::new(), None)
            .unwrap();
        let seen = backend.runner.seen.lock().unwrap();
        assert_eq!(seen[0].env.get("PRINCESSIDE_TEST").map(String::as_str), Some("1"));
        assert_eq!(seen[0].timeout, Some(std::time::Duration::from_millis(15_000)));
    }
}
