//! Toolchain discovery and child-process environment.
//!
//! The workspace keeps its whole toolchain inside `.toolchain/` (see
//! `docs/p0-toolchain-report.md`).  `scripts/env.sh` activates it for a shell,
//! but a binary launched by a GUI, a test harness or `cargo run` from a clean
//! shell has no such guarantee — so the CLI reproduces exactly the same rules
//! here, and diagnostics stay honest: it only ever *prepends* directories that
//! exist inside the workspace toolchain, and it never modifies `/etc/ld.so.conf`
//! or the host's own libraries.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use princess_core::types::ToolInfo;

/// One entry of the `doctor` tool table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    /// Logical name reported to the UI.
    pub name: &'static str,
    /// Executable looked up on `PATH`.
    pub exe: &'static str,
    /// Arguments that print the version.
    pub version_args: &'static [&'static str],
    /// Whether a missing tool blocks the v1 workflow.
    pub required: bool,
}

/// Every tool the engine reports on (`princess:tools:detect`, contract §3).
pub const TOOLS: &[ToolSpec] = &[
    ToolSpec { name: "cargo", exe: "cargo", version_args: &["--version"], required: true },
    ToolSpec { name: "rustc", exe: "rustc", version_args: &["--version"], required: true },
    ToolSpec { name: "rustfmt", exe: "rustfmt", version_args: &["--version"], required: true },
    ToolSpec { name: "clippy", exe: "cargo-clippy", version_args: &["--version"], required: true },
    ToolSpec { name: "qemu-system-x86_64", exe: "qemu-system-x86_64", version_args: &["--version"], required: true },
    ToolSpec { name: "nasm", exe: "nasm", version_args: &["-v"], required: true },
    ToolSpec { name: "clang", exe: "clang", version_args: &["--version"], required: true },
    ToolSpec { name: "clangd", exe: "clangd", version_args: &["--version"], required: true },
    ToolSpec { name: "ld.lld", exe: "ld.lld", version_args: &["--version"], required: true },
    ToolSpec { name: "gdb", exe: "gdb", version_args: &["--version"], required: true },
    ToolSpec { name: "xorriso", exe: "xorriso", version_args: &["--version"], required: true },
    ToolSpec { name: "mtools", exe: "mformat", version_args: &["--version"], required: true },
    ToolSpec { name: "grub-mkrescue", exe: "grub-mkrescue", version_args: &["--version"], required: true },
    ToolSpec { name: "gcc", exe: "gcc", version_args: &["--version"], required: true },
    ToolSpec { name: "ld", exe: "ld", version_args: &["--version"], required: true },
    ToolSpec { name: "objdump", exe: "objdump", version_args: &["--version"], required: true },
    ToolSpec { name: "addr2line", exe: "addr2line", version_args: &["--version"], required: true },
    ToolSpec { name: "readelf", exe: "readelf", version_args: &["--version"], required: true },
    ToolSpec { name: "nm", exe: "nm", version_args: &["--version"], required: true },
    ToolSpec { name: "make", exe: "make", version_args: &["--version"], required: true },
    ToolSpec { name: "cmake", exe: "cmake", version_args: &["--version"], required: false },
];

/// A resolved view of the host environment the engine will run children in.
#[derive(Debug, Clone)]
pub struct Toolchain {
    /// Workspace root (the directory holding `scripts/env.sh`), when found.
    workspace_root: Option<PathBuf>,
    /// `PATH` for children, with the workspace toolchain prepended.
    path: OsString,
    /// `LD_LIBRARY_PATH` for children, or `None` to inherit as-is.
    ld_library_path: Option<OsString>,
    /// QEMU's `-L` data directory (BIOS/option ROMs), when it exists.
    qemu_data: Option<PathBuf>,
}

impl Toolchain {
    /// Discover the workspace toolchain relative to a project directory.
    ///
    /// Lookup order: ancestors of the project directory, then ancestors of this
    /// binary's source directory (so `--project /elsewhere` still finds the
    /// workspace toolchain), then the ambient environment.
    pub fn discover(project_root: &Path) -> Self {
        Self::discover_with_qemu_hint(project_root, std::env::var_os("PRINCESSIDE_QEMU_DATA"))
    }

    /// [`Toolchain::discover`] with the QEMU data-dir hint injected.
    ///
    /// Exists so the "a hint that is not a directory is discarded" rule can be
    /// tested without mutating the process environment: cargo runs tests in
    /// threads, and another test in this module depends on the real hint.
    pub(crate) fn discover_with_qemu_hint(project_root: &Path, qemu_hint: Option<OsString>) -> Self {
        let mut roots: Vec<PathBuf> = Vec::new();
        // Absolute start points: a relative `--project .` must not produce a
        // relative toolchain root, or every resolved tool path (and therefore
        // `build.started.argv`) would depend on the caller's cwd.
        let absolute_project = princess_core::config::absolute(project_root);
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for start in [absolute_project.as_path(), manifest_dir.as_path()] {
            let mut current = Some(start.to_path_buf());
            while let Some(dir) = current {
                if dir.join("scripts").join("env.sh").is_file() {
                    roots.push(dir.clone());
                    break;
                }
                current = dir.parent().map(Path::to_path_buf);
            }
        }
        // An explicitly exported workspace root wins over everything.
        if let Some(explicit) = std::env::var_os("PRINCESSIDE_ROOT") {
            let dir = princess_core::config::absolute(&PathBuf::from(explicit));
            if dir.join("scripts").join("env.sh").is_file() {
                roots.insert(0, dir);
            }
        }
        roots.dedup();
        let workspace_root = roots.first().cloned();

        let mut path_parts: Vec<OsString> = Vec::new();
        let mut ld_parts: Vec<OsString> = Vec::new();
        // The env var is a *hint*, not an override of the field's "when it
        // exists" rule.  `env.sh` exports the workspace path unconditionally, so
        // on a machine whose QEMU data lives in a system prefix this variable
        // points at a directory that is not there; taking it at face value made
        // the engine hand QEMU `-L <nonexistent>` and made this module's own test
        // fail on every layout without a workspace copy.  A non-directory hint is
        // discarded, exactly as the workspace-prefix branch below already does.
        let mut qemu_data = qemu_hint
            .map(|value| princess_core::config::absolute(&PathBuf::from(value)))
            .filter(|dir| dir.is_dir());

        if let Some(root) = &workspace_root {
            let toolchain = root.join(".toolchain");
            let prefix = toolchain.join("prefix");
            for dir in [
                toolchain.join("cargo").join("bin"),
                prefix.join("usr/lib/llvm-14/bin"),
                prefix.join("usr/bin"),
                prefix.join("bin"),
                prefix.join("usr/sbin"),
            ] {
                if dir.is_dir() {
                    path_parts.push(dir.into_os_string());
                }
            }
            for dir in [
                prefix.join("usr/lib/x86_64-linux-gnu"),
                prefix.join("lib/x86_64-linux-gnu"),
                prefix.join("usr/lib"),
                prefix.join("lib"),
                prefix.join("usr/lib/llvm-14/lib"),
            ] {
                if dir.is_dir() {
                    ld_parts.push(dir.into_os_string());
                }
            }
            let candidate = prefix.join("usr/share/qemu");
            if candidate.is_dir() && qemu_data.is_none() {
                qemu_data = Some(candidate);
            }
        }

        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let path = join_paths(&path_parts, &inherited_path);

        let inherited_ld = std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default();
        let ld_library_path = if ld_parts.is_empty() {
            None
        } else {
            Some(join_paths(&ld_parts, &inherited_ld))
        };

        Self {
            workspace_root,
            path,
            ld_library_path,
            qemu_data,
        }
    }

    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    pub fn path(&self) -> &OsString {
        &self.path
    }

    pub fn qemu_data_dir(&self) -> Option<&Path> {
        self.qemu_data.as_deref()
    }

    /// `which`, searching exactly the `PATH` children will see.
    pub fn which(&self, exe: &str) -> Option<PathBuf> {
        if exe.contains('/') {
            let candidate = PathBuf::from(exe);
            return candidate.is_file().then_some(candidate);
        }
        for dir in std::env::split_paths(&self.path) {
            let candidate = dir.join(exe);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// First line of `exe <version_args>`, or `None` when it cannot be run.
    pub fn version(&self, exe: &str, args: &[&str]) -> Option<String> {
        let path = self.which(exe)?;
        let output = self.command(&path).args(args).output().ok()?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        if text.trim().is_empty() {
            text = String::from_utf8_lossy(&output.stderr).into_owned();
        }
        text.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| line.to_string())
    }

    /// A `std::process::Command` with the toolchain environment applied.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.env("PATH", &self.path);
        if let Some(ld) = &self.ld_library_path {
            cmd.env("LD_LIBRARY_PATH", ld);
        }
        cmd
    }

    /// Apply the toolchain environment to a tokio child command.
    pub fn apply_tokio(&self, cmd: &mut tokio::process::Command) {
        cmd.env("PATH", &self.path);
        if let Some(ld) = &self.ld_library_path {
            cmd.env("LD_LIBRARY_PATH", ld);
        }
    }

    /// The full doctor table (`princess:tools:detect`).
    pub fn detect(&self) -> Vec<ToolInfo> {
        TOOLS
            .iter()
            .map(|spec| {
                let path = self.which(spec.exe);
                let available = path.is_some();
                let version = if available {
                    self.version(spec.exe, spec.version_args)
                } else {
                    None
                };
                ToolInfo {
                    name: spec.name.to_string(),
                    path: path.as_ref().map(|p| p.to_string_lossy().into_owned()),
                    version,
                    available,
                    required: spec.required,
                }
            })
            .collect()
    }
}

/// `prefix` first, then the inherited entries, with every directory kept once.
///
/// Idempotent by construction (the same rule as `scripts/env.sh`): sourcing or
/// discovering twice never stacks a directory, and a duplicated entry that was
/// already in the ambient `PATH` is collapsed too.
fn join_paths(prefix: &[OsString], inherited: &OsString) -> OsString {
    let mut parts: Vec<OsString> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in prefix
        .iter()
        .map(PathBuf::from)
        .chain(std::env::split_paths(inherited))
    {
        if seen.contains(&dir) {
            continue;
        }
        seen.push(dir.clone());
        parts.push(dir.into_os_string());
    }
    std::env::join_paths(parts).unwrap_or_else(|_| inherited.clone())
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_root_is_found_from_a_project_directory() {
        // The crate lives at <workspace>/crates/princess-cli.
        let toolchain = Toolchain::discover(Path::new(env!("CARGO_MANIFEST_DIR")));
        let root = toolchain.workspace_root().expect("workspace root");
        assert!(root.is_absolute(), "workspace root must be absolute: {}", root.display());
        assert!(root.join("scripts/env.sh").is_file(), "{}", root.display());
        assert!(root.join(".toolchain").is_dir());
    }

    #[test]
    fn toolchain_path_contains_the_workspace_bins_and_is_idempotent() {
        let toolchain = Toolchain::discover(Path::new(env!("CARGO_MANIFEST_DIR")));
        let root = toolchain.workspace_root().unwrap().to_path_buf();
        let dirs: Vec<PathBuf> = std::env::split_paths(toolchain.path()).collect();
        assert!(dirs.contains(&root.join(".toolchain/cargo/bin")));
        assert!(dirs.contains(&root.join(".toolchain/prefix/usr/bin")));
        assert!(dirs.contains(&root.join(".toolchain/prefix/usr/lib/llvm-14/bin")));

        // Discovering twice must not stack the same directory twice.
        let again = Toolchain::discover(Path::new(env!("CARGO_MANIFEST_DIR")));
        let mut sorted: Vec<String> = std::env::split_paths(again.path())
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let before = sorted.len();
        sorted.sort();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "PATH contains duplicates");
    }

    #[test]
    fn detects_the_host_toolchain_from_a_clean_environment() {
        let toolchain = Toolchain::discover(Path::new(env!("CARGO_MANIFEST_DIR")));
        let table = toolchain.detect();
        assert_eq!(table.len(), TOOLS.len());
        // cargo is what is running this test, so it must resolve.
        let cargo = table.iter().find(|t| t.name == "cargo").unwrap();
        assert!(cargo.available, "{cargo:?}");
        assert!(cargo.version.as_deref().unwrap_or("").contains("cargo"));
        assert!(cargo.path.as_deref().unwrap_or("").contains(".toolchain"));
        // Unknown tools are reported, not invented.
        assert!(toolchain.which("definitely-not-a-real-tool").is_none());
    }

    #[test]
    fn discovery_from_a_relative_project_directory_is_absolute() {
        // `--project .` (the CLI default) must not leak relative tool paths.
        let toolchain = Toolchain::discover(Path::new("."));
        let root = toolchain.workspace_root().expect("workspace root");
        assert!(root.is_absolute(), "{}", root.display());
        let dirs: Vec<PathBuf> = std::env::split_paths(toolchain.path()).collect();
        assert!(
            dirs.iter().all(|dir| dir.is_absolute() || !dir.starts_with(".")),
            "relative PATH entries leaked: {dirs:?}"
        );
    }

    #[test]
    fn qemu_data_dir_resolves_inside_the_workspace() {
        let toolchain = Toolchain::discover(Path::new(env!("CARGO_MANIFEST_DIR")));
        let root = toolchain.workspace_root().expect("workspace root");
        match toolchain.qemu_data_dir() {
            Some(data) => {
                // Discovery only reports a directory that exists...
                assert!(data.is_dir(), "{}", data.display());
                // ...and it is either the workspace copy, or exactly the env
                // hint.  A hint is an explicit override and may legitimately
                // point outside the workspace (a system prefix, say), so the
                // "inside the workspace" rule cannot be asserted unconditionally.
                let hinted = std::env::var_os("PRINCESSIDE_QEMU_DATA")
                    .map(|value| princess_core::config::absolute(&PathBuf::from(value)));
                assert!(
                    data.starts_with(&root) || hinted.as_deref() == Some(data),
                    "{} is neither inside {} nor the PRINCESSIDE_QEMU_DATA hint",
                    data.display(),
                    root.display()
                );
            }
            None => {
                // No QEMU data dir anywhere: the invariant is that nothing was
                // invented — and, with no usable hint, that the workspace copy
                // really is absent.  (This is the normal case for a machine whose
                // QEMU data lives in a system prefix; it used to panic here.)
                let hint_usable = std::env::var_os("PRINCESSIDE_QEMU_DATA")
                    .map(|value| princess_core::config::absolute(&PathBuf::from(value)).is_dir())
                    .unwrap_or(false);
                assert!(!hint_usable, "a usable hint was ignored");
                assert!(
                    !root.join(".toolchain/prefix/usr/share/qemu").is_dir(),
                    "a workspace QEMU data dir exists but discovery did not report it"
                );
            }
        }
    }

    #[test]
    fn a_hint_that_is_not_a_directory_is_discarded() {
        // Regression: `scripts/env.sh` exports PRINCESSIDE_QEMU_DATA
        // unconditionally, so on a machine without a workspace QEMU data dir the
        // hint points at nothing.  The engine used to trust it and hand QEMU
        // `-L <nonexistent>`; discovery must drop such a hint instead.
        let missing = std::env::temp_dir().join("princesside-definitely-not-a-qemu-data-dir");
        let toolchain = Toolchain::discover_with_qemu_hint(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            Some(missing.clone().into_os_string()),
        );
        assert_ne!(
            toolchain.qemu_data_dir().map(Path::to_path_buf),
            Some(missing),
            "a nonexistent QEMU data hint was trusted"
        );

        // ...while a real directory hint is still honoured.
        let existing = std::env::temp_dir();
        let toolchain = Toolchain::discover_with_qemu_hint(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            Some(existing.clone().into_os_string()),
        );
        assert_eq!(
            toolchain.qemu_data_dir().map(Path::to_path_buf),
            Some(existing),
            "an existing QEMU data hint must be used as-is"
        );
    }
}
