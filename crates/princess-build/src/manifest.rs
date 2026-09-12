//! Language manifest integration for princess-build.
//!
//! This module provides helpers to read language configuration from
//! manifests rather than hardcoded constants. It integrates with the
//! princess-lang crate's manifest parsing.
//!
//! ## Purpose
//!
//! The M13 language module layer introduces manifests that describe
//! language support configuration. This module allows princess-build
//! to read from those manifests while maintaining backward compatibility
//! with the existing hardcoded constants.
//!
//! ## Migration Path
//!
//! 1. Phase 1 (current): Read from manifest, fallback to hardcoded
//! 2. Phase 2 (future): All configuration comes from manifests
//! 3. Phase 3 (future): Remove hardcoded constants

use std::path::Path;

use princess_lang::manifest::LanguageManifest;

/// Get the language service command from a manifest.
///
/// Returns the command and args from the `[language.lsp]` section.
/// Falls back to the legacy hardcoded command if the manifest cannot be read.
pub fn lang_service_command_from_manifest(manifest_path: &Path) -> Vec<String> {
    match LanguageManifest::from_file(manifest_path) {
        Ok(manifest) => {
            let mut cmd = vec![manifest.lsp.command.clone()];
            cmd.extend(manifest.lsp.args.clone());
            cmd
        }
        Err(_) => {
            // Fallback to legacy command
            crate::clangd::lang_service_command()
        }
    }
}

/// Get the language service command with full fallback chain.
///
/// Priority:
/// 1. `PRINCESSIDE_LANG_SERVICE_CLANGD` environment variable (D18)
/// 2. Language manifest (if path provided)
/// 3. Legacy hardcoded constant
pub fn lang_service_command_with_fallback(manifest_path: Option<&Path>) -> Vec<String> {
    // D18: Check environment variable first
    if let Ok(env_cmd) = std::env::var("PRINCESSIDE_LANG_SERVICE_CLANGD") {
        if !env_cmd.is_empty() {
            let mut cmd = vec![env_cmd];
            cmd.extend([
                "--background-index".to_string(),
                "--clang-tidy=false".to_string(),
            ]);
            return cmd;
        }
    }

    // Try manifest if path provided
    if let Some(path) = manifest_path {
        return lang_service_command_from_manifest(path);
    }

    // Fallback to legacy
    crate::clangd::lang_service_command()
}

/// Check if a manifest specifies that compile_commands.json should be generated.
pub fn should_generate_compile_commands(manifest_path: &Path) -> bool {
    match LanguageManifest::from_file(manifest_path) {
        Ok(manifest) => manifest.build.cdb_generation,
        Err(_) => true, // Default: generate (legacy behavior)
    }
}

/// Check if a manifest specifies that .clangd should be generated.
pub fn should_generate_dot_clangd(manifest_path: &Path) -> bool {
    match LanguageManifest::from_file(manifest_path) {
        Ok(manifest) => manifest.build.dot_clangd_generation,
        Err(_) => true, // Default: generate (legacy behavior)
    }
}

/// Check if a manifest specifies that make clean should run before CDB generation.
pub fn should_clean_before_cdb(manifest_path: &Path) -> bool {
    match LanguageManifest::from_file(manifest_path) {
        Ok(manifest) => manifest.build.clean_before_cdb,
        Err(_) => true, // Default: clean (legacy behavior, D18)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn lang_service_command_falls_back_to_legacy() {
        // Non-existent path should fall back to legacy
        let cmd = lang_service_command_from_manifest(Path::new("/nonexistent/c.toml"));
        assert_eq!(cmd[0], "clangd-16");
        assert!(cmd.contains(&"--background-index".to_string()));
    }

    #[test]
    fn lang_service_command_reads_from_manifest() {
        let dir = std::env::temp_dir().join(format!(
            "princess-build-manifest-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let toml = r#"
[language]
id = "c"
displayName = "C"
extensions = [".c"]

[language.lsp]
command = "custom-clangd"
args = ["--custom-flag"]

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        fs::write(dir.join("c.toml"), toml).unwrap();

        let cmd = lang_service_command_from_manifest(&dir.join("c.toml"));
        assert_eq!(cmd[0], "custom-clangd");
        assert!(cmd.contains(&"--custom-flag".to_string()));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn env_var_takes_priority() {
        // Save original env
        let original = std::env::var("PRINCESSIDE_LANG_SERVICE_CLANGD").ok();

        // Set env var
        std::env::set_var("PRINCESSIDE_LANG_SERVICE_CLANGD", "env-clangd");

        let cmd = lang_service_command_with_fallback(None);
        assert_eq!(cmd[0], "env-clangd");

        // Restore env
        match original {
            Some(v) => std::env::set_var("PRINCESSIDE_LANG_SERVICE_CLANGD", v),
            None => std::env::remove_var("PRINCESSIDE_LANG_SERVICE_CLANGD"),
        }
    }

    #[test]
    fn manifest_specifies_cdb_generation() {
        let dir = std::env::temp_dir().join(format!(
            "princess-build-cdb-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let toml = r#"
[language]
id = "c"
displayName = "C"
extensions = [".c"]

[language.lsp]
command = "clangd-16"

[language.build]
backend = "make"
cdb_generation = true
dot_clangd_generation = false
clean_before_cdb = true

[language.run]
backend = "qemu-multiboot2"
"#;
        fs::write(dir.join("c.toml"), toml).unwrap();

        assert!(should_generate_compile_commands(&dir.join("c.toml")));
        assert!(!should_generate_dot_clangd(&dir.join("c.toml")));
        assert!(should_clean_before_cdb(&dir.join("c.toml")));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_manifest_returns_defaults() {
        assert!(should_generate_compile_commands(Path::new("/nonexistent")));
        assert!(should_generate_dot_clangd(Path::new("/nonexistent")));
        assert!(should_clean_before_cdb(Path::new("/nonexistent")));
    }
}
