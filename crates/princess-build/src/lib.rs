//! # princess-build
//!
//! The PrincessIDE **build engine** (P2-B1).  It owns everything between "the
//! user pressed Build" and "the front end knows which ELF appeared on disk":
//!
//! | module | responsibility | spec |
//! |---|---|---|
//! | [`toolchain`] | find `make`/`gcc`/`ld`/`nasm`/`bear`/`clangd-16`, report versions, and produce `E_TOOLCHAIN_MISSING` **with a runnable fix** when something is absent | §3, P2-6b |
//! | [`diagnostics`] | turn `gcc` / `clang` / `ld` / `nasm` stderr into `build.diagnostic{severity,file,line,col,message,source}` | §2, P2-6 |
//! | [`process`] | spawn a child in **its own process group**, stream stdout/stderr separately through [`Utf8Chunker`], enforce the deadline, kill the *group* | §6 |
//! | [`backend`] | the `BuildBackend` implementation: `detect() / plan() / execute() / cancel()` plus artifact discovery | §5, P2-2 |
//! | [`artifacts`] | discover build products when the manifest declares none (never hard-code a fixture file name) | §4 |
//! | [`clangd`] | generate and validate the **kernel-specialised** `.clangd` (D7) and the `compile_commands.json` rules (D8/D18) | D7/D8/D17/D18 |
//!
//! ## What this crate deliberately does **not** do
//!
//! * It does not invent artifact names.  `[build] artifacts = []` means
//!   *discover*, and discovery is extension-driven ([`artifacts::discover`]).
//! * It does not label GNU `gcc` output as `clang`: the diagnostic source is
//!   derived from the producing binary (`DiagnosticSource::Gcc` is the contract's
//!   documented addition).
//! * It does not implement the LSP protocol stack (D17).  It prepares
//!   `.clangd` + `compile_commands.json` and hands the editor a command line.
//!
//! ## Events
//!
//! Everything is pushed through [`princess_core::EventSink`] — this crate never
//! touches the recorder, so the engine keeps ownership of `seq`/`ts`/`opId`.
//! Event order for one build:
//!
//! ```text
//! build.started → log.append(build) → build.diagnostic* → artifact.changed*
//!               → build.finished
//! ```
//!
//! `build.finished` is emitted on **every** path, including spawn failure and
//! cancellation (§6 rule 5).

pub mod artifacts;
pub mod backend;
pub mod clangd;
pub mod diagnostics;
pub mod javac;
pub mod manifest;
pub mod process;
pub mod toolchain;

pub use artifacts::{discover, snapshot_generation};
pub use backend::{
    build_project, describe_toolchain, diagnostics_in, launcher_dir, project_needs_nasm,
    shell_split, source_for_tool, BuildOutcome, BuildOutcomeError, MakeBackend,
    DEFAULT_BUILD_TIMEOUT_MS,
};
pub use clangd::{
    compile_commands_usable, lang_service_command, lang_service_command_from_manifest,
    lang_service_command_with_fallback, normalise_bear_output, validate_compile_commands,
    validate_dot_clangd, CompileCommand, CompileCommands, DotClangd, WrapperShim, DEFAULT_TRIPLE,
    GCC_ONLY_FLAG_BLACKLIST, GCC_ONLY_FLAGR_BLACKLIST, LANG_SERVICE_TOOL, NOSTD_INCLUDE_FLAG,
};
pub use javac::JavacBackend;
pub use manifest::{
    lang_service_command_with_fallback as manifest_lang_service_command_with_fallback,
    should_clean_before_cdb, should_generate_compile_commands, should_generate_dot_clangd,
};
pub use diagnostics::{parse_chunk, DiagnosticParser, ParsedDiagnostic, RawDiagnostic};
pub use process::{
    display_argv, kill_group, which, CommandOutput, CommandRunner, FakeRunner, StdioChunk,
    StdioStream, SystemRunner,
};
pub use toolchain::{
    detect_toolchain, kernel_tool_specs, repair_suggestions, tool_specs_from_manifest, ToolRole,
    ToolSpec, ToolStatus, Toolchain, ToolchainDetector,
};

/// The engine's build-event ordering, pinned in one place so the CLI, the Tauri
/// shell and the tests cannot disagree about it.
pub const BUILD_EVENT_ORDER: [&str; 5] = [
    "build.started",
    "log.append",
    "build.diagnostic",
    "artifact.changed",
    "build.finished",
];
