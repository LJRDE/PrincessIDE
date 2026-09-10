//! Engine extension points — `docs/spec/10-contracts.md` §5.
//!
//! **v1 deliberately has no plugin loader**: no dynamic libraries, no script
//! plugins, no marketplace.  These traits exist so the engine's own backends are
//! interchangeable and so the boundaries between them are written down, and they
//! are used by **static dispatch** (`Box<dyn Trait>` inside the engine, a
//! concrete type chosen from `princess.toml`).  User extensibility in v1 is
//! `backend = "custom"` + `command` only.
//!
//! The method *names* are fixed by the contract:
//!
//! | trait | contract methods |
//! |---|---|
//! | [`BuildBackend`] | `detect` / `plan` / `execute` / `cancel` |
//! | [`RunBackend`] | `plan` / `launch` / `serial` / `stop` / `classify_exit` |
//! | [`DebugBackend`] | DAP capability subset: breakpoints, stepping, registers, memory, disassembly |
//! | [`BinProvider`] | ELF sections/symbols/source lines/disassembly |
//! | [`AiProvider`] | OpenAI-compatible streaming `chat/completions` |
//!
//! The signatures are the engine's v1 shape; the Rust `continue` keyword is
//! spelled `continue_`, and everything that would block an async runtime is left
//! to the implementation (the traits themselves carry no runtime types, so
//! `princess-core` stays dependency-light and usable from a test harness).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::{BootProtocol, BuildBackendKind, ResolvedProject};
use crate::error::{PrincessError, Result};
use crate::event::{
    BuildFinishedPayload, BuildStartedPayload, DebugStoppedPayload, EventBody, EventRecorder,
    LogAppendPayload, RunExitedPayload, RunFaultPayload,
};
use crate::types::{
    AiUsage, BreakpointSpec, ChatRequest, DisassembledInstruction, GdbStub, LogStream, RegisterFile,
    Scope, SectionInfo, SerialCapture, StackFrame, SymbolInfo, SymbolicatedLocation, TextEncoding,
    ToolchainReport, Variable,
};

/// Anything that can accept engine events: the event recorder on the wire, a
/// replay buffer, or a test spy.
pub trait EventSink {
    /// Append one event.
    fn emit(&mut self, body: EventBody) -> Result<()>;

    /// The long-operation id the sink stamps on events (`None` when unrelated).
    fn op_id(&self) -> Option<&str>;

    /// Convenience: append a text chunk to a stream.
    fn log(&mut self, stream: LogStream, chunk: &str) -> Result<()> {
        self.emit(EventBody::LogAppend(LogAppendPayload {
            stream,
            chunk: chunk.to_string(),
            encoding: TextEncoding::Utf8,
        }))
    }

    /// Convenience: an `ide` line.  The engine guarantees no UTF-8 code point is
    /// split, so callers must already own whole strings.
    fn note(&mut self, text: &str) -> Result<()> {
        let mut text = text.to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        self.log(LogStream::Ide, &text)
    }
}

impl<W: std::io::Write> EventSink for EventRecorder<W> {
    fn emit(&mut self, body: EventBody) -> Result<()> {
        // Fully qualified on purpose: `impl EventSink::emit` must not recurse.
        EventRecorder::emit(self, body).map(|_| ())
    }

    fn op_id(&self) -> Option<&str> {
        EventRecorder::op_id(self)
    }
}

/// Cooperative cancellation for one long operation.
///
/// The engine kills process *groups* on cancel (§6 rule 3); this token is the
/// in-process half of the same signal, so a backend loop notices that the
/// operation is over instead of waiting for its child.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.  Idempotent and callable from any thread.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// `Err(E_CANCELLED)` when cancellation was requested.
    pub fn check(&self, op_id: &str) -> Result<()> {
        if self.is_cancelled() {
            return Err(PrincessError::cancelled(op_id));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- build ------

/// A fully resolved build invocation: everything needed to run it, with no
/// further decisions left to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPlan {
    /// Which backend produced this plan.
    pub backend: BuildBackendKind,
    /// Identifier of the resolved toolchain (for the `build.started` event).
    pub toolchain_id: String,
    /// argv as executed, `argv[0]` first.
    pub argv: Vec<String>,
    /// Absolute working directory.
    pub cwd: PathBuf,
    /// Extra environment for the child (on top of the inherited environment).
    pub env: BTreeMap<String, String>,
    /// Artifacts the manifest declared; empty means "discover".
    pub declared_artifacts: Vec<PathBuf>,
    /// Optional backend-level deadline.
    pub timeout_ms: Option<u64>,
}

/// Fields the engine puts into `build.started` (contract §2) — derived from a
/// [`BuildPlan`] so the event can never disagree with what was executed.
impl BuildPlan {
    pub fn started_event(&self) -> BuildStartedPayload {
        BuildStartedPayload {
            backend: self.backend.as_str().to_string(),
            toolchain_id: self.toolchain_id.clone(),
            argv: self.argv.clone(),
            cwd: self.cwd.to_string_lossy().into_owned(),
        }
    }
}

/// Facts a backend reports about its own toolchain.
pub use crate::types::ToolInfo;

/// Build backend (`[build] backend`).
pub trait BuildBackend: Send + Sync {
    /// Stable identifier, e.g. `make`.
    fn id(&self) -> &str;

    /// `detect()` — which tools this backend needs, and whether they resolve.
    fn detect(&self, project: &ResolvedProject) -> Result<ToolchainReport>;

    /// `plan()` — turn the manifest into an exact invocation.
    fn plan(&self, project: &ResolvedProject) -> Result<BuildPlan>;

    /// `execute()` — run it, streaming `log.append` / `build.diagnostic` events
    /// and returning the `build.finished` payload.
    ///
    /// Must emit events for both output streams separately (§6 rule 1) and must
    /// return a payload even when the child dies (`*.finished` is guaranteed,
    /// §6 rule 5).
    fn execute(
        &self,
        plan: &BuildPlan,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Result<BuildFinishedPayload>;

    /// `cancel()` — terminate the running build's whole process group.
    fn cancel(&self, op_id: &str) -> Result<()>;
}

// ------------------------------------------------------------------ run ------

/// What the engine decided to boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootMedium {
    /// A GRUB ISO (`-cdrom <iso> -boot d`); the only path that boots a 64-bit
    /// multiboot2 kernel under QEMU.
    Iso(PathBuf),
    /// A directly loadable kernel image (`-kernel <elf>`); QEMU only accepts
    /// 32-bit images on this path.
    Kernel(PathBuf),
    /// A raw/floppy/disk image.
    Disk(PathBuf),
}

impl BootMedium {
    pub fn path(&self) -> &Path {
        match self {
            BootMedium::Iso(p) | BootMedium::Kernel(p) | BootMedium::Disk(p) => p,
        }
    }
}

/// A fully resolved emulator invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    /// Backend id (`qemu`).
    pub backend: String,
    /// Exact argv, `argv[0]` first.
    pub argv: Vec<String>,
    /// Absolute working directory.
    pub cwd: PathBuf,
    /// Boot medium the engine chose.
    pub boot_medium: BootMedium,
    /// Boot protocol from the manifest (informational for the front end).
    pub boot: BootProtocol,
    /// Run deadline; the engine kills the process group when it expires.
    pub timeout_ms: u64,
    /// Serial capture configuration.
    pub serial: SerialCapture,
    /// GDB stub the run exposes, when `-s`/`-g` style debugging was requested.
    pub gdb_stub: Option<GdbStub>,
}

/// Exit facts handed to `classify_exit` — no backend may guess a reason that is
/// not derivable from these (contract §6 rule 4).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExitFacts {
    /// Process exit code, `None` if it died from a signal.
    pub exit_code: Option<i32>,
    /// Terminating signal, when the emulator was killed.
    pub signal: Option<i32>,
    /// The engine hit `timeout_ms`.
    pub timed_out: bool,
    /// Cancellation was requested (`run:stop` / `op:cancel`).
    pub cancelled: bool,
    /// The guest asked for a clean power-off (ACPI shutdown).
    pub guest_powered_off: bool,
    /// A CPU reset with no recovery path was observed.
    pub triple_fault: bool,
}

/// Outcome of one run, mapped 1:1 onto `run.exited` (+ the optional fault).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub exit_code: Option<i32>,
    pub reason: crate::types::ExitReason,
    pub uptime_ms: u64,
    /// The last CPU fault observed on the serial line, if any.
    pub fault: Option<RunFaultPayload>,
    /// Where the serial log was teed, when it was.
    pub serial_log: Option<PathBuf>,
}

impl RunOutcome {
    pub fn exited_event(&self) -> RunExitedPayload {
        RunExitedPayload {
            exit_code: self.exit_code,
            reason: self.reason,
            uptime_ms: self.uptime_ms,
        }
    }
}

/// Run backend (`[run] backend = "qemu"`).
pub trait RunBackend: Send + Sync {
    /// Stable identifier, e.g. `qemu`.
    fn id(&self) -> &str;

    /// `plan()` — build the exact argv from the manifest and the boot medium.
    fn plan(&self, project: &ResolvedProject, medium: &BootMedium) -> Result<RunPlan>;

    /// `launch()` — start the machine, stream serial/monitor events, enforce the
    /// deadline, kill the process group, and classify the exit.
    fn launch(
        &self,
        plan: &RunPlan,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Result<RunOutcome>;

    /// `serial()` — the capture configuration (and tee target) for a plan.
    fn serial(&self, plan: &RunPlan) -> SerialCapture;

    /// `stop()` — terminate the machine's process group.
    fn stop(&self, op_id: &str) -> Result<()>;

    /// `classify_exit()` — the single place the engine attributes `reason`.
    fn classify_exit(&self, facts: &ExitFacts) -> crate::types::ExitReason;
}

/// The engine-wide exit attribution rule, shared by every run backend so the
/// answer cannot drift between them.
///
/// Precedence, most specific first: cancellation, deadline, guest power-off,
/// triple fault, unclean death, then a plain exit code.
pub fn classify_exit(facts: &ExitFacts) -> crate::types::ExitReason {
    use crate::types::ExitReason;
    if facts.cancelled {
        return ExitReason::Killed;
    }
    if facts.timed_out {
        return ExitReason::Timeout;
    }
    if facts.triple_fault {
        return ExitReason::TripleFault;
    }
    if facts.guest_powered_off {
        return ExitReason::GuestShutdown;
    }
    match facts.exit_code {
        // A clean emulator exit with no shutdown evidence is still attributed to
        // the guest asking the machine to stop (ACPI shutdown is the only path
        // that makes QEMU exit 0 without `-no-shutdown`).
        Some(0) => ExitReason::GuestShutdown,
        Some(_) => ExitReason::Killed,
        None => ExitReason::Killed,
    }
}

// ---------------------------------------------------------------- debug ------

/// DAP-style capability set a [`DebugBackend`] reports honestly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DebugCapabilities {
    pub supports_conditional_breakpoints: bool,
    pub supports_disassemble_request: bool,
    pub supports_read_memory: bool,
    pub supports_write_memory: bool,
    pub supports_restart: bool,
    pub supports_terminate: bool,
    pub supports_set_variable: bool,
}

impl Default for DebugCapabilities {
    fn default() -> Self {
        Self {
            supports_conditional_breakpoints: true,
            supports_disassemble_request: true,
            supports_read_memory: true,
            supports_write_memory: true,
            supports_restart: true,
            supports_terminate: true,
            supports_set_variable: false,
        }
    }
}

/// Debug backend (`[debug] backend = "gdb"` — adapted to the DAP subset the UI
/// speaks; see P1-C for the transport decision).
pub trait DebugBackend: Send + Sync {
    fn id(&self) -> &str;

    /// What this backend can actually do; the UI must not offer more.
    fn capabilities(&self) -> DebugCapabilities;

    /// Connect to (or launch behind) a stub.
    fn attach(&mut self, stub: &GdbStub, events: &mut dyn EventSink) -> Result<()>;

    /// Release the target without killing it.
    fn detach(&mut self) -> Result<()>;

    /// Replace the breakpoints of one source file; returns them verified (or
    /// not) as the backend found them.
    fn set_breakpoints(
        &mut self,
        file: &Path,
        breakpoints: &[BreakpointSpec],
        events: &mut dyn EventSink,
    ) -> Result<Vec<BreakpointSpec>>;

    fn continue_(
        &mut self,
        thread_id: Option<i64>,
        events: &mut dyn EventSink,
    ) -> Result<DebugStoppedPayload>;

    fn step_over(
        &mut self,
        thread_id: Option<i64>,
        events: &mut dyn EventSink,
    ) -> Result<DebugStoppedPayload>;

    fn step_into(
        &mut self,
        thread_id: Option<i64>,
        events: &mut dyn EventSink,
    ) -> Result<DebugStoppedPayload>;

    fn stack_trace(&mut self, thread_id: i64, start_frame: u64, levels: u64)
        -> Result<Vec<StackFrame>>;

    fn scopes(&mut self, frame_id: u64) -> Result<Vec<Scope>>;

    fn variables(&mut self, variables_reference: i64) -> Result<Vec<Variable>>;

    fn read_memory(&mut self, address: u64, byte_count: u64) -> Result<Vec<u8>>;

    fn write_memory(&mut self, address: u64, bytes: &[u8]) -> Result<()>;

    fn disassemble(&mut self, address: u64, count: u64) -> Result<Vec<DisassembledInstruction>>;

    fn registers(&mut self, thread_id: i64) -> Result<RegisterFile>;
}

// ------------------------------------------------------------------ bin ------

/// What a [`BinProvider`] learned when it opened an artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryFacts {
    pub path: PathBuf,
    /// `elf32` | `elf64` | ...
    pub format: String,
    pub entry_point: u64,
    /// GNU build-id, when the image has one.
    pub build_id: Option<String>,
}

/// Binary inspection provider (P5: hex / disassembly / sections / page tables).
pub trait BinProvider: Send + Sync {
    /// Open an artifact and read its identifying facts.
    fn open(&mut self, artifact: &Path) -> Result<BinaryFacts>;

    /// Sections and segments.
    fn sections(&self) -> Result<Vec<SectionInfo>>;

    /// Static symbol table.
    fn symbols(&self) -> Result<Vec<SymbolInfo>>;

    /// Source line + function for an address, from DWARF.
    fn source_line_for_address(&self, address: u64) -> Result<Option<SymbolicatedLocation>>;

    /// Disassemble `count` instructions starting at `address`.
    fn disassemble(&self, address: u64, count: u64) -> Result<Vec<DisassembledInstruction>>;
}

// ------------------------------------------------------------------- ai ------

/// AI provider (contract §5): OpenAI-compatible `chat/completions`, streamed.
///
/// Streaming is expressed by emitting `ai.chunk` events through `events` and
/// returning the usage for `ai.finished`; there is no async in this signature so
/// `princess-core` stays runtime-agnostic.  An unavailable provider **must**
/// answer `E_AI_UNAVAILABLE` and must not block any other IDE feature (P7-2).
pub trait AiProvider: Send + Sync {
    fn id(&self) -> &str;

    /// Stream a completion.  Returns token usage for `ai.finished`.
    fn chat_completions(
        &self,
        request: &ChatRequest,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Result<AiUsage>;

    /// Abort an in-flight request by id.
    fn cancel(&self, request_id: &str) -> Result<()>;
}

// ----------------------------------------------------------------- tests -----

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;
    use crate::types::{Artifact, BuildStatus, ExitReason, ToolInfo};

    /// Collects events in memory; proves `EventSink` is usable as a trait object.
    #[derive(Default)]
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
    fn event_sink_default_methods_work_through_a_trait_object() {
        let mut spy = Spy::default();
        let sink: &mut dyn EventSink = &mut spy;
        sink.log(LogStream::Build, "make: nothing to be done\n").unwrap();
        sink.note("no trailing newline").unwrap();
        assert_eq!(spy.bodies.len(), 2);
        match &spy.bodies[1] {
            EventBody::LogAppend(payload) => {
                assert_eq!(payload.stream, LogStream::Ide);
                assert_eq!(payload.chunk, "no trailing newline\n");
            }
            other => panic!("unexpected body {other:?}"),
        }
    }

    #[test]
    fn event_recorder_is_an_event_sink_without_recursing() {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut recorder = EventRecorder::new(&mut out);
            let sink: &mut dyn EventSink = &mut recorder;
            sink.emit(EventBody::LogAppend(LogAppendPayload {
                stream: LogStream::Build,
                chunk: "hello\n".into(),
                encoding: TextEncoding::Utf8,
            }))
            .unwrap();
            assert_eq!(sink.op_id(), None);
        }
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains(r#""seq":1"#), "{text}");
        assert!(text.contains(r#""kind":"log.append""#), "{text}");
    }

    #[test]
    fn cancel_token_is_shared_and_checked() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(!token.is_cancelled());
        assert!(token.check("op-1").is_ok());
        clone.cancel();
        assert!(token.is_cancelled());
        let err = token.check("op-1").unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::Cancelled);
        assert!(err.message.contains("op-1"));
    }

    /// The engine holds backends as `Box<dyn ...>`; a trait that stops being
    /// object safe would break the next batch silently, so pin it here.
    #[test]
    fn every_backend_trait_is_object_safe() {
        struct StubBuild;
        impl BuildBackend for StubBuild {
            fn id(&self) -> &str {
                "stub"
            }
            fn detect(&self, _project: &ResolvedProject) -> Result<ToolchainReport> {
                Ok(ToolchainReport {
                    tools: vec![ToolInfo {
                        name: "gcc".into(),
                        path: Some("/usr/bin/gcc".into()),
                        version: Some("gcc 12".into()),
                        available: true,
                        required: true,
                    }],
                    ok: true,
                })
            }
            fn plan(&self, project: &ResolvedProject) -> Result<BuildPlan> {
                Ok(BuildPlan {
                    backend: BuildBackendKind::Make,
                    toolchain_id: "host-gcc".into(),
                    argv: vec!["make".into()],
                    cwd: project.build_cwd.clone(),
                    env: BTreeMap::new(),
                    declared_artifacts: project.declared_artifacts.clone(),
                    timeout_ms: None,
                })
            }
            fn execute(
                &self,
                _plan: &BuildPlan,
                events: &mut dyn EventSink,
                cancel: &CancelToken,
            ) -> Result<BuildFinishedPayload> {
                cancel.check("op")?;
                events.note("stub build")?;
                Ok(BuildFinishedPayload {
                    status: BuildStatus::Ok,
                    exit_code: Some(0),
                    duration_ms: 1,
                    artifacts: Vec::<Artifact>::new(),
                })
            }
            fn cancel(&self, _op_id: &str) -> Result<()> {
                Ok(())
            }
        }

        struct StubRun;
        impl RunBackend for StubRun {
            fn id(&self) -> &str {
                "stub"
            }
            fn plan(&self, project: &ResolvedProject, medium: &BootMedium) -> Result<RunPlan> {
                Ok(RunPlan {
                    backend: "qemu".into(),
                    argv: vec!["qemu-system-x86_64".into()],
                    cwd: project.build_cwd.clone(),
                    boot_medium: medium.clone(),
                    boot: project.config.run.boot,
                    timeout_ms: project.config.run.timeout_ms,
                    serial: self.serial(&RunPlan {
                        backend: "qemu".into(),
                        argv: Vec::new(),
                        cwd: project.build_cwd.clone(),
                        boot_medium: medium.clone(),
                        boot: project.config.run.boot,
                        timeout_ms: project.config.run.timeout_ms,
                        serial: SerialCapture::default(),
                        gdb_stub: None,
                    }),
                    gdb_stub: None,
                })
            }
            fn launch(
                &self,
                _plan: &RunPlan,
                events: &mut dyn EventSink,
                cancel: &CancelToken,
            ) -> Result<RunOutcome> {
                cancel.check("op")?;
                events.log(LogStream::SerialCom1, "PrincessIDE reference kernel booted\n")?;
                let facts = ExitFacts {
                    timed_out: true,
                    ..ExitFacts::default()
                };
                Ok(RunOutcome {
                    exit_code: None,
                    reason: self.classify_exit(&facts),
                    uptime_ms: 1,
                    fault: None,
                    serial_log: None,
                })
            }
            fn serial(&self, plan: &RunPlan) -> SerialCapture {
                plan.serial.clone()
            }
            fn stop(&self, _op_id: &str) -> Result<()> {
                Ok(())
            }
            fn classify_exit(&self, facts: &ExitFacts) -> ExitReason {
                classify_exit(facts)
            }
        }

        let build: Box<dyn BuildBackend> = Box::new(StubBuild);
        let run: Box<dyn RunBackend> = Box::new(StubRun);
        assert_eq!(build.id(), "stub");
        assert_eq!(run.id(), "stub");

        let project = crate::config::ProjectConfig::default().resolve("/tmp/p2-traits");
        let plan = build.plan(&project).unwrap();
        assert_eq!(plan.argv, vec!["make".to_string()]);
        assert_eq!(plan.started_event().backend, "make");

        let mut spy = Spy::default();
        let payload = build.execute(&plan, &mut spy, &CancelToken::new()).unwrap();
        assert_eq!(payload.status, BuildStatus::Ok);
        assert_eq!(spy.bodies.len(), 1);

        let run_plan = run
            .plan(&project, &BootMedium::Kernel("/tmp/k.elf".into()))
            .unwrap();
        assert_eq!(run.serial(&run_plan).device, "com1");

        let mut spy = Spy::default();
        let outcome = run.launch(&run_plan, &mut spy, &CancelToken::new()).unwrap();
        assert_eq!(outcome.reason, ExitReason::Timeout);
        assert_eq!(outcome.exited_event().reason, ExitReason::Timeout);
        assert_eq!(spy.bodies[0].kind(), EventKind::LogAppend);

        // The other three traits only need to be nameable as trait objects.
        fn assert_object_safe<T: ?Sized>() {}
        assert_object_safe::<dyn DebugBackend>();
        assert_object_safe::<dyn BinProvider>();
        assert_object_safe::<dyn AiProvider>();
    }

    #[test]
    fn exit_attribution_precedence_is_fixed() {
        let cancelled = ExitFacts {
            cancelled: true,
            timed_out: true,
            guest_powered_off: true,
            triple_fault: true,
            exit_code: Some(0),
            ..ExitFacts::default()
        };
        assert_eq!(classify_exit(&cancelled), ExitReason::Killed);

        let timed_out = ExitFacts {
            timed_out: true,
            ..ExitFacts::default()
        };
        assert_eq!(classify_exit(&timed_out), ExitReason::Timeout);

        let triple = ExitFacts {
            triple_fault: true,
            exit_code: Some(0),
            ..ExitFacts::default()
        };
        assert_eq!(classify_exit(&triple), ExitReason::TripleFault);

        let shutdown = ExitFacts {
            guest_powered_off: true,
            exit_code: Some(0),
            ..ExitFacts::default()
        };
        assert_eq!(classify_exit(&shutdown), ExitReason::GuestShutdown);

        let clean = ExitFacts {
            exit_code: Some(0),
            ..ExitFacts::default()
        };
        assert_eq!(classify_exit(&clean), ExitReason::GuestShutdown);

        let signal = ExitFacts {
            signal: Some(9),
            exit_code: None,
            ..ExitFacts::default()
        };
        assert_eq!(classify_exit(&signal), ExitReason::Killed);

        let nonzero = ExitFacts {
            exit_code: Some(1),
            ..ExitFacts::default()
        };
        assert_eq!(classify_exit(&nonzero), ExitReason::Killed);
    }
}
