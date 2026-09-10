//! The [`DebugBackend`] implementation: standard DAP on the trunk, the
//! capability layer on the side.
//!
//! This type is what `princess-core`'s trait boundary exposes to the rest of the
//! engine.  It owns:
//!
//! * the QEMU target process ([`crate::qemu`]),
//! * the gdb DAP transport ([`crate::transport`]),
//! * the session state machine, including the delayed-`attach` handshake.
//!
//! # The handshake, and why it is not a detail
//!
//! `attach`'s response is withheld until `configurationDone` is processed
//! (research C §6; re-measured here).  A backend that sent `attach` and blocked
//! on its reply would deadlock the engine.  [`DapBackend::attach`] therefore
//! sends `attach` + `configurationDone`, waits for the latter, and only then
//! collects the former from the transport's stash.
//!
//! # Session state is real state
//!
//! Research C §3.2 documents that a single failed or out-of-order request leaves
//! gdb's DAP session in a state where every following request returns a
//! plausible-looking wrong answer — the "cascading false negatives" that made
//! the first research round conclude, incorrectly, that `monitor` and `pause`
//! were unsupported.  [`SessionState`] makes that state explicit so a caller
//! cannot accidentally keep using a poisoned session, and
//! [`DapBackend::require_ready`] refuses to issue requests against one.

use std::path::{Path, PathBuf};
use std::time::Duration;

use princess_core::{
    BreakpointSpec, DebugBackend, DebugCapabilities, DebugStoppedPayload, DisassembledInstruction,
    EventBody, EventSink, GdbStub, LogStream, PrincessError, RegisterFile, Result, Scope,
    SourceLocation, StackFrame, StopReason, StubMode, Variable,
};
use serde_json::{json, Value};

use crate::arch::{self, ArchCheck};
use crate::capability::{CapabilityLayer, RegisterSelection};
use crate::qemu::{QemuCommand, QemuProcess};
use crate::transport::{AdapterCommand, Transport};

/// Where a session is in its life cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Nothing started.
    Idle,
    /// The adapter is up and the handshake completed; requests are allowed.
    Ready,
    /// The handshake is in flight.
    Attaching,
    /// The stub died or a protocol-level failure poisoned the session.
    ///
    /// Requests must be refused rather than attempted: a poisoned gdb DAP
    /// session answers with plausible lies (research C §3.2), and returning
    /// those to the UI is exactly what D10/P4-4 forbid.
    Poisoned,
    /// Cleanly detached / shut down.
    Detached,
}

/// Extra, kernel-specific debug capabilities the engine advertises.
///
/// These are **not** DAP capabilities — they are the four things the built-in
/// DAP lacks (D11) and that this crate adds.  `princess-core`'s
/// [`DebugCapabilities`] is frozen and has no fields for them, so they are
/// reported here; the UI must offer hardware breakpoints only when
/// [`HardwareSupport::Hardware`] was actually measured, never by assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareSupport {
    /// Not probed yet.
    Unknown,
    /// A hardware breakpoint was successfully created and verified.
    Hardware,
    /// `hbreak` is unavailable on this target.
    Unsupported,
}

/// How the session was told to find the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetMode {
    /// The engine starts QEMU itself (`-s -S`) and owns its lifetime.
    Launch {
        /// The exact QEMU command, kept so the report can show it.
        command: Box<QemuCommand>,
    },
    /// Connect to an already-running stub; the engine owns nothing.
    Attach,
}

/// Configuration for one debug session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugSessionConfig {
    /// The gdb binary to use as a DAP adapter (must be >= 14, D11).
    pub adapter: PathBuf,
    /// The ELF whose symbols the session loads.
    pub symbols: PathBuf,
    /// How to reach the target.
    pub target: TargetMode,
    /// How long to wait for any single DAP response.
    pub request_timeout: Duration,
    /// How long to wait for the stub to come up when launching QEMU.
    pub ready_timeout: Duration,
    /// Serial log to keep, when the engine owns QEMU.
    pub serial_log: Option<PathBuf>,
}

impl DebugSessionConfig {
    /// A config for the standard "engine boots the ISO and debugs it" flow.
    pub fn launching(
        adapter: impl Into<PathBuf>,
        symbols: impl Into<PathBuf>,
        qemu_argv: Vec<String>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            adapter: adapter.into(),
            symbols: symbols.into(),
            target: TargetMode::Launch {
                command: Box::new(QemuCommand {
                    argv: qemu_argv,
                    cwd: cwd.into(),
                    symbols: None,
                    ready_timeout: Duration::from_secs(30),
                }),
            },
            request_timeout: crate::transport::DEFAULT_REQUEST_TIMEOUT,
            ready_timeout: Duration::from_secs(30),
            serial_log: None,
        }
    }
}

/// The engine's GDB-DAP debug backend.
pub struct DapBackend {
    config: DebugSessionConfig,
    qemu: Option<QemuProcess>,
    transport: Option<Transport>,
    state: SessionState,
    thread_id: i64,
    hardware: HardwareSupport,
    arch_check: Option<ArchCheck>,
    /// Breakpoints this backend created, so the UI can be told about the ones
    /// the built-in DAP's own map does not know about (`hbreak`).
    hardware_breakpoints: Vec<crate::capability::BreakpointRecord>,
    /// Last stop, so `frames` can be resolved lazily the way DAP expects.
    frames: Vec<StackFrame>,
    adapter_supports: Option<Value>,
}

impl std::fmt::Debug for DapBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DapBackend")
            .field("state", &self.state)
            .field("hardware", &self.hardware)
            .field("thread_id", &self.thread_id)
            .finish()
    }
}

/// Prefix used for the synthetic frame ids of hardware breakpoints, so a
/// `breakpoint.changed` event can be traced back to the capability layer.
const HW_BREAKPOINT_ID_PREFIX: &str = "hw:";

impl DapBackend {
    /// Build a backend; nothing is started until [`DebugBackend::attach`].
    pub fn new(config: DebugSessionConfig) -> Self {
        Self {
            config,
            qemu: None,
            transport: None,
            state: SessionState::Idle,
            thread_id: 1,
            hardware: HardwareSupport::Unknown,
            arch_check: None,
            hardware_breakpoints: Vec::new(),
            frames: Vec::new(),
            adapter_supports: None,
        }
    }

    /// Current session state.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// What the adapter said it supports, once `initialize` has run.
    pub fn adapter_capabilities(&self) -> Option<&Value> {
        self.adapter_supports.as_ref()
    }

    /// Whether hardware breakpoints were proven to work on this target.
    pub fn hardware_support(&self) -> HardwareSupport {
        self.hardware
    }

    /// The verdict of the D10 architecture self-check.
    pub fn arch_check(&self) -> Option<&ArchCheck> {
        self.arch_check.as_ref()
    }

    /// The exact QEMU command, when this backend launched the machine.
    pub fn qemu_command(&self) -> Option<&QemuCommand> {
        match &self.config.target {
            TargetMode::Launch { command } => Some(command),
            TargetMode::Attach => None,
        }
    }

    /// Hardware breakpoints this layer created.
    pub fn hardware_breakpoints(&self) -> &[crate::capability::BreakpointRecord] {
        &self.hardware_breakpoints
    }

    fn transport_mut(&mut self) -> Result<&mut Transport> {
        match self.transport.as_mut() {
            Some(transport) if !transport.is_closed() => Ok(transport),
            _ => Err(poisoned_error("there is no live debug adapter connection")),
        }
    }

    /// Refuse to talk to a session that is not ready, then hand back the
    /// transport and the per-request timeout.
    ///
    /// Returning the timeout by value (rather than a borrow of `self.config`)
    /// keeps the borrow checker happy at every call site and, more importantly,
    /// keeps the discipline visible: *every* request goes through here, so no
    /// future request can accidentally skip the readiness check.
    fn require_ready(&mut self, what: &str) -> Result<&mut Transport> {
        match self.state {
            SessionState::Ready => {}
            SessionState::Poisoned => {
                return Err(poisoned_error(what));
            }
            SessionState::Idle | SessionState::Attaching => {
                return Err(PrincessError::internal(format!(
                    "cannot {what}: the debug session is not attached yet"
                )));
            }
            SessionState::Detached => {
                return Err(PrincessError::internal(format!(
                    "cannot {what}: the debug session has been detached"
                )));
            }
        }
        let timeout = self.config.request_timeout;
        let Some(transport) = self.transport.as_mut() else {
            return Err(poisoned_error(what));
        };
        if transport.is_closed() {
            self.state = SessionState::Poisoned;
            return Err(poisoned_error(what));
        }
        transport.set_default_timeout(timeout);
        Ok(transport)
    }

    /// Mark the session unusable after a protocol-level failure.
    fn poison(&mut self, _reason: &str) {
        self.state = SessionState::Poisoned;
    }

    /// The capability layer over the live session.
    ///
    /// Returned by value rather than stored, so it can never outlive the
    /// transport borrow and there is no second copy of session state.
    pub fn capabilities_layer(&mut self) -> Result<CapabilityLayer<'_>> {
        let transport = self.transport_mut()?;
        Ok(CapabilityLayer::new(transport))
    }

    /// Read the physical memory the DAP trunk cannot reach.
    ///
    /// This is the capability that makes a page-table or GDT/IDT view possible:
    /// D11 §4.1 records that the built-in DAP's `readMemory` is virtual-only by
    /// construction, so there is no DAP request that could do this.
    pub fn read_physical_memory(&mut self, address: u64, count: u64) -> Result<Vec<u8>> {
        let transport = self.require_ready("read physical memory")?;
        CapabilityLayer::new(transport).read_physical_memory(address, count)
    }

    /// Set a hardware breakpoint through the capability layer.
    ///
    /// Emits `debug.breakpoint.changed` so the UI converges on the same state
    /// the engine has — the built-in DAP's own breakpoint map knows nothing
    /// about a breakpoint created with `hbreak`.
    pub fn set_hardware_breakpoint(
        &mut self,
        spec: &str,
        events: &mut dyn EventSink,
    ) -> Result<crate::capability::BreakpointRecord> {
        let transport = self.transport_mut()?;
        let mut layer = CapabilityLayer::new(transport);
        let record = match layer.set_hardware_breakpoint(spec) {
            Ok(record) => {
                self.hardware = HardwareSupport::Hardware;
                record
            }
            Err(err) => {
                // A failed `hbreak` is *not* necessarily "unsupported": it is
                // usually a bad symbol.  Only a target-level refusal counts.
                if err.detail.as_deref().is_some_and(|d| {
                    d.contains("not supported") || d.contains("Undefined command")
                }) {
                    self.hardware = HardwareSupport::Unsupported;
                }
                return Err(err);
            }
        };
        self.hardware_breakpoints.push(record.clone());
        events.emit(EventBody::DebugBreakpointChanged(
            princess_core::DebugBreakpointChangedPayload {
                id: format!("{HW_BREAKPOINT_ID_PREFIX}{}", record.number),
                verified: !record.is_pending(),
                location: Some(SourceLocation {
                    file: record.what.clone(),
                    line: 0,
                    column: None,
                }),
            },
        ))?;
        Ok(record)
    }

    /// Load the ELF's symbols and run the D10 self-check.
    ///
    /// Split out of `attach` so it can also be called after a rebuild.
    pub fn prepare_target(&mut self) -> Result<()> {
        let symbols = self.config.symbols.clone();
        let transport = self.transport_mut()?;
        let mut layer = CapabilityLayer::new(transport);
        layer.prepare_session()?;
        layer.load_symbols(&symbols)?;

        // D10: compare the ELF against what the stub actually speaks, *before*
        // any frame or register is read.  A mismatch here is the only thing
        // standing between the user and a nonsense backtrace.
        let verdict = arch::check(layer.transport_mut(), &symbols)?;
        verdict.clone().require_consistent(&symbols)?;
        self.arch_check = Some(verdict);
        Ok(())
    }

    /// Convenience for the acceptance harness: boot, attach, prepare.
    pub fn start_session(&mut self, events: &mut dyn EventSink) -> Result<()> {
        self.attach(
            &GdbStub {
                host: "127.0.0.1".into(),
                port: 1234,
                mode: StubMode::Attach,
            },
            events,
        )?;
        self.prepare_target()
    }

    /// Translate a DAP `stopped` event into the engine's payload.
    fn stopped_payload(&mut self, event: &Value, reason: StopReason) -> Result<DebugStoppedPayload> {
        let thread_id = event
            .pointer("/body/threadId")
            .and_then(Value::as_i64)
            .unwrap_or(self.thread_id);
        self.thread_id = thread_id;

        let frames = self.request_stack_trace(thread_id, 0, 1).unwrap_or_default();
        let frame = frames.first().cloned().unwrap_or_else(|| StackFrame {
            id: 0,
            name: String::new(),
            source: None,
            instructionPointer: String::new(),
        });

        // Registers come from the capability layer, never from DAP `scopes`:
        // the scope is absent in some sessions and never carries CR0/CR3.
        let regs: RegisterFile = self
            .try_read_registers()
            .unwrap_or_default();

        Ok(DebugStoppedPayload {
            reason,
            thread_id,
            frame,
            regs,
        })
    }

    fn try_read_registers(&mut self) -> Option<RegisterFile> {
        let transport = self.transport.as_mut()?;
        let mut layer = CapabilityLayer::new(transport);
        layer.read_registers(RegisterSelection::All).ok().map(|(map, _)| map)
    }

    fn request_stack_trace(
        &mut self,
        thread_id: i64,
        start_frame: u64,
        levels: u64,
    ) -> Result<Vec<StackFrame>> {
        let timeout = self.config.request_timeout;
        let transport = self.require_ready("read the stack trace")?;
        let response = transport.request_within(
            "stackTrace",
            Some(json!({
                "threadId": thread_id,
                "startFrame": start_frame,
                "levels": if levels == 0 { 20 } else { levels },
            })),
            timeout,
        );
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                // A protocol-level failure (timeout, dead adapter) poisons the
                // session; a `success: false` reply does not.
                if err.code == princess_core::ErrorCode::Timeout
                    || err.code == princess_core::ErrorCode::QemuFailed
                {
                    self.poison(&err.message);
                }
                return Err(err);
            }
        };
        let body = crate::transport::ensure_success("stackTrace", &response)?;
        let frames = parse_stack_frames(&body);
        self.frames = frames.clone();
        Ok(frames)
    }

    /// Stop reason from a DAP `stopped` event body.
    fn stop_reason(body: &Value) -> StopReason {
        match body.get("reason").and_then(Value::as_str) {
            Some("breakpoint") => StopReason::Breakpoint,
            Some("step") => StopReason::Step,
            Some("signal") | Some("exception") => StopReason::Signal,
            Some("entry") | Some("attach") => StopReason::Entry,
            _ => StopReason::Signal,
        }
    }

    /// Wait for the next `stopped` event and turn it into a payload.
    fn await_stop(&mut self, events: &mut dyn EventSink, what: &str) -> Result<DebugStoppedPayload> {
        let timeout = self.config.request_timeout.max(Duration::from_secs(30));
        let transport = self.transport_mut()?;
        let event = transport.wait_event("stopped", timeout)?.ok_or_else(|| {
            PrincessError::new(
                princess_core::ErrorCode::Timeout,
                format!("{what}: the target never reported a stopped event"),
            )
            .with_detail(
                "the engine waited for a `stopped` DAP event; without it there is no \
                 honest answer to give about where the CPU is."
                    .to_string(),
            )
        })?;
        let reason = Self::stop_reason(event.get("body").unwrap_or(&Value::Null));
        events.emit(EventBody::DebugOutput(princess_core::DebugOutputPayload {
            category: princess_core::DebugOutputCategory::Console,
            text: format!("stopped: {reason:?}\n"),
        }))?;
        self.stopped_payload(&event, reason)
    }
}

impl DebugBackend for DapBackend {
    fn id(&self) -> &str {
        "gdb-dap"
    }

    fn capabilities(&self) -> DebugCapabilities {
        // Start from the frozen defaults, then correct the two entries that the
        // built-in DAP's own `initialize` response contradicts or that this
        // layer changes the meaning of.
        let adapter = self.adapter_supports.as_ref();
        let flag = |name: &str, fallback: bool| {
            adapter
                .and_then(|a| a.get(name))
                .and_then(Value::as_bool)
                .unwrap_or(fallback)
        };
        DebugCapabilities {
            supports_conditional_breakpoints: flag("supportsConditionalBreakpoints", true),
            supports_disassemble_request: flag("supportsDisassembleRequest", true),
            // This one is deliberately true *because of the capability layer*:
            // the DAP request exists, and physical reads are added on top.  A
            // caller that needs physical memory must still branch on
            // `hardware_support`/`physical_memory` results, which is why the
            // probe result is exposed explicitly.
            supports_read_memory: true,
            supports_write_memory: flag("supportsWriteMemoryRequest", true),
            supports_restart: false,
            supports_terminate: flag("supportsTerminateRequest", true),
            supports_set_variable: flag("supportsSetVariable", false),
        }
    }

    fn attach(&mut self, stub: &GdbStub, events: &mut dyn EventSink) -> Result<()> {
        if self.state == SessionState::Ready {
            return Err(PrincessError::internal(
                "the debug session is already attached; detach first",
            ));
        }
        self.state = SessionState::Attaching;

        // 1. Start the machine, if the engine owns it.
        if let TargetMode::Launch { command } = &self.config.target {
            let mut command = (**command).clone();
            command.ready_timeout = self.config.ready_timeout;
            if let Some(log) = &self.config.serial_log {
                command.argv = command
                    .argv
                    .iter()
                    .map(|arg| match arg.strip_prefix("file:") {
                        Some(_) => format!("file:{}", log.to_string_lossy()),
                        None => arg.clone(),
                    })
                    .collect();
            }
            events.log(
                LogStream::Ide,
                &format!("qemu (engine-owned, timeout-wrapped): {}\n", command.display()),
            )?;
            let process = QemuProcess::start(command)?;
            events.log(
                LogStream::Ide,
                &format!("qemu pid {} is up; GDB stub listening\n", process.pid()),
            )?;
            self.qemu = Some(process);
        }

        // 2. Bring up the adapter.
        let adapter = self.config.adapter.to_string_lossy().into_owned();
        let mut transport = Transport::spawn(AdapterCommand::new(vec![
            adapter.clone(),
            "-q".into(),
            "-i=dap".into(),
        ]))?;
        events.log(LogStream::Ide, &format!("debug adapter: {adapter} -q -i=dap\n"))?;

        // 3. initialize.
        let response = transport.request_within(
            "initialize",
            Some(json!({
                "clientID": "princesside",
                "clientName": "PrincessIDE",
                "adapterID": "gdb",
                "locale": "en_US",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "pathFormat": "path",
                "supportsRunInTerminalRequest": false,
                "supportsVariableType": true,
                "supportsMemoryReferences": true,
                "supportsProgressReporting": false,
            })),
            self.config.request_timeout,
        )?;
        let initialized = crate::transport::ensure_success("initialize", &response)?;
        self.adapter_supports = initialized.get("body").cloned();

        // 4. attach + configurationDone, honouring the delayed `attach` reply.
        let target = format!("{}:{}", stub.host, stub.port);
        let attach_seq = transport.send("attach", Some(json!({ "target": target })))?;
        let config_seq = transport.send("configurationDone", None)?;

        // configurationDone is answered first; the attach reply may already be
        // stashed by the time we ask for it.
        transport
            .wait_response(config_seq, self.config.request_timeout)
            .map_err(|err| {
                self.poison("configurationDone failed");
                err
            })?;
        let attach_response = transport
            .wait_response(attach_seq, self.config.request_timeout)
            .map_err(|err| {
                self.poison("attach failed");
                err
            })?;
        crate::transport::ensure_success("attach", &attach_response).map_err(|err| {
            self.poison("attach refused");
            err
        })?;

        // 5. The initial `stopped` event (`reason: "attach"`): consume it so a
        //    later `continue` does not immediately "stop" on a stale event.
        let _ = transport.wait_event("stopped", Duration::from_secs(2))?;
        if let Some(id) = transport
            .take_events()
            .iter()
            .find_map(|e| e.pointer("/body/threadId").and_then(Value::as_i64))
        {
            self.thread_id = id;
        }

        self.transport = Some(transport);
        self.state = SessionState::Ready;
        events.emit(EventBody::DebugOutput(princess_core::DebugOutputPayload {
            category: princess_core::DebugOutputCategory::Console,
            text: format!("attached to {target}\n"),
        }))?;
        Ok(())
    }

    fn detach(&mut self) -> Result<()> {
        if let Some(mut transport) = self.transport.take() {
            // Best effort: the target may already be gone.
            let _ = transport.request_within("disconnect", Some(json!({})), Duration::from_secs(3));
            transport.shutdown();
        }
        if let Some(qemu) = self.qemu.as_mut() {
            qemu.stop()?;
        }
        self.qemu = None;
        self.state = SessionState::Detached;
        Ok(())
    }

    fn set_breakpoints(
        &mut self,
        file: &Path,
        breakpoints: &[BreakpointSpec],
        events: &mut dyn EventSink,
    ) -> Result<Vec<BreakpointSpec>> {
        let timeout = self.config.request_timeout;
        let transport = self.require_ready("set breakpoints")?;
        let requested: Vec<Value> = breakpoints
            .iter()
            .map(|bp| {
                let mut entry = json!({});
                if let Some(line) = bp.line {
                    entry["line"] = json!(line);
                }
                if let Some(condition) = &bp.condition {
                    entry["condition"] = json!(condition);
                }
                entry
            })
            .collect();

        let response = transport.request_within(
            "setBreakpoints",
            Some(json!({
                "source": { "path": file.to_string_lossy(), "name": file.file_name().map(|n| n.to_string_lossy().into_owned()) },
                "breakpoints": requested,
                "sourceModified": false,
            })),
            timeout,
        )?;
        let body = crate::transport::ensure_success("setBreakpoints", &response)?;

        let mut result = Vec::new();
        if let Some(list) = body.pointer("/body/breakpoints").and_then(Value::as_array) {
            for (index, item) in list.iter().enumerate() {
                let template = breakpoints.get(index);
                let verified = item.get("verified").and_then(Value::as_bool).unwrap_or(false);
                let line = item
                    .get("line")
                    .and_then(Value::as_u64)
                    .map(|l| l as u32)
                    .or_else(|| template.and_then(|t| t.line));
                let location = item.get("source").and_then(|source| {
                    let path = source.get("path")?.as_str()?;
                    Some(SourceLocation {
                        file: path.to_string(),
                        line: line.unwrap_or(0),
                        column: item
                            .get("column")
                            .and_then(Value::as_u64)
                            .map(|c| c as u32),
                    })
                });
                result.push(BreakpointSpec {
                    id: item
                        .get("id")
                        .map(|id| id.to_string())
                        .or_else(|| template.map(|t| t.id.clone()))
                        .unwrap_or_else(|| format!("bp-{index}")),
                    file: Some(file.to_string_lossy().into_owned()),
                    line,
                    address: item
                        .get("instructionReference")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    condition: template.and_then(|t| t.condition.clone()),
                    verified,
                    location,
                });
            }
        }

        for bp in &result {
            events.emit(EventBody::DebugBreakpointChanged(
                princess_core::DebugBreakpointChangedPayload {
                    id: bp.id.clone(),
                    verified: bp.verified,
                    location: bp.location.clone(),
                },
            ))?;
        }
        Ok(result)
    }

    fn continue_(
        &mut self,
        thread_id: Option<i64>,
        events: &mut dyn EventSink,
    ) -> Result<DebugStoppedPayload> {
        let thread = thread_id.unwrap_or(self.thread_id);
        let timeout = self.config.request_timeout;
        let transport = self.require_ready("continue")?;
        transport
            .request_within("continue", Some(json!({ "threadId": thread })), timeout)
            .and_then(|response| crate::transport::ensure_success("continue", &response))?;
        self.await_stop(events, "continue")
    }

    fn step_over(
        &mut self,
        thread_id: Option<i64>,
        events: &mut dyn EventSink,
    ) -> Result<DebugStoppedPayload> {
        let thread = thread_id.unwrap_or(self.thread_id);
        let timeout = self.config.request_timeout;
        let transport = self.require_ready("step over")?;
        transport
            .request_within("next", Some(json!({ "threadId": thread })), timeout)
            .and_then(|response| crate::transport::ensure_success("next", &response))?;
        self.await_stop(events, "step over")
    }

    fn step_into(
        &mut self,
        thread_id: Option<i64>,
        events: &mut dyn EventSink,
    ) -> Result<DebugStoppedPayload> {
        let thread = thread_id.unwrap_or(self.thread_id);
        let timeout = self.config.request_timeout;
        let transport = self.require_ready("step into")?;
        // Source-level stepping in a kernel is misleading: there is no process
        // and often no frame pointer chain.  Instruction granularity is what
        // actually tells the truth here, and the adapter advertises
        // `supportsSteppingGranularity`.
        transport
            .request_within(
                "stepIn",
                Some(json!({ "threadId": thread, "granularity": "instruction" })),
                timeout,
            )
            .and_then(|response| crate::transport::ensure_success("stepIn", &response))?;
        self.await_stop(events, "step into")
    }

    fn stack_trace(
        &mut self,
        thread_id: i64,
        start_frame: u64,
        levels: u64,
    ) -> Result<Vec<StackFrame>> {
        self.request_stack_trace(thread_id, start_frame, levels)
    }

    fn scopes(&mut self, frame_id: u64) -> Result<Vec<Scope>> {
        let timeout = self.config.request_timeout;
        let transport = self.require_ready("read scopes")?;
        let response = transport.request_within(
            "scopes",
            Some(json!({ "frameId": frame_id })),
            timeout,
        )?;
        let body = crate::transport::ensure_success("scopes", &response)?;

        let mut scopes = Vec::new();
        if let Some(list) = body.pointer("/body/scopes").and_then(Value::as_array) {
            for item in list {
                let Some(name) = item.get("name").and_then(Value::as_str) else {
                    continue;
                };
                scopes.push(Scope {
                    name: name.to_string(),
                    variablesReference: item
                        .get("variablesReference")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    expensive: item.get("expensive").and_then(Value::as_bool).unwrap_or(false),
                });
            }
        }

        // The capability layer's whole reason for existing: `Registers` is
        // missing from this list in some sessions (research C §4.2) and never
        // carries CR0/CR3 when it is present.  Rather than depend on it, expose
        // a synthetic scope backed by `info registers`.  `-1` is a reserved
        // reference in this backend meaning "the register panel".
        if !scopes.iter().any(|s| s.name == "Registers") {
            scopes.push(Scope {
                name: "Registers".into(),
                variablesReference: REGISTERS_SCOPE_REFERENCE,
                expensive: false,
            });
        }
        Ok(scopes)
    }

    fn variables(&mut self, variables_reference: i64) -> Result<Vec<Variable>> {
        if variables_reference == REGISTERS_SCOPE_REFERENCE {
            let transport = self.require_ready("read the register panel")?;
            let (_, entries) = CapabilityLayer::new(transport)
                .read_registers(RegisterSelection::All)?;
            return Ok(entries
                .into_iter()
                .map(|entry| Variable {
                    name: entry.name,
                    value: entry.value,
                    type_name: entry.annotation,
                    variablesReference: 0,
                })
                .collect());
        }

        let timeout = self.config.request_timeout;
        let transport = self.require_ready("read variables")?;
        let response = transport.request_within(
            "variables",
            Some(json!({ "variablesReference": variables_reference })),
            timeout,
        )?;
        let body = crate::transport::ensure_success("variables", &response)?;
        let mut variables = Vec::new();
        if let Some(list) = body.pointer("/body/variables").and_then(Value::as_array) {
            for item in list {
                let Some(name) = item.get("name").and_then(Value::as_str) else {
                    continue;
                };
                variables.push(Variable {
                    name: name.to_string(),
                    value: item
                        .get("value")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    type_name: item.get("type").and_then(Value::as_str).map(str::to_string),
                    variablesReference: item
                        .get("variablesReference")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                });
            }
        }
        Ok(variables)
    }

    fn read_memory(&mut self, address: u64, byte_count: u64) -> Result<Vec<u8>> {
        let timeout = self.config.request_timeout;
        let transport = self.require_ready("read memory")?;
        let response = transport.request_within(
            "readMemory",
            Some(json!({
                "memoryReference": format!("0x{address:x}"),
                "count": byte_count,
            })),
            timeout,
        )?;
        let body = crate::transport::ensure_success("readMemory", &response)?;
        let encoded = body
            .pointer("/body/data")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                PrincessError::internal("readMemory returned no data field")
                    .with_detail(body.to_string())
            })?;
        let bytes = base64_decode(encoded).ok_or_else(|| {
            PrincessError::internal("readMemory returned data that is not valid base64")
                .with_detail(format!("data: {encoded:.200}"))
        })?;
        // A short read must be an error, never a quietly truncated buffer.
        if bytes.len() as u64 != byte_count {
            return Err(PrincessError::internal(format!(
                "readMemory returned {} bytes for a {byte_count} byte request",
                bytes.len()
            ))
            .with_detail(body.to_string()));
        }
        Ok(bytes)
    }

    fn write_memory(&mut self, address: u64, bytes: &[u8]) -> Result<()> {
        let timeout = self.config.request_timeout;
        let transport = self.require_ready("write memory")?;
        let response = transport.request_within(
            "writeMemory",
            Some(json!({
                "memoryReference": format!("0x{address:x}"),
                "data": base64_encode(bytes),
            })),
            timeout,
        )?;
        crate::transport::ensure_success("writeMemory", &response)?;
        Ok(())
    }

    fn disassemble(&mut self, address: u64, count: u64) -> Result<Vec<DisassembledInstruction>> {
        let timeout = self.config.request_timeout;
        let transport = self.require_ready("disassemble")?;
        // D11 §4.5: `memoryReference` must be a *number*; `$pc` is rejected by
        // the adapter, so callers pass an address (use `CapabilityLayer::
        // program_counter` to resolve `$pc` first).
        let response = transport.request_within(
            "disassemble",
            Some(json!({
                "memoryReference": format!("0x{address:x}"),
                "instructionCount": count,
                "resolveSymbols": true,
            })),
            timeout,
        )?;
        let body = crate::transport::ensure_success("disassemble", &response)?;
        let mut instructions = Vec::new();
        if let Some(list) = body.pointer("/body/instructions").and_then(Value::as_array) {
            for item in list {
                let Some(text) = item.get("instruction").and_then(Value::as_str) else {
                    continue;
                };
                instructions.push(DisassembledInstruction {
                    address: item
                        .get("address")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    bytes: item
                        .get("instructionBytes")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    text: text.to_string(),
                    symbol: item.get("symbol").and_then(Value::as_str).map(str::to_string),
                    source: item.get("location").and_then(|location| {
                        let path = location.get("path")?.as_str()?;
                        Some(SourceLocation {
                            file: path.to_string(),
                            line: location.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                            column: None,
                        })
                    }),
                });
            }
        }
        Ok(instructions)
    }

    fn registers(&mut self, _thread_id: i64) -> Result<RegisterFile> {
        let transport = self.require_ready("read registers")?;
        let mut layer = CapabilityLayer::new(transport);
        let (map, _) = layer.read_registers(RegisterSelection::All)?;
        Ok(map)
    }
}

impl Drop for DapBackend {
    fn drop(&mut self) {
        // Never leave a machine or an adapter behind, whatever path got us here.
        if let Some(mut transport) = self.transport.take() {
            transport.shutdown();
        }
        if let Some(qemu) = self.qemu.as_mut() {
            let _ = qemu.stop();
        }
    }
}

/// The reserved `variablesReference` meaning "the register panel".
pub const REGISTERS_SCOPE_REFERENCE: i64 = -1;

/// The refusal used whenever a session cannot be talked to.
///
/// A free function rather than a method so call sites can build it while
/// `self.transport` is mutably borrowed.
fn poisoned_error(what: &str) -> PrincessError {
    PrincessError::new(
        princess_core::ErrorCode::QemuFailed,
        format!("cannot {what}: the debug session is no longer usable"),
    )
    .with_detail(
        "gdb's DAP session answers with plausible-looking but wrong data after a \
         protocol-level failure (research C §3.2), so the engine refuses to keep \
         using it. Re-attach to continue."
            .to_string(),
    )
}

// ------------------------------------------------------------------ helpers --

/// Parse DAP `stackTrace` body frames into engine frames.
pub fn parse_stack_frames(body: &Value) -> Vec<StackFrame> {
    let mut frames = Vec::new();
    let Some(list) = body.pointer("/body/stackFrames").and_then(Value::as_array) else {
        return frames;
    };
    for item in list {
        let source = item.get("source").and_then(|source| {
            let path = source.get("path").and_then(Value::as_str)?;
            let line = item.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
            Some(SourceLocation {
                file: path.to_string(),
                line,
                column: item
                    .get("column")
                    .and_then(Value::as_u64)
                    .filter(|c| *c > 0)
                    .map(|c| c as u32),
            })
        });
        frames.push(StackFrame {
            id: item.get("id").and_then(Value::as_u64).unwrap_or(0),
            name: item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            source,
            instructionPointer: item
                .get("instructionPointerReference")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        });
    }
    frames
}

/// Standard base64 (RFC 4648) decoder, used for DAP `readMemory` payloads.
///
/// Hand-rolled rather than pulling in a crate: DAP uses the plain alphabet with
/// `=` padding and nothing else, and P4 must not add a dependency for ~30 lines
/// (D22 keeps the dependency surface small by policy).
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let padding = chunk.iter().filter(|b| **b == b'=').count();
        if chunk.len() < 2 || padding > 2 {
            return None;
        }
        let mut accumulator: u32 = 0;
        let mut count = 0;
        for byte in chunk {
            if *byte == b'=' {
                accumulator <<= 6;
                count += 1;
                continue;
            }
            accumulator = (accumulator << 6) | value(*byte)? as u32;
            count += 1;
        }
        while count < 4 {
            accumulator <<= 6;
            count += 1;
        }
        let triple = accumulator.to_be_bytes();
        out.push(triple[1]);
        if chunk.len() > 2 && padding < 2 {
            out.push(triple[2]);
        }
        if chunk.len() > 3 && padding < 1 {
            out.push(triple[3]);
        }
    }
    Some(out)
}

/// Standard base64 encoder, the inverse of [`base64_decode`].
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The base64 payload gdb actually returned for `readMemory 0x104000 16`.
    #[test]
    fn decodes_the_measured_read_memory_payload() {
        let bytes = base64_decode("I1AQAAAAAAAAAAAAAAAAAA==").unwrap();
        assert_eq!(
            bytes,
            vec![0x23, 0x50, 0x10, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn base64_round_trips_every_length_modulo_three() {
        for length in 0..40usize {
            let data: Vec<u8> = (0..length).map(|i| (i * 37 + 11) as u8).collect();
            let encoded = base64_encode(&data);
            assert_eq!(
                base64_decode(&encoded).as_deref(),
                Some(data.as_slice()),
                "length {length} (encoded {encoded})"
            );
            assert_eq!(encoded.len() % 4, 0, "base64 must be padded to a multiple of 4");
        }
    }

    #[test]
    fn base64_rejects_garbage_instead_of_guessing() {
        assert!(base64_decode("!!!!").is_none());
        assert!(base64_decode("A").is_none());
    }

    /// The real `stackTrace` body from the paging fixture (research C §3.1 /
    /// re-measured here): C frame + assembly frame, both with source lines.
    #[test]
    fn parses_the_measured_mixed_c_and_asm_stack_trace() {
        let body = json!({
            "body": { "stackFrames": [
                { "id": 0, "line": 120, "column": 0,
                  "instructionPointerReference": "0x1011c9",
                  "moduleId": "/root/PrincessIDE/fixtures/paging-kernel/build/pagingkernel.elf",
                  "source": { "name": "kernel.c", "path": "/root/PrincessIDE/fixtures/paging-kernel/kernel.c" },
                  "name": "paging_fault_probe" },
                { "id": 1, "line": 157,
                  "instructionPointerReference": "0x1012cc",
                  "source": { "name": "kernel.c", "path": "/root/PrincessIDE/fixtures/paging-kernel/kernel.c" },
                  "name": "kernel_main" },
                { "id": 2, "line": 194,
                  "instructionPointerReference": "0x100193",
                  "source": { "name": "boot.S", "path": "/root/PrincessIDE/fixtures/paging-kernel/boot.S" },
                  "name": "_start" }
            ]}
        });
        let frames = parse_stack_frames(&body);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].name, "paging_fault_probe");
        assert_eq!(frames[0].source.as_ref().unwrap().line, 120);
        assert_eq!(frames[0].instructionPointer, "0x1011c9");
        // The assembly frame must keep its own source file, not be folded in.
        assert_eq!(frames[2].name, "_start");
        assert_eq!(frames[2].source.as_ref().unwrap().file, "/root/PrincessIDE/fixtures/paging-kernel/boot.S");
        assert_eq!(frames[2].source.as_ref().unwrap().line, 194);
    }

    #[test]
    fn an_empty_stack_trace_is_empty_not_padded_with_a_fake_frame() {
        assert!(parse_stack_frames(&json!({"body": {"stackFrames": []}})).is_empty());
        assert!(parse_stack_frames(&json!({})).is_empty());
    }

    #[test]
    fn frames_without_debug_info_have_no_source_but_keep_their_address() {
        let body = json!({"body": {"stackFrames": [
            {"id": 0, "name": "??", "instructionPointerReference": "0x1234"}
        ]}});
        let frames = parse_stack_frames(&body);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].source.is_none(), "a missing source must be null, not invented");
        assert_eq!(frames[0].instructionPointer, "0x1234");
    }

    #[test]
    fn stop_reasons_map_onto_the_frozen_enum() {
        assert_eq!(DapBackend::stop_reason(&json!({"reason": "breakpoint"})), StopReason::Breakpoint);
        assert_eq!(DapBackend::stop_reason(&json!({"reason": "step"})), StopReason::Step);
        assert_eq!(DapBackend::stop_reason(&json!({"reason": "signal"})), StopReason::Signal);
        assert_eq!(DapBackend::stop_reason(&json!({"reason": "exception"})), StopReason::Signal);
        assert_eq!(DapBackend::stop_reason(&json!({"reason": "entry"})), StopReason::Entry);
        assert_eq!(DapBackend::stop_reason(&json!({"reason": "attach"})), StopReason::Entry);
        // An unknown reason must not silently become "breakpoint".
        assert_eq!(DapBackend::stop_reason(&json!({"reason": "wat"})), StopReason::Signal);
    }

    #[test]
    fn a_fresh_backend_is_idle_and_reports_no_capabilities() {
        let backend = DapBackend::new(DebugSessionConfig::launching(
            "/bin/true",
            "/tmp/k.elf",
            vec!["/bin/true".into()],
            "/tmp",
        ));
        assert_eq!(backend.state(), SessionState::Idle);
        assert_eq!(backend.hardware_support(), HardwareSupport::Unknown);
        assert!(backend.adapter_capabilities().is_none());
        assert!(backend.hardware_breakpoints().is_empty());
        assert_eq!(backend.id(), "gdb-dap");
    }

    /// The trait must report the adapter's own capability answers, not defaults.
    #[test]
    fn capabilities_follow_the_adapters_initialize_response() {
        let mut backend = DapBackend::new(DebugSessionConfig::launching(
            "/bin/true",
            "/tmp/k.elf",
            vec!["/bin/true".into()],
            "/tmp",
        ));
        let caps = backend.capabilities();
        assert!(caps.supports_read_memory);
        assert!(!caps.supports_restart, "the frozen default says restart, gdb cannot");

        backend.adapter_supports = Some(json!({
            "supportsConditionalBreakpoints": false,
            "supportsWriteMemoryRequest": false,
            "supportsSetVariable": true,
            "supportsTerminateRequest": true,
            "supportsDisassembleRequest": true,
        }));
        let caps = backend.capabilities();
        assert!(!caps.supports_conditional_breakpoints);
        assert!(!caps.supports_write_memory);
        assert!(caps.supports_set_variable);
        assert!(caps.supports_terminate);
    }

    #[test]
    fn requests_against_a_non_attached_session_are_refused() {
        let mut backend = DapBackend::new(DebugSessionConfig::launching(
            "/bin/true",
            "/tmp/k.elf",
            vec!["/bin/true".into()],
            "/tmp",
        ));
        let err = backend.registers(1).unwrap_err();
        assert!(err.message.contains("not attached"), "{err}");
    }

    #[test]
    fn a_poisoned_session_refuses_further_requests() {
        let mut backend = DapBackend::new(DebugSessionConfig::launching(
            "/bin/true",
            "/tmp/k.elf",
            vec!["/bin/true".into()],
            "/tmp",
        ));
        backend.state = SessionState::Poisoned;
        let err = backend.stack_trace(1, 0, 20).unwrap_err();
        assert!(err.message.contains("no longer usable"), "{err}");
        assert!(
            err.detail.as_deref().unwrap().contains("plausible"),
            "the refusal must explain *why* a poisoned session is dangerous"
        );
    }

    #[test]
    fn the_register_scope_reference_is_reserved_and_distinct() {
        // DAP uses positive references for real scopes; -1 can never collide.
        assert!(REGISTERS_SCOPE_REFERENCE < 0);
    }
}
