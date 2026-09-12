//! # princess-view — the view model layer for dual-frontend architecture
//!
//! This crate provides **pure-data view models** that both the Web (Tauri) and
//! native (Vulkan) frontends consume.  The key invariant (D30.3):
//!
//! > **Logic lives in the engine; both frontends only render.**
//!
//! Each view model is:
//! * **Zero-side-effect, no I/O** — constructors take in-memory engine data only.
//! * **`serde`-serialisable** — field order is stable (use `struct`, not `HashMap`).
//! * **JSON-roundtrip-stable** — two serialisations of the same input are byte-identical.
//!
//! ## Views
//!
//! | view | input | model |
//! |---|---|---|
//! | [`EventLog`] | event stream (`princess-core`) | paginated rows for virtualised scrolling |
//! | [`Source`] | source file content + diagnostics | per-line display with inline diagnostics |
//! | [`Hex`] | binary data | address-continuous hex dump rows |
//! | [`Disassembly`] | `princess-bin` disassembly + source lines | D20 side-by-side requirement |
//!
//! ## Contract note (P-E0b)
//!
//! The `princess:view:*` commands are **not** added to `docs/spec/10-contracts.md` §3
//! in this round.  P-E0b will perform the atomic migration of spec §3 + TS + Rust.
//! This round only defines the view models and their golden fixtures.

pub mod eventlog;
pub mod source;
pub mod hex;
pub mod disasm;

pub use eventlog::{EventLogBuilder, EventLogModel, EventLogRow};
pub use source::{SourceBuilder, SourceModel, SourceRow, SourceDiagnostic, DiagnosticInput};
pub use hex::{HexBuilder, HexModel, HexRow};
pub use disasm::{DisassemblyBuilder, DisassemblyModel, DisassemblyRow, DisassemblyInput};
