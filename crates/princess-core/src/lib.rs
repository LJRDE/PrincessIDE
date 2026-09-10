//! # princess-core
//!
//! The engine's domain core: everything the rest of the workspace is not allowed
//! to disagree about.  It implements the frozen interface contract
//! (`docs/spec/10-contracts.md`) and nothing else — no process spawning, no
//! QEMU, no DWARF, no UI.
//!
//! | module | contract section |
//! |---|---|
//! | [`event`] | §2 event stream: envelope, all v1 `kind`s, NDJSON recorder |
//! | [`error`] | §3 error model: closed code set, `Result`, IPC reply shape |
//! | [`config`] | §4 `princess.toml`: strict parsing, path resolution |
//! | [`traits`] | §5 backend extension points (static dispatch in v1) |
//!
//! Support modules: [`stream`] (UTF-8-safe chunking for `log.append`),
//! [`hash`] (SHA-256 for `build.finished.artifacts[]`), [`types`] (shared value
//! types).
//!
//! ## Guarantees this crate is responsible for
//!
//! * **Byte-exact JSON.**  Event envelopes are hand-serialised in contract
//!   order (`v`, `seq`, `ts`, `opId`, `kind`, `payload`) and payload payloads
//!   come straight from their `serde` derives; unit tests pin the exact strings.
//! * **Fail loud.**  A `princess.toml` with an unknown key, a missing `schema`
//!   or an unknown enum value is an `E_INVALID_CONFIG` error, never a silently
//!   ignored field.
//! * **No invented facts.**  Optional values are `null` rather than fabricated
//!   (e.g. `symbols.indexed.buildId` for an image linked with `--build-id=none`).

pub mod config;
pub mod error;
pub mod event;
pub mod hash;
pub mod stream;
pub mod traits;
pub mod types;

pub use config::{
    Arch, BuildBackendKind, ConfigSource, DebugBackendKind, Language, LoadedConfig, ProjectConfig,
    ResolvedProject, RunBackendKind, CONFIG_FILE_NAME, CONFIG_SCHEMA_VERSION,
    DEFAULT_RUN_TIMEOUT_MS,
};
pub use error::{ErrorCode, IpcResponse, PrincessError, Result};
pub use event::{
    parse_ndjson, validate_stream, AiChunkPayload, AiFinishedPayload, ArtifactChangedPayload,
    BuildDiagnosticPayload, BuildFinishedPayload, BuildStartedPayload,
    DebugBreakpointChangedPayload, DebugOutputCategory, DebugOutputPayload, DebugStoppedPayload,
    Event, EventBody, EventKind, EventRecorder, LogAppendPayload, RunExitedPayload, RunFaultPayload,
    RunStartedPayload, StopReason, SymbolsIndexedPayload, EVENT_MODEL_VERSION,
};
pub use stream::Utf8Chunker;
pub use traits::{
    classify_exit, AiProvider, BinProvider, BinaryFacts, BootMedium, BuildBackend, BuildPlan,
    CancelToken, DebugBackend, DebugCapabilities, EventSink, ExitFacts, RunBackend, RunOutcome,
    RunPlan,
};
pub use types::{
    AiUsage, Artifact, ArtifactKind, BreakpointSpec, BuildStatus, ChatMessage, ChatRequest,
    DiagnosticSeverity, DiagnosticSource, DisassembledInstruction, ExitReason, GdbStub, LogStream,
    RegisterFile, Scope, SectionInfo, SerialCapture, SourceLocation, StackFrame, StubMode,
    SymbolInfo, SymbolicatedLocation, TextEncoding, ToolInfo, ToolchainReport, Variable,
};

/// The engine version string reported by `princess:tools:detect` and by the
/// version-mismatch warning the UI must show (contract §7).
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
