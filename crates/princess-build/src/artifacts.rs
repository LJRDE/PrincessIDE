//! Artifact discovery — contract §4, *"`[build] artifacts` 为空数组时，引擎**发现**
//! 产物而不是报错。**引擎不得硬编码任何夹具文件名**"*.
//!
//! This is the module that makes that sentence true.  Nothing here knows that
//! `fixtures/refkernel/` produces `refkernel.elf` or that the template produces
//! `kernel.elf`: discovery is **extension-driven** over the conventional output
//! directories, and it is anchored to "what changed during this build" so a
//! stale binary from a previous run cannot be reported as a fresh artifact.
//!
//! Two rules keep it honest (both mirror `princess-cli`'s documented defaults so
//! the CLI and the engine cannot drift apart):
//!
//! * declared artifacts are always reported when they exist — the manifest is
//!   the user's explicit statement;
//! * discovered artifacts must be **newer than the build start** (with a second
//!   of slack for coarse filesystem timestamps), so `build.finished.artifacts[]`
//!   never claims credit for a file this build did not write.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use princess_core::hash::file_size_and_hash;
use princess_core::types::{Artifact, ArtifactKind};

/// Directories scanned when the manifest declares no artifacts.
pub const ARTIFACT_DIRS: [&str; 3] = ["build", "out", "dist"];

/// How deep the scan descends below those directories.  Deep enough for
/// `build/iso/boot/` (GRUB's staging tree), shallow enough to stay cheap.
pub const ARTIFACT_SCAN_DEPTH: usize = 3;

/// Extensions the engine recognises as build products.
pub const ARTIFACT_EXTENSIONS: [&str; 4] = ["elf", "iso", "img", "bin"];

/// Filesystem timestamps are coarse; treat "written in the last second" as new.
pub const TIMESTAMP_SLACK: Duration = Duration::from_secs(1);

/// A generation stamp for one build: capture it *before* spawning the child and
/// pass it to [`discover`].  Anything not touched after it is not ours.
pub fn snapshot_generation() -> SystemTime {
    SystemTime::now()
}

/// Collect the artifacts of one build.
///
/// * `declared` — `[build] artifacts`, absolute (from `ResolvedProject`).  A
///   declared path that does not exist is silently absent: the engine reports
///   what is there, it does not invent a zero-byte entry.
/// * `generation` — the timestamp from before the build, or `None` to accept
///   every candidate regardless of age (used by `princess:build:inspect` and by
///   tests on a pre-built tree).
pub fn discover(
    root: &Path,
    declared: &[PathBuf],
    generation: Option<SystemTime>,
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
            scan_dir(&dir, ARTIFACT_SCAN_DEPTH, generation, &mut paths);
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
            // The file vanished between the scan and the hash: report nothing
            // rather than fabricating a zero-size artifact.
            Err(_) => continue,
        }
    }
    artifacts
}

fn scan_dir(dir: &Path, depth: usize, generation: Option<SystemTime>, out: &mut Vec<PathBuf>) {
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
            scan_dir(&path, depth - 1, generation, out);
            continue;
        }
        if !has_artifact_extension(&path) {
            continue;
        }
        if let Some(threshold) = generation {
            if let Ok(modified) = meta.modified() {
                if modified + TIMESTAMP_SLACK < threshold {
                    continue;
                }
            }
        }
        out.push(path);
    }
}

/// Is this one of the extensions the engine treats as a build product?
pub fn has_artifact_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| ARTIFACT_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// The ELF images among a set of artifacts — what a run/symbol backend wants.
pub fn elf_artifacts(artifacts: &[Artifact]) -> Vec<&Artifact> {
    artifacts
        .iter()
        .filter(|a| a.kind == ArtifactKind::Elf)
        .collect()
}

/// The bootable images among a set of artifacts, most bootable first (ISO, then
/// raw image, then bare ELF — D2's GRUB-ISO path is the only one that boots a
/// 64-bit multiboot2 kernel under QEMU).
pub fn boot_candidates(artifacts: &[Artifact]) -> Vec<&Artifact> {
    let rank = |kind: ArtifactKind| match kind {
        ArtifactKind::Iso => 0,
        ArtifactKind::Image => 1,
        ArtifactKind::Elf => 2,
        _ => 3,
    };
    let mut candidates: Vec<&Artifact> = artifacts
        .iter()
        .filter(|a| matches!(a.kind, ArtifactKind::Iso | ArtifactKind::Image | ArtifactKind::Elf))
        .collect();
    candidates.sort_by_key(|a| (rank(a.kind), a.path.clone()));
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempTree(PathBuf);

    impl TempTree {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "princesside-artifacts-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempTree(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn discovery_finds_a_build_product_without_knowing_its_name() {
        let tree = TempTree::new("discover");
        std::fs::create_dir_all(tree.path().join("build/iso/boot")).unwrap();
        std::fs::write(tree.path().join("build/whatever-the-maker-called-it.elf"), b"\x7fELF")
            .unwrap();
        std::fs::write(tree.path().join("build/iso/boot/bootable.iso"), b"iso").unwrap();
        // Noise that must not be reported.
        std::fs::write(tree.path().join("build/kernel.o"), b"obj").unwrap();
        std::fs::write(tree.path().join("build/build.log"), b"log").unwrap();

        let artifacts = discover(tree.path(), &[], None);
        let names: Vec<String> = artifacts
            .iter()
            .map(|a| {
                Path::new(&a.path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec!["bootable.iso", "whatever-the-maker-called-it.elf"]);

        let elf = artifacts.iter().find(|a| a.kind == ArtifactKind::Elf).unwrap();
        assert_eq!(elf.size, 4);
        assert_eq!(elf.sha256.len(), 64);
    }

    #[test]
    fn a_stale_binary_is_not_claimed_as_this_builds_artifact() {
        let tree = TempTree::new("stale");
        std::fs::create_dir_all(tree.path().join("build")).unwrap();
        let stale = tree.path().join("build/old.elf");
        std::fs::write(&stale, b"old").unwrap();

        // The timestamp slack means "written within the last second" counts as
        // ours, which is what makes coarse filesystem clocks safe for a real
        // build.  To test staleness we must therefore actually let that second
        // elapse *before* taking the generation stamp.
        std::thread::sleep(TIMESTAMP_SLACK + Duration::from_millis(100));
        let generation = snapshot_generation();
        // The file was written before the generation stamp: not ours.
        let artifacts = discover(tree.path(), &[], Some(generation));
        assert!(artifacts.is_empty(), "{artifacts:?}");

        // After a rebuild it is new again.
        std::thread::sleep(TIMESTAMP_SLACK + Duration::from_millis(100));
        std::fs::write(&stale, b"fresh").unwrap();
        let artifacts = discover(tree.path(), &[], Some(generation));
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].size, 5);
    }

    #[test]
    fn declared_artifacts_are_reported_even_when_they_look_stale() {
        let tree = TempTree::new("declared");
        std::fs::create_dir_all(tree.path().join("build")).unwrap();
        let declared = tree.path().join("build/kernel.elf");
        std::fs::write(&declared, b"\x7fELF").unwrap();
        let generation = snapshot_generation();
        let artifacts = discover(tree.path(), &[declared.clone()], Some(generation));
        assert_eq!(artifacts.len(), 1, "{artifacts:?}");
        assert_eq!(artifacts[0].path, declared.to_string_lossy());
    }

    #[test]
    fn a_declared_artifact_that_does_not_exist_is_absent_not_fabricated() {
        let tree = TempTree::new("missing");
        let declared = tree.path().join("build/never-built.elf");
        let artifacts = discover(tree.path(), &[declared], None);
        assert!(artifacts.is_empty());
    }

    #[test]
    fn boot_candidates_prefer_the_iso_over_the_bare_elf() {
        let artifacts = vec![
            Artifact {
                path: "/b/kernel.elf".into(),
                kind: ArtifactKind::Elf,
                size: 1,
                sha256: String::new(),
            },
            Artifact {
                path: "/b/kernel.iso".into(),
                kind: ArtifactKind::Iso,
                size: 1,
                sha256: String::new(),
            },
        ];
        let candidates = boot_candidates(&artifacts);
        assert_eq!(candidates[0].path, "/b/kernel.iso");
        assert_eq!(candidates[1].path, "/b/kernel.elf");
    }
}
