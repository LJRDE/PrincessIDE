//! Language module layer (M13) — "one language = one manifest + one adapter set".
//!
//! This crate implements the language module abstraction for PrincessIDE.
//! Each language is described by a TOML manifest that specifies:
//! - Language metadata (id, display name, file extensions)
//! - LSP configuration (command, args, initialization options)
//! - Build backend (make, javac, etc.)
//! - Run backend (qemu, jvm, etc.)
//! - Debug backend (gdb-dap, jdwp, etc.)
//!
//! The manifest is loaded from `languages/*.toml` files and validated
//! against registered adapter implementations.
//!
//! ## Architecture
//!
//! - [`manifest`]: TOML manifest parsing and validation
//! - [`registry`]: Language registry (discovery, lookup, conflict detection)
//! - [`adapter`]: Adapter trait interfaces for LSP, Build, Run, Debug
//!
//! ## Design Decisions
//!
//! - **D31**: Language modules are pluggable, not forks
//! - **D4**: C+asm remains the primary path; language modules are optional
//! - **D17**: LSP protocol stack is not reimplemented; we delegate to existing infrastructure
//! - **Boundary rules 9-11**: Languages must not change kernel path behavior, must have their own gates, must not bring their own protocol stacks

pub mod manifest;
pub mod registry;
pub mod adapter;

pub use manifest::LanguageManifest;
pub use registry::LanguageRegistry;
pub use adapter::{LanguageAdapter, LanguageLsp, LanguageBuild, LanguageRun, LanguageDebug};

/// Result type for language module operations.
pub type Result<T> = std::result::Result<T, LanguageError>;

/// Error types for language module operations.
#[derive(Debug, Clone)]
pub struct LanguageError {
    pub code: LanguageErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanguageErrorCode {
    /// Required field is missing from the manifest.
    MissingField,
    /// Unknown field encountered (strict mode).
    UnknownField,
    /// Invalid field value.
    InvalidValue,
    /// Duplicate language id.
    DuplicateId,
    /// Adapter not found.
    AdapterNotFound,
    /// I/O error reading manifest file.
    IoError,
    /// TOML parse error.
    ParseError,
}

impl std::fmt::Display for LanguageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {}", self.code, self.message)
    }
}

impl std::error::Error for LanguageError {}

impl LanguageError {
    pub fn new(code: LanguageErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
