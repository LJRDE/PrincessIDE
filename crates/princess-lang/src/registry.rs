//! Language registry — discovery, lookup, and conflict detection.
//!
//! This module handles loading language manifests from directories,
//! validating them, and providing a registry for lookup by language id.
//!
//! ## Features
//!
//! - **Discovery**: Scan directories for `*.toml` files
//! - **Validation**: Parse and validate each manifest
//! - **Lookup**: Find languages by id or file extension
//! - **Conflict Detection**: Detect duplicate language ids
//!
//! ## Usage
//!
//! ```rust,no_run
//! use princess_lang::registry::LanguageRegistry;
//! use std::path::Path;
//!
//! let mut registry = LanguageRegistry::new();
//! registry.load_from_dir(Path::new("languages/")).unwrap();
//! let c_lang = registry.get("c");
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::adapter::{CAdapter, LanguageAdapter};
use crate::manifest::LanguageManifest;
use crate::{LanguageError, LanguageErrorCode, Result};

/// Language registry that holds loaded manifests and adapters.
///
/// # Example
///
/// ```rust,no_run
/// use princess_lang::registry::LanguageRegistry;
/// use std::path::Path;
///
/// let mut registry = LanguageRegistry::new();
/// registry.load_from_dir(Path::new("languages/")).unwrap();
/// let c_lang = registry.get("c");
/// ```
pub struct LanguageRegistry {
    /// Loaded manifests by language id.
    manifests: HashMap<String, LanguageManifest>,
    /// Registered adapters by language id.
    adapters: HashMap<String, Box<dyn LanguageAdapter>>,
    /// Paths that were scanned.
    scanned_paths: Vec<PathBuf>,
}

impl LanguageRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            manifests: HashMap::new(),
            adapters: HashMap::new(),
            scanned_paths: Vec::new(),
        }
    }

    /// Load all manifests from a directory.
    ///
    /// Scans for `*.toml` files and parses each one.
    /// Returns an error if any manifest is invalid or if duplicate ids are found.
    pub fn load_from_dir(&mut self, dir: &Path) -> Result<()> {
        if !dir.is_dir() {
            return Err(LanguageError::new(
                LanguageErrorCode::IoError,
                format!("{} is not a directory", dir.display()),
            ));
        }

        self.scanned_paths.push(dir.to_path_buf());

        let entries = std::fs::read_dir(dir).map_err(|e| {
            LanguageError::new(
                LanguageErrorCode::IoError,
                format!("failed to read directory {}: {e}", dir.display()),
            )
        })?;

        let mut loaded = 0;
        for entry in entries {
            let entry = entry.map_err(|e| {
                LanguageError::new(
                    LanguageErrorCode::IoError,
                    format!("failed to read directory entry: {e}"),
                )
            })?;

            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                self.load_from_file(&path)?;
                loaded += 1;
            }
        }

        if loaded == 0 {
            // Empty directory is not an error, but we note it
            eprintln!("warning: no language manifests found in {}", dir.display());
        }

        Ok(())
    }

    /// Load a single manifest from a file.
    ///
    /// Validates the manifest and checks for duplicate ids.
    pub fn load_from_file(&mut self, path: &Path) -> Result<()> {
        let manifest = LanguageManifest::from_file(path)?;

        // Check for duplicate id
        if self.manifests.contains_key(&manifest.id) {
            return Err(LanguageError::new(
                LanguageErrorCode::DuplicateId,
                format!(
                    "duplicate language id '{}' (already registered from another manifest)",
                    manifest.id
                ),
            ));
        }

        let id = manifest.id.clone();
        self.manifests.insert(id.clone(), manifest);

        // Register default adapter for known languages
        if id == "c" {
            self.adapters.insert(id, Box::new(CAdapter::new()));
        }

        Ok(())
    }

    /// Get a manifest by language id.
    pub fn get(&self, id: &str) -> Option<&LanguageManifest> {
        self.manifests.get(id)
    }

    /// Get an adapter by language id.
    pub fn get_adapter(&self, id: &str) -> Option<&dyn LanguageAdapter> {
        self.adapters.get(id).map(|a| a.as_ref())
    }

    /// Find a language that handles the given file extension.
    pub fn by_extension(&self, ext: &str) -> Option<&LanguageManifest> {
        self.manifests.values().find(|m| m.handles_extension(ext))
    }

    /// List all registered language ids.
    pub fn list_ids(&self) -> Vec<&str> {
        self.manifests.keys().map(|s| s.as_str()).collect()
    }

    /// Get the number of registered languages.
    pub fn len(&self) -> usize {
        self.manifests.len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.manifests.is_empty()
    }

    /// Get all registered manifests.
    pub fn all(&self) -> Vec<&LanguageManifest> {
        self.manifests.values().collect()
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_test_manifest(dir: &Path, id: &str, extensions: &[&str]) {
        let toml = format!(
            r#"
[language]
id = "{id}"
displayName = "Test {id}"
extensions = {extensions:?}

[language.lsp]
command = "test-lsp"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#
        );
        fs::write(dir.join(format!("{id}.toml")), toml).unwrap();
    }

    #[test]
    fn load_from_dir_succeeds() {
        let dir = std::env::temp_dir().join(format!("princess-lang-registry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        create_test_manifest(&dir, "c", &[".c", ".h"]);
        create_test_manifest(&dir, "java", &[".java"]);

        let mut registry = LanguageRegistry::new();
        registry.load_from_dir(&dir).unwrap();

        assert_eq!(registry.len(), 2);
        assert!(registry.get("c").is_some());
        assert!(registry.get("java").is_some());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_from_empty_dir_succeeds() {
        let dir = std::env::temp_dir().join(format!("princess-lang-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut registry = LanguageRegistry::new();
        registry.load_from_dir(&dir).unwrap();

        assert_eq!(registry.len(), 0);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_from_nonexistent_dir_fails() {
        let mut registry = LanguageRegistry::new();
        let err = registry
            .load_from_dir(Path::new("/nonexistent/path"))
            .unwrap_err();
        assert_eq!(err.code, LanguageErrorCode::IoError);
    }

    #[test]
    fn duplicate_id_fails() {
        let dir = std::env::temp_dir().join(format!("princess-lang-dup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        create_test_manifest(&dir, "c", &[".c"]);
        // Create another manifest with same id
        create_test_manifest(&dir, "c2", &[".c"]);

        let mut registry = LanguageRegistry::new();
        // First load succeeds
        registry.load_from_file(&dir.join("c.toml")).unwrap();
        // Second load with different file but same id should work (different id)
        registry.load_from_file(&dir.join("c2.toml")).unwrap();

        // Now create a manifest with duplicate id
        let toml = r#"
[language]
id = "c"
displayName = "Duplicate C"
extensions = [".c"]

[language.lsp]
command = "test-lsp"

[language.build]
backend = "make"

[language.run]
backend = "qemu-multiboot2"
"#;
        fs::write(dir.join("c_dup.toml"), toml).unwrap();

        let err = registry.load_from_file(&dir.join("c_dup.toml")).unwrap_err();
        assert_eq!(err.code, LanguageErrorCode::DuplicateId);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn get_by_id_works() {
        let dir = std::env::temp_dir().join(format!("princess-lang-get-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        create_test_manifest(&dir, "c", &[".c", ".h"]);

        let mut registry = LanguageRegistry::new();
        registry.load_from_dir(&dir).unwrap();

        let c = registry.get("c").unwrap();
        assert_eq!(c.id, "c");
        assert_eq!(c.display_name, "Test c");

        assert!(registry.get("nonexistent").is_none());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn by_extension_works() {
        let dir = std::env::temp_dir().join(format!("princess-lang-ext-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        create_test_manifest(&dir, "c", &[".c", ".h"]);
        create_test_manifest(&dir, "java", &[".java"]);

        let mut registry = LanguageRegistry::new();
        registry.load_from_dir(&dir).unwrap();

        let c = registry.by_extension(".c").unwrap();
        assert_eq!(c.id, "c");

        let java = registry.by_extension(".java").unwrap();
        assert_eq!(java.id, "java");

        assert!(registry.by_extension(".py").is_none());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn list_ids_works() {
        let dir = std::env::temp_dir().join(format!("princess-lang-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        create_test_manifest(&dir, "c", &[".c"]);
        create_test_manifest(&dir, "java", &[".java"]);

        let mut registry = LanguageRegistry::new();
        registry.load_from_dir(&dir).unwrap();

        let mut ids = registry.list_ids();
        ids.sort();
        assert_eq!(ids, vec!["c", "java"]);

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn c_adapter_is_registered() {
        let dir = std::env::temp_dir().join(format!("princess-lang-c-adapter-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        create_test_manifest(&dir, "c", &[".c"]);

        let mut registry = LanguageRegistry::new();
        registry.load_from_dir(&dir).unwrap();

        let adapter = registry.get_adapter("c");
        assert!(adapter.is_some());
        assert_eq!(adapter.unwrap().language_id(), "c");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn invalid_manifest_fails() {
        let dir = std::env::temp_dir().join(format!("princess-lang-invalid-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Write an invalid manifest
        fs::write(dir.join("invalid.toml"), "not valid toml").unwrap();

        let mut registry = LanguageRegistry::new();
        let err = registry.load_from_dir(&dir).unwrap_err();
        assert_eq!(err.code, LanguageErrorCode::ParseError);

        fs::remove_dir_all(&dir).unwrap();
    }
}
