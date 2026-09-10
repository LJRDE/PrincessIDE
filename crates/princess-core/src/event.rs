//! The engine → UI event stream — `docs/spec/10-contracts.md` §2.
//!
//! One ordered, append-only stream of events.  Every event is wrapped in the
//! same envelope and the JSON produced here is what the web front end consumes
//! verbatim: no translation layer, no renames.
//!
//! ```jsonc
//! {
//!   "v": 1,
//!   "seq": 12345,
//!   "ts": "2026-05-05T12:00:00.123Z",
//!   "opId": "op-7f3a",
//!   "kind": "build.finished",
//!   "payload": { }
//! }
//! ```
//!
//! Design notes that matter to consumers:
//!
//! * `Event` has a **hand-written** `Serialize`: the six envelope keys are
//!   always emitted in contract order and `kind`/`payload` are derived from
//!   [`EventBody`], so a payload type can never drift away from its `kind`.
//! * Payloads tolerate *unknown* fields on input (§7: "新增字段必须可选") so a
//!   newer engine does not break an older UI, but they are strict about types.
//! * `seq` is allocated by [`EventRecorder`] and is strictly monotonic within a
//!   session; [`validate_stream`] re-checks that invariant on a recorded stream.

use std::fmt;
use std::io::Write;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{ErrorCode, PrincessError, Result};
use crate::types::{
    AiUsage, Artifact, ArtifactKind, BuildStatus, DiagnosticSeverity, DiagnosticSource, ExitReason,
    LogStream, RegisterFile, SourceLocation, StackFrame, SymbolicatedLocation, TextEncoding,
};

/// The event-model version carried in every envelope's `v` field (§7).
pub const EVENT_MODEL_VERSION: u32 = 1;

/// `kind` discriminator for every event in the v1 contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EventKind {
    #[serde(rename = "log.append")]
    LogAppend,
    #[serde(rename = "build.started")]
    BuildStarted,
    #[serde(rename = "build.diagnostic")]
    BuildDiagnostic,
    #[serde(rename = "build.finished")]
    BuildFinished,
    #[serde(rename = "run.started")]
    RunStarted,
    #[serde(rename = "run.fault")]
    RunFault,
    #[serde(rename = "run.exited")]
    RunExited,
    #[serde(rename = "debug.stopped")]
    DebugStopped,
    #[serde(rename = "debug.breakpoint.changed")]
    DebugBreakpointChanged,
    #[serde(rename = "debug.output")]
    DebugOutput,
    #[serde(rename = "symbols.indexed")]
    SymbolsIndexed,
    #[serde(rename = "artifact.changed")]
    ArtifactChanged,
    #[serde(rename = "ai.chunk")]
    AiChunk,
    #[serde(rename = "ai.finished")]
    AiFinished,
}

impl EventKind {
    /// The wire string, frozen by the contract.  `serde` uses the same strings
    /// (asserted by unit tests), this is for non-serde call sites.
    pub const fn as_str(self) -> &'static str {
        match self {
            EventKind::LogAppend => "log.append",
            EventKind::BuildStarted => "build.started",
            EventKind::BuildDiagnostic => "build.diagnostic",
            EventKind::BuildFinished => "build.finished",
            EventKind::RunStarted => "run.started",
            EventKind::RunFault => "run.fault",
            EventKind::RunExited => "run.exited",
            EventKind::DebugStopped => "debug.stopped",
            EventKind::DebugBreakpointChanged => "debug.breakpoint.changed",
            EventKind::DebugOutput => "debug.output",
            EventKind::SymbolsIndexed => "symbols.indexed",
            EventKind::ArtifactChanged => "artifact.changed",
            EventKind::AiChunk => "ai.chunk",
            EventKind::AiFinished => "ai.finished",
        }
    }

    /// Every kind in the v1 contract, in table order.
    pub const ALL: [EventKind; 14] = [
        EventKind::LogAppend,
        EventKind::BuildStarted,
        EventKind::BuildDiagnostic,
        EventKind::BuildFinished,
        EventKind::RunStarted,
        EventKind::RunFault,
        EventKind::RunExited,
        EventKind::DebugStopped,
        EventKind::DebugBreakpointChanged,
        EventKind::DebugOutput,
        EventKind::SymbolsIndexed,
        EventKind::ArtifactChanged,
        EventKind::AiChunk,
        EventKind::AiFinished,
    ];

    pub fn from_str_opt(text: &str) -> Option<EventKind> {
        EventKind::ALL.into_iter().find(|k| k.as_str() == text)
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------- payloads ---

/// `log.append` — a chunk of streamed text (contract §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogAppendPayload {
    pub stream: LogStream,
    /// The text.  The engine never splits a UTF-8 code point across chunks.
    pub chunk: String,
    pub encoding: TextEncoding,
}

/// `build.started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildStartedPayload {
    /// `make` | `cmake` | `cargo` | `zig` | `custom`.
    pub backend: String,
    /// Identifier of the resolved toolchain the backend will use.
    pub toolchain_id: String,
    /// argv as executed (argv[0] first), after path resolution.
    pub argv: Vec<String>,
    /// Absolute working directory.
    pub cwd: String,
}

/// `build.diagnostic`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildDiagnosticPayload {
    pub severity: DiagnosticSeverity,
    /// Absolute path when the engine could resolve it, else the raw text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    pub message: String,
    pub source: DiagnosticSource,
}

/// `build.finished` — always emitted, even when the backend died (§6 rule 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildFinishedPayload {
    pub status: BuildStatus,
    /// Process exit code; `null` when the child was killed by a signal.
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub artifacts: Vec<Artifact>,
}

/// `run.started`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStartedPayload {
    /// The exact emulator argv the engine executed.
    pub qemu_argv: Vec<String>,
    /// `host:port` of the GDB stub, or `null` when no stub was requested.
    pub gdb_stub: Option<String>,
}

/// `run.fault` — a CPU exception taken by the guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunFaultPayload {
    /// Vector mnemonic, e.g. `#UD`, `#PF` — as the guest reported it.
    pub vector: String,
    /// Faulting instruction pointer.
    pub rip: u64,
    /// The guest's own hex text for `rip`, preserved verbatim.
    pub rip_text: String,
    /// Exception error code (0 for vectors that push none).
    pub error_code: u64,
    /// Register snapshot; hex text keyed by canonical register name.
    #[serde(default)]
    pub regs: RegisterFile,
    /// Filled in when debug info could map `rip`; `null` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbolicated: Option<SymbolicatedLocation>,
}

/// `run.exited` — always emitted, whatever killed the machine (§6 rule 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunExitedPayload {
    /// Emulator exit code; `null` when it died from a signal.
    pub exit_code: Option<i32>,
    /// Attributed by the engine, never by the UI (§6 rule 4).
    pub reason: ExitReason,
    /// Wall-clock lifetime of the run.
    pub uptime_ms: u64,
}

/// Why the debugger stopped (`debug.stopped.reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    #[serde(rename = "breakpoint")]
    Breakpoint,
    #[serde(rename = "step")]
    Step,
    #[serde(rename = "signal")]
    Signal,
    #[serde(rename = "entry")]
    Entry,
}

/// `debug.stopped`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugStoppedPayload {
    pub reason: StopReason,
    pub thread_id: i64,
    pub frame: StackFrame,
    #[serde(default)]
    pub regs: RegisterFile,
}

/// `debug.breakpoint.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugBreakpointChangedPayload {
    pub id: String,
    pub verified: bool,
    /// Where the backend planted it (may be a relocated address).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
}

/// Which console a `debug.output` chunk belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebugOutputCategory {
    #[serde(rename = "console")]
    Console,
    #[serde(rename = "stdout")]
    Stdout,
    #[serde(rename = "stderr")]
    Stderr,
}

/// `debug.output`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugOutputPayload {
    pub category: DebugOutputCategory,
    pub text: String,
}

/// `symbols.indexed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolsIndexedPayload {
    /// Absolute path of the indexed artifact.
    pub artifact: String,
    /// GNU build-id when the artifact has one; `null` for images built with
    /// `--build-id=none` (the reference kernel is one of those — the engine
    /// never invents an id).
    pub build_id: Option<String>,
    pub symbol_count: u64,
}

/// `artifact.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactChangedPayload {
    /// Absolute path.
    pub path: String,
    pub kind: ArtifactKind,
}

/// `ai.chunk`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChunkPayload {
    pub request_id: String,
    pub text: String,
}

/// `ai.finished`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiFinishedPayload {
    pub request_id: String,
    pub usage: AiUsage,
}

// -------------------------------------------------------------- event body ---

/// A typed event body: the `kind` + `payload` halves of the envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventBody {
    LogAppend(LogAppendPayload),
    BuildStarted(BuildStartedPayload),
    BuildDiagnostic(BuildDiagnosticPayload),
    BuildFinished(BuildFinishedPayload),
    RunStarted(RunStartedPayload),
    RunFault(RunFaultPayload),
    RunExited(RunExitedPayload),
    DebugStopped(DebugStoppedPayload),
    DebugBreakpointChanged(DebugBreakpointChangedPayload),
    DebugOutput(DebugOutputPayload),
    SymbolsIndexed(SymbolsIndexedPayload),
    ArtifactChanged(ArtifactChangedPayload),
    AiChunk(AiChunkPayload),
    AiFinished(AiFinishedPayload),
}

impl EventBody {
    pub fn kind(&self) -> EventKind {
        match self {
            EventBody::LogAppend(_) => EventKind::LogAppend,
            EventBody::BuildStarted(_) => EventKind::BuildStarted,
            EventBody::BuildDiagnostic(_) => EventKind::BuildDiagnostic,
            EventBody::BuildFinished(_) => EventKind::BuildFinished,
            EventBody::RunStarted(_) => EventKind::RunStarted,
            EventBody::RunFault(_) => EventKind::RunFault,
            EventBody::RunExited(_) => EventKind::RunExited,
            EventBody::DebugStopped(_) => EventKind::DebugStopped,
            EventBody::DebugBreakpointChanged(_) => EventKind::DebugBreakpointChanged,
            EventBody::DebugOutput(_) => EventKind::DebugOutput,
            EventBody::SymbolsIndexed(_) => EventKind::SymbolsIndexed,
            EventBody::ArtifactChanged(_) => EventKind::ArtifactChanged,
            EventBody::AiChunk(_) => EventKind::AiChunk,
            EventBody::AiFinished(_) => EventKind::AiFinished,
        }
    }
}

// --------------------------------------------------------------- serializer --

/// Serialises only the payload half of an [`EventBody`].
struct PayloadOf<'a>(&'a EventBody);

impl Serialize for PayloadOf<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            EventBody::LogAppend(p) => p.serialize(s),
            EventBody::BuildStarted(p) => p.serialize(s),
            EventBody::BuildDiagnostic(p) => p.serialize(s),
            EventBody::BuildFinished(p) => p.serialize(s),
            EventBody::RunStarted(p) => p.serialize(s),
            EventBody::RunFault(p) => p.serialize(s),
            EventBody::RunExited(p) => p.serialize(s),
            EventBody::DebugStopped(p) => p.serialize(s),
            EventBody::DebugBreakpointChanged(p) => p.serialize(s),
            EventBody::DebugOutput(p) => p.serialize(s),
            EventBody::SymbolsIndexed(p) => p.serialize(s),
            EventBody::ArtifactChanged(p) => p.serialize(s),
            EventBody::AiChunk(p) => p.serialize(s),
            EventBody::AiFinished(p) => p.serialize(s),
        }
    }
}

// ------------------------------------------------------------------ envelope --

/// One event in the engine → UI stream (contract §2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Event-model version.
    pub v: u32,
    /// Monotonic within the session, starting at 1.
    pub seq: u64,
    /// Production timestamp, serialised as `...T..:..:...123Z` (UTC, ms).
    pub ts: DateTime<Utc>,
    /// Correlating long operation, or `None`.
    pub op_id: Option<String>,
    pub body: EventBody,
}

impl Event {
    pub fn new(seq: u64, ts: DateTime<Utc>, op_id: Option<String>, body: EventBody) -> Self {
        Self {
            v: EVENT_MODEL_VERSION,
            seq,
            ts,
            op_id,
            body,
        }
    }

    pub fn kind(&self) -> EventKind {
        self.body.kind()
    }

    /// Validate the envelope: model version and sequence sanity.
    pub fn validate(&self) -> Result<()> {
        if self.v != EVENT_MODEL_VERSION {
            return Err(PrincessError::new(
                ErrorCode::Internal,
                format!(
                    "event model version {} is not supported by this engine (expected {})",
                    self.v, EVENT_MODEL_VERSION
                ),
            ));
        }
        if self.seq == 0 {
            return Err(PrincessError::internal("event seq must start at 1"));
        }
        Ok(())
    }

    /// Newline-terminated JSON, exactly as it goes on the wire / into a fixture.
    pub fn to_ndjson(&self) -> Result<String> {
        let mut text = serde_json::to_string(self)?;
        text.push('\n');
        Ok(text)
    }
}

impl Serialize for Event {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        // Contract order: v, seq, ts, opId, kind, payload.
        let mut map = s.serialize_map(Some(6))?;
        map.serialize_entry("v", &self.v)?;
        map.serialize_entry("seq", &self.seq)?;
        map.serialize_entry("ts", &TimestampMillis(&self.ts))?;
        map.serialize_entry("opId", &self.op_id)?;
        map.serialize_entry("kind", self.body.kind().as_str())?;
        map.serialize_entry("payload", &PayloadOf(&self.body))?;
        map.end()
    }
}

/// Wire form used while deserialising: `payload` is kept raw until `kind` told
/// us which payload type to decode it as.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EventWire {
    v: u32,
    seq: u64,
    #[serde(with = "timestamp_millis")]
    ts: DateTime<Utc>,
    #[serde(default)]
    op_id: Option<String>,
    kind: EventKind,
    payload: serde_json::Value,
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let wire = EventWire::deserialize(d)?;
        let body = decode_payload(wire.kind, wire.payload).map_err(D::Error::custom)?;
        Ok(Event {
            v: wire.v,
            seq: wire.seq,
            ts: wire.ts,
            op_id: wire.op_id,
            body,
        })
    }
}

fn decode_payload(kind: EventKind, value: serde_json::Value) -> Result<EventBody> {
    use serde_json::from_value;
    let body = match kind {
        EventKind::LogAppend => EventBody::LogAppend(from_value(value).map_err(payload_err(kind))?),
        EventKind::BuildStarted => EventBody::BuildStarted(from_value(value).map_err(payload_err(kind))?),
        EventKind::BuildDiagnostic => {
            EventBody::BuildDiagnostic(from_value(value).map_err(payload_err(kind))?)
        }
        EventKind::BuildFinished => {
            EventBody::BuildFinished(from_value(value).map_err(payload_err(kind))?)
        }
        EventKind::RunStarted => EventBody::RunStarted(from_value(value).map_err(payload_err(kind))?),
        EventKind::RunFault => EventBody::RunFault(from_value(value).map_err(payload_err(kind))?),
        EventKind::RunExited => EventBody::RunExited(from_value(value).map_err(payload_err(kind))?),
        EventKind::DebugStopped => EventBody::DebugStopped(from_value(value).map_err(payload_err(kind))?),
        EventKind::DebugBreakpointChanged => {
            EventBody::DebugBreakpointChanged(from_value(value).map_err(payload_err(kind))?)
        }
        EventKind::DebugOutput => EventBody::DebugOutput(from_value(value).map_err(payload_err(kind))?),
        EventKind::SymbolsIndexed => {
            EventBody::SymbolsIndexed(from_value(value).map_err(payload_err(kind))?)
        }
        EventKind::ArtifactChanged => {
            EventBody::ArtifactChanged(from_value(value).map_err(payload_err(kind))?)
        }
        EventKind::AiChunk => EventBody::AiChunk(from_value(value).map_err(payload_err(kind))?),
        EventKind::AiFinished => EventBody::AiFinished(from_value(value).map_err(payload_err(kind))?),
    };
    Ok(body)
}

fn payload_err(kind: EventKind) -> impl Fn(serde_json::Error) -> PrincessError {
    move |err| {
        PrincessError::new(
            ErrorCode::Internal,
            format!("malformed {kind} payload: {err}"),
        )
    }
}

/// `strings`-style millisecond UTC timestamp: `2026-05-05T12:00:00.123Z`.
struct TimestampMillis<'a>(&'a DateTime<Utc>);

impl Serialize for TimestampMillis<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0.to_rfc3339_opts(SecondsFormat::Millis, true))
    }
}

/// Serde module for `DateTime<Utc>` in the contract's timestamp spelling.
pub mod timestamp_millis {
    use super::*;

    pub fn serialize<S: Serializer>(
        value: &DateTime<Utc>,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        TimestampMillis(value).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<DateTime<Utc>, D::Error> {
        let text = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&text)
            .map(|dt| dt.with_timezone(&Utc))
            .map_err(|err| D::Error::custom(format!("invalid RFC3339 timestamp {text:?}: {err}")))
    }
}

// ----------------------------------------------------------------- recorder ---

/// Serialises events to an append-only NDJSON sink, allocating `seq` for you.
///
/// The CLI uses this for stdout and `--record`, the Tauri shell will use it for
/// the per-session replay buffer (`princess:op:replay`).
pub struct EventRecorder<W: Write> {
    out: W,
    next_seq: u64,
    op_id: Option<String>,
}

impl<W: Write> EventRecorder<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            next_seq: 1,
            op_id: None,
        }
    }

    pub fn with_op_id(mut self, op_id: impl Into<String>) -> Self {
        self.op_id = Some(op_id.into());
        self
    }

    /// Set (or clear) the `opId` stamped on subsequent events.
    pub fn set_op_id(&mut self, op_id: Option<String>) {
        self.op_id = op_id;
    }

    pub fn op_id(&self) -> Option<&str> {
        self.op_id.as_deref()
    }

    /// The `seq` the next emitted event will carry.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Stamp, serialise and write one event, then flush.
    ///
    /// Flushing per event keeps the stream live for a UI reader; the cost is one
    /// `write` syscall per event, which is irrelevant next to building a kernel.
    pub fn emit(&mut self, body: EventBody) -> Result<Event> {
        self.emit_at(Utc::now(), body)
    }

    /// Same as [`EventRecorder::emit`] but with an explicit timestamp — used by
    /// tests that assert on exact JSON.
    pub fn emit_at(&mut self, ts: DateTime<Utc>, body: EventBody) -> Result<Event> {
        let event = Event::new(self.next_seq, ts, self.op_id.clone(), body);
        event.validate()?;
        self.next_seq += 1;
        self.write(&event)?;
        Ok(event)
    }

    /// Write an already-built event without re-stamping it (used when replaying
    /// a recorded stream).  `seq` still advances past it.
    pub fn record(&mut self, event: &Event) -> Result<()> {
        event.validate()?;
        if event.seq >= self.next_seq {
            self.next_seq = event.seq + 1;
        }
        self.write(event)
    }

    /// Convenience for the most common event: a chunk of text on a stream.
    pub fn log(&mut self, stream: LogStream, chunk: impl Into<String>) -> Result<Event> {
        self.emit(EventBody::LogAppend(LogAppendPayload {
            stream,
            chunk: chunk.into(),
            encoding: TextEncoding::Utf8,
        }))
    }

    /// Emit an `ide` log line, newline appended when missing.
    pub fn note(&mut self, text: impl Into<String>) -> Result<Event> {
        let mut text = text.into();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        self.log(LogStream::Ide, text)
    }

    fn write(&mut self, event: &Event) -> Result<()> {
        let line = event.to_ndjson()?;
        self.out
            .write_all(line.as_bytes())
            .and_then(|()| self.out.flush())
            .map_err(|err| PrincessError::internal(format!("cannot write event stream: {err}")))
    }

    pub fn into_inner(self) -> W {
        self.out
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }
}

// ------------------------------------------------------------------ helpers ---

/// Verify a recorded/replayed stream: version supported and `seq` strictly
/// increasing (contract §2: the UI detects dropped events with `seq`).
pub fn validate_stream(events: &[Event]) -> Result<()> {
    let mut last: Option<u64> = None;
    for event in events {
        event.validate()?;
        if let Some(previous) = last {
            if event.seq <= previous {
                return Err(PrincessError::new(
                    ErrorCode::Internal,
                    format!(
                        "event seq is not monotonic: {previous} then {} (kind {})",
                        event.seq,
                        event.kind()
                    ),
                ));
            }
        }
        last = Some(event.seq);
    }
    Ok(())
}

/// Parse an NDJSON event stream (one JSON object per line, blank lines skipped).
pub fn parse_ndjson(text: &str) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let event: Event = serde_json::from_str(line).map_err(|err| {
            PrincessError::new(
                ErrorCode::Internal,
                format!("line {} is not a valid PrincessIDE event: {err}", index + 1),
            )
            .with_detail(line.to_string())
        })?;
        events.push(event);
    }
    Ok(events)
}

/// A short, human-typable operation id: `op-7f3a`.
pub fn new_op_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0);
    format!("op-{:04x}", (nanos ^ (std::process::id() as u64) << 8) & 0xffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use chrono::TimeZone;

    fn ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 5, 12, 0, 0).unwrap() + chrono::Duration::milliseconds(123)
    }

    fn sample_bodies() -> Vec<EventBody> {
        vec![
            EventBody::LogAppend(LogAppendPayload {
                stream: LogStream::SerialCom1,
                chunk: "PrincessIDE reference kernel booted\n".into(),
                encoding: TextEncoding::Utf8,
            }),
            EventBody::BuildStarted(BuildStartedPayload {
                backend: "make".into(),
                toolchain_id: "host-gcc".into(),
                argv: vec!["make".into(), "iso".into()],
                cwd: "/root/PrincessIDE/fixtures/refkernel".into(),
            }),
            EventBody::BuildDiagnostic(BuildDiagnosticPayload {
                severity: DiagnosticSeverity::Error,
                file: Some("/root/PrincessIDE/fixtures/refkernel/kernel.c".into()),
                line: Some(12),
                col: Some(5),
                message: "expected expression before ';' token".into(),
                source: DiagnosticSource::Gcc,
            }),
            EventBody::BuildFinished(BuildFinishedPayload {
                status: BuildStatus::Ok,
                exit_code: Some(0),
                duration_ms: 1234,
                artifacts: vec![Artifact {
                    path: "/root/PrincessIDE/fixtures/refkernel/build/refkernel.elf".into(),
                    kind: ArtifactKind::Elf,
                    size: 23992,
                    sha256: "00ff".into(),
                }],
            }),
            EventBody::RunStarted(RunStartedPayload {
                qemu_argv: vec!["qemu-system-x86_64".into(), "-cdrom".into(), "x.iso".into()],
                gdb_stub: Some("127.0.0.1:1234".into()),
            }),
            EventBody::RunFault(RunFaultPayload {
                vector: "#UD".into(),
                rip: 0x10_0b3d,
                rip_text: "0x0000000000100b3d".into(),
                error_code: 0,
                regs: RegisterFile::from([
                    ("CS".to_string(), "0x0008".to_string()),
                    ("RIP".to_string(), "0x0000000000100b3d".to_string()),
                ]),
                symbolicated: Some(SymbolicatedLocation {
                    symbol: "refkernel_fault_probe".into(),
                    file: "/root/PrincessIDE/fixtures/refkernel/kernel.c".into(),
                    line: 100,
                }),
            }),
            EventBody::RunExited(RunExitedPayload {
                exit_code: None,
                reason: ExitReason::Timeout,
                uptime_ms: 15000,
            }),
            EventBody::DebugStopped(DebugStoppedPayload {
                reason: StopReason::Breakpoint,
                thread_id: 1,
                frame: StackFrame {
                    id: 0,
                    name: "kmain".into(),
                    source: Some(SourceLocation {
                        file: "/k/kernel.c".into(),
                        line: 42,
                        column: Some(1),
                    }),
                    instructionPointer: "0x100040".into(),
                },
                regs: RegisterFile::new(),
            }),
            EventBody::DebugBreakpointChanged(DebugBreakpointChangedPayload {
                id: "bp-1".into(),
                verified: true,
                location: Some(SourceLocation {
                    file: "/k/kernel.c".into(),
                    line: 42,
                    column: None,
                }),
            }),
            EventBody::DebugOutput(DebugOutputPayload {
                category: DebugOutputCategory::Console,
                text: "(gdb) break kmain\n".into(),
            }),
            EventBody::SymbolsIndexed(SymbolsIndexedPayload {
                artifact: "/k/refkernel.elf".into(),
                build_id: None,
                symbol_count: 42,
            }),
            EventBody::ArtifactChanged(ArtifactChangedPayload {
                path: "/k/refkernel.elf".into(),
                kind: ArtifactKind::Elf,
            }),
            EventBody::AiChunk(AiChunkPayload {
                request_id: "req-1".into(),
                text: "hello".into(),
            }),
            EventBody::AiFinished(AiFinishedPayload {
                request_id: "req-1".into(),
                usage: AiUsage {
                    promptTokens: 3,
                    completionTokens: 4,
                    totalTokens: 7,
                },
            }),
        ]
    }

    #[test]
    fn event_kind_strings_are_frozen() {
        let expected = [
            (EventKind::LogAppend, "log.append"),
            (EventKind::BuildStarted, "build.started"),
            (EventKind::BuildDiagnostic, "build.diagnostic"),
            (EventKind::BuildFinished, "build.finished"),
            (EventKind::RunStarted, "run.started"),
            (EventKind::RunFault, "run.fault"),
            (EventKind::RunExited, "run.exited"),
            (EventKind::DebugStopped, "debug.stopped"),
            (EventKind::DebugBreakpointChanged, "debug.breakpoint.changed"),
            (EventKind::DebugOutput, "debug.output"),
            (EventKind::SymbolsIndexed, "symbols.indexed"),
            (EventKind::ArtifactChanged, "artifact.changed"),
            (EventKind::AiChunk, "ai.chunk"),
            (EventKind::AiFinished, "ai.finished"),
        ];
        assert_eq!(EventKind::ALL.len(), expected.len());
        for (kind, text) in expected {
            assert_eq!(kind.as_str(), text);
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{text}\""));
            assert_eq!(EventKind::from_str_opt(text), Some(kind));
        }
    }

    #[test]
    fn envelope_shape_is_exactly_the_contract() {
        let event = Event::new(
            12345,
            ts(),
            Some("op-7f3a".into()),
            EventBody::LogAppend(LogAppendPayload {
                stream: LogStream::Build,
                chunk: "hi\n".into(),
                encoding: TextEncoding::Utf8,
            }),
        );
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"v":1,"seq":12345,"ts":"2026-05-05T12:00:00.123Z","opId":"op-7f3a","kind":"log.append","payload":{"stream":"build","chunk":"hi\n","encoding":"utf8"}}"#
        );
        // Key order is part of the contract's readability; assert it explicitly.
        let keys: Vec<&str> = json
            .split(',')
            .map(|part| part.split(':').next().unwrap().trim_matches(|c| c == '{' || c == '"'))
            .collect();
        assert_eq!(keys[0], "v");
        assert_eq!(keys[1], "seq");
        assert_eq!(keys[2], "ts");
        assert_eq!(keys[3], "opId");
        assert_eq!(keys[4], "kind");
        assert_eq!(keys[5], "payload");
    }

    #[test]
    fn null_op_id_is_written_as_null() {
        let event = Event::new(
            1,
            ts(),
            None,
            EventBody::RunExited(RunExitedPayload {
                exit_code: None,
                reason: ExitReason::Timeout,
                uptime_ms: 1,
            }),
        );
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""opId":null"#), "{json}");
        assert!(json.contains(r#""exitCode":null"#), "{json}");
        assert!(json.contains(r#""reason":"timeout""#), "{json}");
    }

    #[test]
    fn every_payload_kind_round_trips() {
        for (index, body) in sample_bodies().into_iter().enumerate() {
            let kind = body.kind();
            let event = Event::new(index as u64 + 1, ts(), None, body.clone());
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(back.body, body, "{kind} did not round-trip");
            assert_eq!(back.kind(), kind);
            assert_eq!(back.seq, index as u64 + 1);
            assert_eq!(back.ts, ts());
            assert_eq!(back.v, EVENT_MODEL_VERSION);
            // A second round-trip must be byte-identical (no field reordering).
            assert_eq!(serde_json::to_string(&back).unwrap(), json);
        }
    }

    #[test]
    fn ndjson_line_and_parse_round_trip() {
        let mut out: Vec<u8> = Vec::new();
        let mut rec = EventRecorder::new(&mut out).with_op_id("op-0001");
        for body in sample_bodies() {
            rec.emit_at(ts(), body).unwrap();
        }
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), EventKind::ALL.len());
        assert!(text.ends_with('\n'));
        for line in text.lines() {
            assert!(!line.contains('\n'));
            assert!(line.starts_with(r#"{"v":1,"seq":"#));
        }
        let events = parse_ndjson(&text).unwrap();
        assert_eq!(events.len(), EventKind::ALL.len());
        validate_stream(&events).unwrap();
        // ids and timestamps come from the recorder, not the payload
        assert!(events.iter().all(|e| e.op_id.as_deref() == Some("op-0001")));
        assert_eq!(events.first().unwrap().seq, 1);
        assert_eq!(events.last().unwrap().seq, EventKind::ALL.len() as u64);
    }

    #[test]
    fn seq_is_monotonic_and_validated() {
        let mut out: Vec<u8> = Vec::new();
        let mut rec = EventRecorder::new(&mut out);
        assert_eq!(rec.next_seq(), 1);
        let a = rec
            .log(LogStream::Ide, "one\n")
            .unwrap();
        let b = rec.note("two").unwrap();
        assert_eq!((a.seq, b.seq), (1, 2));
        assert!(matches!(&b.body, EventBody::LogAppend(p) if p.chunk == "two\n"));

        let events = parse_ndjson(&String::from_utf8(out).unwrap()).unwrap();
        validate_stream(&events).unwrap();

        // Duplicate seq must be rejected.
        let mut bad = events.clone();
        bad[1].seq = 1;
        let err = validate_stream(&bad).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(err.message.contains("not monotonic"), "{err}");

        // Unsupported model version must be rejected.
        let mut future = events.clone();
        future[0].v = 2;
        assert!(validate_stream(&future).is_err());
    }

    #[test]
    fn unknown_payload_fields_are_tolerated_but_wrong_types_are_not() {
        let line = r#"{"v":1,"seq":1,"ts":"2026-05-05T12:00:00.000Z","opId":null,"kind":"log.append","payload":{"stream":"ide","chunk":"x","encoding":"utf8","futureField":42}}"#;
        let event: Event = serde_json::from_str(line).unwrap();
        assert_eq!(event.kind(), EventKind::LogAppend);

        let bad = r#"{"v":1,"seq":1,"ts":"2026-05-05T12:00:00.000Z","opId":null,"kind":"log.append","payload":{"stream":"nope","chunk":"x","encoding":"utf8"}}"#;
        let err = serde_json::from_str::<Event>(bad).unwrap_err();
        assert!(err.to_string().contains("log.append"), "{err}");

        let bad_ts = r#"{"v":1,"seq":1,"ts":"yesterday","opId":null,"kind":"log.append","payload":{"stream":"ide","chunk":"x","encoding":"utf8"}}"#;
        assert!(serde_json::from_str::<Event>(bad_ts).is_err());
    }

    #[test]
    fn parse_ndjson_reports_the_offending_line() {
        let text = "{\"v\":1,\"seq\":1,\"ts\":\"2026-05-05T12:00:00.000Z\",\"opId\":null,\"kind\":\"log.append\",\"payload\":{\"stream\":\"ide\",\"chunk\":\"a\",\"encoding\":\"utf8\"}}\n\nnot json\n";
        let err = parse_ndjson(text).unwrap_err();
        assert!(err.message.contains("line 3"), "{err}");
        assert_eq!(err.detail.as_deref(), Some("not json"));
    }

    #[test]
    fn op_id_looks_like_the_contract_example() {
        let op = new_op_id();
        assert!(op.starts_with("op-"), "{op}");
        assert_eq!(op.len(), 7);
        assert!(op[3..].chars().all(|c| c.is_ascii_hexdigit()));
    }
}
