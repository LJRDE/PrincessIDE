//! Opening a project: manifest (or documented defaults), resolved paths,
//! artifact discovery, and boot-medium selection.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use princess_core::config::{LoadedConfig, ProjectConfig, ResolvedProject};
use princess_core::hash::file_size_and_hash;
use princess_core::traits::BootMedium;
use princess_core::types::{Artifact, ArtifactKind};
use princess_core::{ErrorCode, PrincessError, Result};

use crate::env::Toolchain;

/// A project ready to build, run or symbolicate.
#[derive(Debug, Clone)]
pub struct Project {
    pub loaded: LoadedConfig,
    pub resolved: ResolvedProject,
    pub toolchain: Toolchain,
}

impl Project {
    /// Load `<dir>/princess.toml` (or documented defaults) and resolve it.
    pub fn open(dir: &Path) -> Result<Self> {
        let loaded = ProjectConfig::load_or_default(dir)?;
        let resolved = loaded.config.resolve(&loaded.root);
        let toolchain = Toolchain::discover(&resolved.root);
        Ok(Self {
            loaded,
            resolved,
            toolchain,
        })
    }

    pub fn name(&self) -> String {
        self.resolved.name()
    }

    /// A one-line description of where the configuration came from; surfaced to
    /// the user because the contract forbids silently using defaults.
    pub fn source_note(&self) -> String {
        match &self.loaded.source {
            princess_core::ConfigSource::Manifest(path) => {
                format!("config: {}", path.display())
            }
            princess_core::ConfigSource::Defaults { reason } => {
                format!("config: {reason}")
            }
        }
    }

    /// The ELF image used for symbolication: `[run] kernel`, else the ELF build
    /// artifact, else `[debug] symbols`.
    pub fn kernel_elf(&self, artifacts: &[Artifact]) -> Option<PathBuf> {
        if let Some(kernel) = &self.resolved.kernel {
            if kernel.is_file() {
                return Some(kernel.clone());
            }
        }
        // Prefer the *shallowest* ELF: a build tree often keeps a copy of the
        // image inside its boot staging directory (`build/iso/boot/kernel.elf`),
        // and the canonical artifact is the one next to the build root.
        if let Some(elf) = artifacts
            .iter()
            .filter(|a| a.kind == ArtifactKind::Elf && Path::new(&a.path).is_file())
            .min_by_key(|a| Path::new(&a.path).components().count())
        {
            return Some(PathBuf::from(&elf.path));
        }
        if let Some(symbols) = &self.resolved.symbols {
            if symbols.is_file() {
                return Some(symbols.clone());
            }
        }
        None
    }

    /// The boot medium: a built ISO when there is one (the only path that boots a
    /// 64-bit multiboot2 kernel under QEMU), else the ELF via `-kernel`.
    pub fn boot_medium(&self, artifacts: &[Artifact]) -> Result<BootMedium> {
        if let Some(iso) = artifacts.iter().find(|a| a.kind == ArtifactKind::Iso) {
            let path = PathBuf::from(&iso.path);
            if path.is_file() {
                return Ok(BootMedium::Iso(path));
            }
        }
        if let Some(disk) = artifacts.iter().find(|a| a.kind == ArtifactKind::Image) {
            let path = PathBuf::from(&disk.path);
            if path.is_file() {
                return Ok(BootMedium::Disk(path));
            }
        }
        if let Some(kernel) = self.kernel_elf(artifacts) {
            return Ok(BootMedium::Kernel(kernel));
        }
        Err(PrincessError::new(
            ErrorCode::NotFound,
            format!(
                "no bootable artifact in {}: build the project first (looked for an ISO, a raw image and an ELF)",
                self.resolved.root.display()
            ),
        ))
    }
}

/// Directories that are scanned for build products when the manifest does not
/// declare `artifacts` (the documented "discover what the build wrote" rule).
const ARTIFACT_DIRS: [&str; 3] = ["build", "out", "dist"];
/// How deep the artifact scan descends below those directories.
const ARTIFACT_SCAN_DEPTH: usize = 3;
/// Extensions the engine recognises as build products.
const ARTIFACT_EXTENSIONS: [&str; 4] = ["elf", "iso", "img", "bin"];

/// Collect the build artifacts: every declared path that exists, plus whatever
/// the build wrote into `build/`, `out/` or `dist/`.
///
/// `newer_than` restricts the *discovered* files to those the build just touched
/// (declared artifacts are always reported when they exist).  Nothing is
/// invented: a declared artifact that is missing is simply absent from the list,
/// and the build path logs a warning about it.
pub fn discover_artifacts(
    root: &Path,
    declared: &[PathBuf],
    newer_than: Option<SystemTime>,
) -> Vec<Artifact> {
    let mut paths: Vec<PathBuf> = Vec::new();

    for path in declared {
        if path.is_file() {
            paths.push(path.clone());
        }
    }

    for dir_name in ARTIFACT_DIRS {
        let dir = root.join(dir_name);
        if dir.is_dir() {
            scan_dir(&dir, ARTIFACT_SCAN_DEPTH, newer_than, &mut paths);
        }
    }

    paths.sort();
    paths.dedup();

    let mut artifacts = Vec::new();
    for path in paths {
        if !path.is_file() {
            continue;
        }
        match file_size_and_hash(&path) {
            Ok((size, sha256)) => artifacts.push(Artifact {
                kind: ArtifactKind::from_path(&path),
                path: path.to_string_lossy().into_owned(),
                size,
                sha256,
            }),
            // A file that vanished between the scan and the hash is reported as
            // "not an artifact" rather than as a fabricated zero-size one.
            Err(_) => continue,
        }
    }
    artifacts
}

fn scan_dir(dir: &Path, depth: usize, newer_than: Option<SystemTime>, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            scan_dir(&path, depth - 1, newer_than, out);
            continue;
        }
        let matches_extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| ARTIFACT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
            .unwrap_or(false);
        if !matches_extension {
            continue;
        }
        if let Some(threshold) = newer_than {
            if let Ok(modified) = meta.modified() {
                // One second of slack: filesystem timestamps are coarse.
                if modified + std::time::Duration::from_secs(1) < threshold {
                    continue;
                }
            }
        }
        out.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("princesside-proj-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn artifacts_are_discovered_from_the_build_directory() {
        let dir = temp_dir("artifacts");
        std::fs::create_dir_all(dir.join("build/iso/boot")).unwrap();
        std::fs::write(dir.join("build/kernel.elf"), b"elf").unwrap();
        std::fs::write(dir.join("build/kernel.iso"), b"iso").unwrap();
        std::fs::write(dir.join("build/iso/boot/kernel.elf"), b"copy").unwrap();
        std::fs::write(dir.join("build/kernel.o"), b"obj").unwrap(); // not an artifact kind
        std::fs::write(dir.join("build/notes.txt"), b"text").unwrap();

        let artifacts = discover_artifacts(&dir, &[], None);
        let names: Vec<String> = artifacts
            .iter()
            .map(|a| {
                Path::new(&a.path)
                    .strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec!["build/iso/boot/kernel.elf", "build/kernel.elf", "build/kernel.iso"]);
        assert_eq!(artifacts.iter().find(|a| a.path.ends_with("kernel.iso")).unwrap().kind, ArtifactKind::Iso);
        assert!(artifacts.iter().all(|a| a.size > 0 && a.sha256.len() == 64));

        // Only files touched after the threshold are reported.
        let future = SystemTime::now() + std::time::Duration::from_secs(60);
        assert!(discover_artifacts(&dir, &[], Some(future)).is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn declared_artifacts_are_reported_even_outside_the_scanned_dirs() {
        let dir = temp_dir("declared");
        std::fs::write(dir.join("image.elf"), b"x").unwrap();
        let artifacts = discover_artifacts(&dir, &[dir.join("image.elf"), dir.join("missing.elf")], None);
        assert_eq!(artifacts.len(), 1);
        assert!(artifacts[0].path.ends_with("image.elf"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn boot_medium_prefers_the_iso_then_the_elf() {
        let dir = temp_dir("medium");
        std::fs::create_dir_all(dir.join("build")).unwrap();
        std::fs::write(dir.join("build/k.elf"), b"elf").unwrap();
        std::fs::write(dir.join("build/k.iso"), b"iso").unwrap();

        let project = Project::open(&dir).map_err(|e| e.to_string());
        // No manifest and no Makefile -> the project refuses to open.  Create a
        // minimal manifest instead.
        assert!(project.is_err());
        std::fs::write(dir.join("princess.toml"), "schema = 1\n").unwrap();
        let project = Project::open(&dir).unwrap();
        let artifacts = discover_artifacts(&dir, &[], None);
        match project.boot_medium(&artifacts).unwrap() {
            BootMedium::Iso(path) => assert!(path.ends_with("k.iso")),
            other => panic!("expected the ISO, got {other:?}"),
        }

        std::fs::remove_file(dir.join("build/k.iso")).unwrap();
        let artifacts = discover_artifacts(&dir, &[], None);
        match project.boot_medium(&artifacts).unwrap() {
            BootMedium::Kernel(path) => assert!(path.ends_with("k.elf")),
            other => panic!("expected the ELF, got {other:?}"),
        }

        // Nothing to boot at all is E_NOT_FOUND, not a silent `-kernel ""`.
        std::fs::remove_file(dir.join("build/k.elf")).unwrap();
        let artifacts = discover_artifacts(&dir, &[], None);
        let err = project.boot_medium(&artifacts).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
