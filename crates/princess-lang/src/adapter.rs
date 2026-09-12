//! Language adapter trait interfaces.
//!
//! This module defines the adapter interfaces for language support.
//! Each adapter provides a specific capability (LSP, Build, Run, Debug).
//!
//! ## Adapter Types
//!
//! - [`LanguageLsp`]: Language server protocol support
//! - [`LanguageBuild`]: Build system integration
//! - [`LanguageRun`]: Runtime environment
//! - [`LanguageDebug`]: Debug protocol support
//!
//! ## Design
//!
//! Adapters are registered with the language registry and looked up
//! by backend identifier from the manifest. This allows the engine
//! to delegate to the appropriate implementation without hardcoding
//! language-specific logic.

use std::collections::BTreeMap;

/// Trait for language server protocol adapters.
pub trait LanguageLsp: Send + Sync {
    /// Get the language server command.
    fn command(&self) -> &str;

    /// Get the command arguments.
    fn args(&self) -> &[String];

    /// Get initialization options (if any).
    fn initialization_options(&self) -> Option<serde_json::Value> {
        None
    }
}

/// Trait for build system adapters.
pub trait LanguageBuild: Send + Sync {
    /// Get the build backend identifier.
    fn backend_id(&self) -> &str;

    /// Generate compile_commands.json (if supported).
    fn generate_compile_commands(&self) -> bool {
        false
    }

    /// Generate .clangd file (if supported).
    fn generate_dot_clangd(&self) -> bool {
        false
    }

    /// Run make clean before CDB generation (if supported).
    fn clean_before_cdb(&self) -> bool {
        false
    }
}

/// Trait for runtime environment adapters.
pub trait LanguageRun: Send + Sync {
    /// Get the run backend identifier.
    fn backend_id(&self) -> &str;

    /// Get serial output arguments.
    fn serial_args(&self) -> &[String] {
        &[]
    }

    /// Get triple fault detection arguments.
    fn triple_fault_args(&self) -> &[String] {
        &[]
    }
}

/// Trait for debug protocol adapters.
pub trait LanguageDebug: Send + Sync {
    /// Get the debug backend identifier.
    fn backend_id(&self) -> &str;
}

/// Combined language adapter.
pub trait LanguageAdapter: Send + Sync {
    /// Get the language id.
    fn language_id(&self) -> &str;

    /// Get the LSP adapter.
    fn lsp(&self) -> Option<&dyn LanguageLsp>;

    /// Get the build adapter.
    fn build(&self) -> Option<&dyn LanguageBuild>;

    /// Get the run adapter.
    fn run(&self) -> Option<&dyn LanguageRun>;

    /// Get the debug adapter.
    fn debug(&self) -> Option<&dyn LanguageDebug>;
}

/// Default implementation of LanguageLsp for C.
pub struct CLspAdapter {
    command: String,
    args: Vec<String>,
}

impl CLspAdapter {
    pub fn new() -> Self {
        Self {
            command: "clangd-16".to_string(),
            args: vec![
                "--background-index".to_string(),
                "--clang-tidy=false".to_string(),
            ],
        }
    }
}

impl LanguageLsp for CLspAdapter {
    fn command(&self) -> &str {
        &self.command
    }

    fn args(&self) -> &[String] {
        &self.args
    }
}

/// Default implementation of LanguageBuild for C.
pub struct CBuildAdapter {
    backend: String,
    cdb_generation: bool,
    dot_clangd_generation: bool,
    clean_before_cdb: bool,
}

impl CBuildAdapter {
    pub fn new() -> Self {
        Self {
            backend: "make".to_string(),
            cdb_generation: true,
            dot_clangd_generation: true,
            clean_before_cdb: true,
        }
    }
}

impl LanguageBuild for CBuildAdapter {
    fn backend_id(&self) -> &str {
        &self.backend
    }

    fn generate_compile_commands(&self) -> bool {
        self.cdb_generation
    }

    fn generate_dot_clangd(&self) -> bool {
        self.dot_clangd_generation
    }

    fn clean_before_cdb(&self) -> bool {
        self.clean_before_cdb
    }
}

/// Default implementation of LanguageRun for C.
pub struct CRunAdapter {
    backend: String,
    serial_args: Vec<String>,
    triple_fault_args: Vec<String>,
}

impl CRunAdapter {
    pub fn new() -> Self {
        Self {
            backend: "qemu-multiboot2".to_string(),
            serial_args: vec![
                "-display".to_string(),
                "none".to_string(),
                "-serial".to_string(),
                "stdio".to_string(),
                "-monitor".to_string(),
                "none".to_string(),
            ],
            triple_fault_args: vec!["-d".to_string(), "cpu_reset".to_string()],
        }
    }
}

impl LanguageRun for CRunAdapter {
    fn backend_id(&self) -> &str {
        &self.backend
    }

    fn serial_args(&self) -> &[String] {
        &self.serial_args
    }

    fn triple_fault_args(&self) -> &[String] {
        &self.triple_fault_args
    }
}

/// Default implementation of LanguageDebug for C.
pub struct CDebugAdapter {
    backend: String,
}

impl CDebugAdapter {
    pub fn new() -> Self {
        Self {
            backend: "gdb-dap".to_string(),
        }
    }
}

impl LanguageDebug for CDebugAdapter {
    fn backend_id(&self) -> &str {
        &self.backend
    }
}

/// Combined adapter for C language.
pub struct CAdapter {
    lsp: CLspAdapter,
    build: CBuildAdapter,
    run: CRunAdapter,
    debug: CDebugAdapter,
}

impl CAdapter {
    pub fn new() -> Self {
        Self {
            lsp: CLspAdapter::new(),
            build: CBuildAdapter::new(),
            run: CRunAdapter::new(),
            debug: CDebugAdapter::new(),
        }
    }
}

impl LanguageAdapter for CAdapter {
    fn language_id(&self) -> &str {
        "c"
    }

    fn lsp(&self) -> Option<&dyn LanguageLsp> {
        Some(&self.lsp)
    }

    fn build(&self) -> Option<&dyn LanguageBuild> {
        Some(&self.build)
    }

    fn run(&self) -> Option<&dyn LanguageRun> {
        Some(&self.run)
    }

    fn debug(&self) -> Option<&dyn LanguageDebug> {
        Some(&self.debug)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_adapter_has_correct_id() {
        let adapter = CAdapter::new();
        assert_eq!(adapter.language_id(), "c");
    }

    #[test]
    fn c_lsp_adapter_has_correct_command() {
        let adapter = CLspAdapter::new();
        assert_eq!(adapter.command(), "clangd-16");
        assert_eq!(
            adapter.args(),
            &["--background-index", "--clang-tidy=false"]
        );
    }

    #[test]
    fn c_build_adapter_has_correct_backend() {
        let adapter = CBuildAdapter::new();
        assert_eq!(adapter.backend_id(), "make");
        assert!(adapter.generate_compile_commands());
        assert!(adapter.generate_dot_clangd());
        assert!(adapter.clean_before_cdb());
    }

    #[test]
    fn c_run_adapter_has_correct_backend() {
        let adapter = CRunAdapter::new();
        assert_eq!(adapter.backend_id(), "qemu-multiboot2");
        assert_eq!(
            adapter.serial_args(),
            &["-display", "none", "-serial", "stdio", "-monitor", "none"]
        );
    }

    #[test]
    fn c_debug_adapter_has_correct_backend() {
        let adapter = CDebugAdapter::new();
        assert_eq!(adapter.backend_id(), "gdb-dap");
    }

    #[test]
    fn c_adapter_provides_all_subadapters() {
        let adapter = CAdapter::new();
        assert!(adapter.lsp().is_some());
        assert!(adapter.build().is_some());
        assert!(adapter.run().is_some());
        assert!(adapter.debug().is_some());
    }
}
