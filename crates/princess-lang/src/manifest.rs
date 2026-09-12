//! Language manifest parsing and validation.
//!
//! This module handles the TOML manifest format for language modules.
//! Each manifest describes a language's capabilities and configuration.
//!
//! ## Manifest Structure
//!
//! ```toml
//! [language]
//! id = "c"
//! displayName = "C"
//! extensions = [".c", ".h", ".S"]
//!
//! [language.lsp]
//! command = "clangd-16"
//! args = ["--background-index"]
//!
//! [language.build]
//! backend = "make"
//!
//! [language.run]
//! backend = "qemu-multiboot2"
//!
//! [language.debug]
//! backend = "gdb-dap"
//! ```

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;

use crate::{LanguageError, LanguageErrorCode, Result};

/// Valid file extensions (must start with a dot).
const VALID_EXTENSIONS: &[&str] = &[
    ".c", ".h", ".S", ".s", ".asm", ".java", ".kt", ".rs", ".cpp", ".cc", ".cxx", ".hpp", ".zig",
];

/// Known backend identifiers.
const KNOWN_BACKENDS: &[&str] = &[
    "make",
    "javac",
    "qemu-multiboot2",
    "jvm",
    "gdb-dap",
    "jdwp",
];

/// Top-level manifest structure.
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestFile {
    pub language: LanguageSection,
}

/// The `[language]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct LanguageSection {
    /// Unique language identifier (e.g., "c", "java").
    pub id: String,
    /// Human-readable display name.
    #[serde(rename = "displayName")]
    pub display_name: String,
    /// File extensions this language handles.
    pub extensions: Vec<String>,
    /// Optional description.
    #[serde(default)]
    pub description: Option<String>,
    /// LSP configuration.
    pub lsp: LspSection,
    /// Build configuration.
    pub build: BuildSection,
    /// Run configuration.
    pub run: RunSection,
    /// Debug configuration (optional).
    #[serde(default)]
    pub debug: Option<DebugSection>,
}

/// The `[language.lsp]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct LspSection {
    /// The language server command.
    pub command: String,
    /// Arguments to the language server.
    #[serde(default)]
    pub args: Vec<String>,
    /// Strategy for .clangd file generation.
    #[serde(default)]
    pub dot_clangd_strategy: Option<String>,
    /// Strategy for compile_commands.json generation.
    #[serde(default)]
    pub compile_commands_strategy: Option<String>,
}

/// The `[language.build]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct BuildSection {
    /// The build backend identifier.
    pub backend: String,
    /// Whether to generate compile_commands.json.
    #[serde(default)]
    pub cdb_generation: bool,
    /// Whether to generate .clangd file.
    #[serde(default)]
    pub dot_clangd_generation: bool,
    /// Whether to run make clean before CDB generation.
    #[serde(default)]
    pub clean_before_cdb: bool,
}

/// The `[language.run]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct RunSection {
    /// The run backend identifier.
    pub backend: String,
    /// Serial output arguments.
    #[serde(default)]
    pub serial_args: Vec<String>,
    /// Triple fault detection arguments.
    #[serde(default)]
    pub triple_fault_args: Vec<String>,
}

/// The `[language.debug]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct DebugSection {
    /// The debug backend identifier.
    pub backend: String,
}

/// Validated language manifest.
#[derive(Debug, Clone)]
pub struct LanguageManifest {
    /// Language identifier.
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// File extensions.
    pub extensions: Vec<String>,
    /// Description.
    pub description: Option<String>,
    /// LSP configuration.
    pub lsp: LspConfig,
    /// Build configuration.
    pub build: BuildConfig,
    /// Run configuration.
    pub run: RunConfig,
    /// Debug configuration.
    pub debug: Option<DebugConfig>,
}

/// Validated LSP configuration.
#[derive(Debug, Clone)]
pub struct LspConfig {
    /// Language server command.
    pub command: String,
    /// Arguments.
    pub args: Vec<String>,
    /// .clangd generation strategy.
    pub dot_clangd_strategy: Option<String>,
    /// compile_commands.json generation strategy.
    pub compile_commands_strategy: Option<String>,
}

/// Validated build configuration.
#[derive(Debug, Clone)]
pub struct BuildConfig {
    /// Build backend.
    pub backend: String,
    /// Generate CDB.
    pub cdb_generation: bool,
    /// Generate .clangd.
    pub dot_clangd_generation: bool,
    /// Clean before CDB.
    pub clean_before_cdb: bool,
}

/// Validated run configuration.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Run backend.
    pub backend: String,
    /// Serial args.
    pub serial_args: Vec<String>,
    /// Triple fault args.
    pub triple_fault_args: Vec<String>,
}

/// Validated debug configuration.
#[derive(Debug, Clone)]
pub struct DebugConfig {
    /// Debug backend.
    pub backend: String,
}

impl LanguageManifest {
    /// Parse and validate a manifest from a TOML string.
    pub fn from_toml(text: &str) -> Result<Self> {
        let file: ManifestFile = toml::from_str(text).map_err(|e| {
            LanguageError::new(
                LanguageErrorCode::ParseError,
                format!("failed to parse manifest TOML: {e}"),
            )
        })?;

        Self::validate_and_convert(file)
    }

    /// Parse and validate a manifest from a file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            LanguageError::new(
                LanguageErrorCode::IoError,
                format!("failed to read manifest {}: {e}", path.display()),
            )
        })?;
        Self::from_toml(&text)
    }

    /// Validate the raw manifest and convert to the validated form.
    fn validate_and_convert(file: ManifestFile) -> Result<Self> {
        let lang = &file.language;

        // Validate required fields
        if lang.id.is_empty() {
            return Err(LanguageError::new(
                LanguageErrorCode::MissingField,
                "language.id must not be empty",
            ));
        }
        if lang.display_name.is_empty() {
            return Err(LanguageError::new(
                LanguageErrorCode::MissingField,
                "language.displayName must not be empty",
            ));
        }
        if lang.extensions.is_empty() {
            return Err(LanguageError::new(
                LanguageErrorCode::MissingField,
                "language.extensions must not be empty",
            ));
        }
        if lang.lsp.command.is_empty() {
            return Err(LanguageError::new(
                LanguageErrorCode::MissingField,
                "language.lsp.command must not be empty",
            ));
        }
        if lang.build.backend.is_empty() {
            return Err(LanguageError::new(
                LanguageErrorCode::MissingField,
                "language.build.backend must not be empty",
            ));
        }
        if lang.run.backend.is_empty() {
            return Err(LanguageError::new(
                LanguageErrorCode::MissingField,
                "language.run.backend must not be empty",
            ));
        }

        // Validate extensions format
        for ext in &lang.extensions {
            if !ext.starts_with('.') {
                return Err(LanguageError::new(
                    LanguageErrorCode::InvalidValue,
                    format!("extension '{ext}' must start with a dot"),
                ));
            }
            // Optional: warn about uncommon extensions
        }

        // Validate backend identifiers
        Self::validate_backend(&lang.build.backend)?;
        Self::validate_backend(&lang.run.backend)?;
        if let Some(debug) = &lang.debug {
            Self::validate_backend(&debug.backend)?;
        }

        // Check for duplicate extensions
        let mut seen = HashSet::new();
        for ext in &lang.extensions {
            if !seen.insert(ext) {
                return Err(LanguageError::new(
                    LanguageErrorCode::InvalidValue,
                    format!("duplicate extension '{ext}'"),
                ));
            }
        }

        Ok(LanguageManifest {
            id: lang.id.clone(),
            display_name: lang.display_name.clone(),
            extensions: lang.extensions.clone(),
            description: lang.description.clone(),
            lsp: LspConfig {
                command: lang.lsp.command.clone(),
                args: lang.lsp.args.clone(),
                dot_clangd_strategy: lang.lsp.dot_clangd_strategy.clone(),
                compile_commands_strategy: lang.lsp.compile_commands_strategy.clone(),
            },
            build: BuildConfig {
                backend: lang.build.backend.clone(),
                cdb_generation: lang.build.cdb_generation,
                dot_clangd_generation: lang.build.dot_clangd_generation,
                clean_before_cdb: lang.build.clean_before_cdb,
            },
            run: RunConfig {
                backend: lang.run.backend.clone(),
                serial_args: lang.run.serial_args.clone(),
                triple_fault_args: lang.run.triple_fault_args.clone(),
            },
            debug: lang.debug.as_ref().map(|d| DebugConfig {
                backend: d.backend.clone(),
            }),
        })
    }

    /// Validate a backend identifier.
    fn validate_backend(backend: &str) -> Result<()> {
        if backend.is_empty() {
            return Err(LanguageError::new(
                LanguageErrorCode::MissingField,
                "backend must not be empty",
            ));
        }
        // We don't enforce that backend must be in KNOWN_BACKENDS here;
        // that check is done at the registry level when looking up adapters.
        Ok(())
    }

    /// Get the language id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Check if this language handles the given file extension.
    pub fn handles_extension(&self, ext: &str) -> bool {
        self.extensions.iter().any(|e| e == ext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_c_manifest() {
        let toml = r#"
[language]
id = "c"
displayName = "C"
extensions = [".c", ".h", ".S"]
description = "C language support"

[language.lsp]
command = "clangd-16"
args = ["--background-index", "--clang-tidy=false"]

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        let manifest = LanguageManifest::from_toml(toml).unwrap();
        assert_eq!(manifest.id, "c");
        assert_eq!(manifest.display_name, "C");
        assert_eq!(manifest.extensions, vec![".c", ".h", ".S"]);
        assert_eq!(manifest.lsp.command, "clangd-16");
        assert_eq!(manifest.build.backend, "make");
        assert_eq!(manifest.run.backend, "qemu-multiboot2");
    }

    #[test]
    fn missing_id_fails() {
        let toml = r#"
[language]
displayName = "C"
extensions = [".c"]

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        let err = LanguageManifest::from_toml(toml).unwrap_err();
        assert_eq!(err.code, LanguageErrorCode::ParseError);
    }

    #[test]
    fn missing_display_name_fails() {
        let toml = r#"
[language]
id = "c"
extensions = [".c"]

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        let err = LanguageManifest::from_toml(toml).unwrap_err();
        assert_eq!(err.code, LanguageErrorCode::ParseError);
    }

    #[test]
    fn missing_extensions_fails() {
        let toml = r#"
[language]
id = "c"
displayName = "C"

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        let err = LanguageManifest::from_toml(toml).unwrap_err();
        assert_eq!(err.code, LanguageErrorCode::ParseError);
    }

    #[test]
    fn extension_without_dot_fails() {
        let toml = r#"
[language]
id = "c"
displayName = "C"
extensions = ["c"]

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        let err = LanguageManifest::from_toml(toml).unwrap_err();
        assert_eq!(err.code, LanguageErrorCode::InvalidValue);
        assert!(err.message.contains("must start with a dot"));
    }

    #[test]
    fn empty_id_fails() {
        let toml = r#"
[language]
id = ""
displayName = "C"
extensions = [".c"]

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        let err = LanguageManifest::from_toml(toml).unwrap_err();
        assert_eq!(err.code, LanguageErrorCode::MissingField);
    }

    #[test]
    fn duplicate_extension_fails() {
        let toml = r#"
[language]
id = "c"
displayName = "C"
extensions = [".c", ".c"]

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        let err = LanguageManifest::from_toml(toml).unwrap_err();
        assert_eq!(err.code, LanguageErrorCode::InvalidValue);
        assert!(err.message.contains("duplicate extension"));
    }

    #[test]
    fn handles_extension_works() {
        let toml = r#"
[language]
id = "c"
displayName = "C"
extensions = [".c", ".h", ".S"]

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        let manifest = LanguageManifest::from_toml(toml).unwrap();
        assert!(manifest.handles_extension(".c"));
        assert!(manifest.handles_extension(".h"));
        assert!(manifest.handles_extension(".S"));
        assert!(!manifest.handles_extension(".java"));
        assert!(!manifest.handles_extension("c"));
    }

    #[test]
    fn parse_manifest_with_debug_section() {
        let toml = r#"
[language]
id = "c"
displayName = "C"
extensions = [".c"]

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"

[language.debug]
backend = "gdb-dap"
"#;
        let manifest = LanguageManifest::from_toml(toml).unwrap();
        assert!(manifest.debug.is_some());
        assert_eq!(manifest.debug.unwrap().backend, "gdb-dap");
    }

    #[test]
    fn parse_manifest_without_debug_section() {
        let toml = r#"
[language]
id = "c"
displayName = "C"
extensions = [".c"]

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        let manifest = LanguageManifest::from_toml(toml).unwrap();
        assert!(manifest.debug.is_none());
    }

    #[test]
    fn parse_real_c_manifest() {
        // Test with actual content from languages/c.toml
        let toml = r#"
[language]
id = "c"
displayName = "C"
extensions = [".c", ".h", ".S"]
description = "C language support with kernel-specialised clangd configuration"

[language.lsp]
command = "clangd-16"
args = ["--background-index", "--clang-tidy=false"]
dot_clangd_strategy = "generate"
compile_commands_strategy = "bear-or-shim"

[language.build]
backend = "make"
cdb_generation = true
dot_clangd_generation = true
clean_before_cdb = true

[language.run]
backend = "qemu-multiboot2"
serial_args = ["-display", "none", "-serial", "stdio", "-monitor", "none"]
triple_fault_args = ["-d", "cpu_reset"]

[language.debug]
backend = "gdb-dap"
"#;
        let manifest = LanguageManifest::from_toml(toml).unwrap();
        assert_eq!(manifest.id, "c");
        assert_eq!(manifest.lsp.command, "clangd-16");
        assert_eq!(manifest.lsp.args, vec!["--background-index", "--clang-tidy=false"]);
        assert!(manifest.build.cdb_generation);
        assert!(manifest.build.dot_clangd_generation);
        assert!(manifest.build.clean_before_cdb);
        assert_eq!(manifest.run.serial_args, vec!["-display", "none", "-serial", "stdio", "-monitor", "none"]);
    }
}
