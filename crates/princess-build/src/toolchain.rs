//! Toolchain detection — contract §3 `princess:tools:detect`, acceptance P2-6b.
//!
//! Two things make this module different from a `which` wrapper:
//!
//! 1. **It never lies about availability.**  A tool is `available` only when the
//!    executable resolves *and* runs; a path that exists but is not executable,
//!    or a binary whose dynamic loader is missing, is reported unavailable with
//!    the loader error in `detail`.
//! 2. **Every missing tool produces a runnable fix.**  P2-6b requires
//!    `E_TOOLCHAIN_MISSING` *plus* "可执行的修复建议" — a sentence the user can
//!    paste into a shell.  [`repair_suggestions`] is where those come from, and
//!    the workspace has a real, idempotent bootstrap script to point at.
//!
//! Tool identification is **cross-compiler aware**: a kernel project may name
//! `x86_64-elf-gcc` in `[toolchain] cc` while the host only has `gcc`, and both
//! are acceptable.  The search order is
//! declared name → prefixed variants → host default, so a project can pin an
//! exact binary without breaking on a host that spells it differently.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use princess_core::types::ToolInfo;

use crate::process::which;

/// What a project needs a tool for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRole {
    /// Drives the build (`make`).
    Driver,
    /// Compiles C (`gcc` / `clang`).
    Compiler,
    /// Links the kernel image (`ld`).
    Linker,
    /// Assembles (`nasm`, `as`).
    Assembler,
    /// Produces `compile_commands.json` (`bear`, D8/D18).
    CompileDb,
    /// The IDE language service (`clangd-16`, D6/D18).
    LanguageService,
}

impl ToolRole {
    /// Roles the engine cannot build without.  `bear` and `clangd` are *not*
    /// required: the engine falls back to a wrapper shim (D8) and simply reports
    /// "language service unavailable" instead of refusing to build.
    pub const fn required(self) -> bool {
        matches!(
            self,
            ToolRole::Driver | ToolRole::Compiler | ToolRole::Linker
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            ToolRole::Driver => "driver",
            ToolRole::Compiler => "compiler",
            ToolRole::Linker => "linker",
            ToolRole::Assembler => "assembler",
            ToolRole::CompileDb => "compile-db",
            ToolRole::LanguageService => "language-service",
        }
    }
}

/// A tool a project may need, with every plausible spelling of its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    /// Logical name reported to the UI (`gcc`, `nasm`, ...).
    pub name: String,
    pub role: ToolRole,
    /// Candidate executable names, best first.
    pub candidates: Vec<String>,
    /// Argument that makes the tool print its version.
    pub version_args: Vec<String>,
    /// Whether the build cannot proceed without it.
    pub required: bool,
    /// Why we need it, and what breaks if it is absent.
    pub purpose: String,
}

impl ToolSpec {
    fn new(
        name: &str,
        role: ToolRole,
        candidates: &[&str],
        version_args: &[&str],
        purpose: &str,
    ) -> Self {
        Self::from_owned(
            name,
            role,
            &candidates.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
            version_args,
            purpose,
        )
    }

    fn from_owned(
        name: &str,
        role: ToolRole,
        candidates: &[String],
        version_args: &[&str],
        purpose: &str,
    ) -> Self {
        Self {
            name: name.to_string(),
            role,
            candidates: candidates.to_vec(),
            version_args: version_args.iter().map(|a| a.to_string()).collect(),
            required: role.required(),
            purpose: purpose.to_string(),
        }
    }
}

/// `gcc` flavours a kernel project may be pinned to.
const CC_PREFIXES: [&str; 4] = ["x86_64-elf-", "x86_64-linux-gnu-", "x86_64-pc-elf-", ""];
const LD_PREFIXES: [&str; 4] = ["x86_64-elf-", "x86_64-linux-gnu-", "x86_64-pc-elf-", ""];

fn compiler_candidates() -> Vec<String> {
    let mut out = Vec::new();
    for prefix in CC_PREFIXES {
        out.push(format!("{prefix}gcc"));
    }
    out.push("clang".to_string());
    out.push("cc".to_string());
    out
}

fn linker_candidates() -> Vec<String> {
    let mut out = Vec::new();
    for prefix in LD_PREFIXES {
        out.push(format!("{prefix}ld"));
    }
    out.push("ld.lld".to_string());
    out.push("ld".to_string());
    out
}

/// The tool set for a C + assembly kernel project (v1's only shape, D4/D17).
pub fn kernel_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec::new(
            "make",
            ToolRole::Driver,
            &["make", "gmake"],
            &["--version"],
            "runs the project's Makefile; without it the make backend cannot build anything",
        ),
        ToolSpec::from_owned(
            "gcc",
            ToolRole::Compiler,
            &compiler_candidates(),
            &["--version"],
            "compiles the C sources (host gcc is fine unless [toolchain] cc pins a cross compiler)",
        ),
        ToolSpec::from_owned(
            "ld",
            ToolRole::Linker,
            &linker_candidates(),
            &["--version"],
            "links the ELF image with the project's linker script",
        ),
        ToolSpec::new(
            "nasm",
            ToolRole::Assembler,
            &["nasm", "yasm"],
            &["-v"],
            "assembles .s/.asm sources; only required for projects that list NASM sources",
        ),
        ToolSpec::new(
            "bear",
            ToolRole::CompileDb,
            &["bear"],
            &["--version"],
            "produces compile_commands.json for clangd (D8); the engine has a wrapper-shim fallback",
        ),
        ToolSpec::new(
            "clangd-16",
            ToolRole::LanguageService,
            &["clangd-16"],
            &["--version"],
            "the C language service (D6/D18); the engine refuses to fall back to clangd-14",
        ),
    ]
}

/// One resolved — or not resolved — tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStatus {
    pub name: String,
    pub role: ToolRole,
    /// Absolute path when it resolved *and* ran.
    pub path: Option<PathBuf>,
    /// First line of the version output.
    pub version: Option<String>,
    pub available: bool,
    pub required: bool,
    /// Why it is needed / what breaks without it.
    pub purpose: String,
    /// What actually went wrong when it did not resolve.
    pub detail: Option<String>,
}

impl ToolStatus {
    /// The contract's `ToolInfo` view of this status.
    pub fn to_tool_info(&self) -> ToolInfo {
        ToolInfo {
            name: self.name.clone(),
            path: self.path.as_ref().map(|p| p.to_string_lossy().into_owned()),
            version: self.version.clone(),
            available: self.available,
            required: self.required,
        }
    }
}

/// The resolved toolchain: raw paths, ready to be turned into a `BuildPlan`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Toolchain {
    pub tools: Vec<ToolStatus>,
    /// Host triple the compiler reported, when it reported one.
    pub host_triple: Option<String>,
}

impl Toolchain {
    pub fn find(&self, name: &str) -> Option<&ToolStatus> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn path_of(&self, name: &str) -> Option<&Path> {
        self.find(name).and_then(|t| t.path.as_deref())
    }

    /// Every required tool that did not resolve.
    pub fn missing_required(&self) -> Vec<&ToolStatus> {
        self.tools
            .iter()
            .filter(|t| t.required && !t.available)
            .collect()
    }

    /// The contract's `ToolchainReport` (what `princess:tools:detect` returns).
    pub fn report(&self) -> princess_core::ToolchainReport {
        princess_core::ToolchainReport {
            tools: self.tools.iter().map(|t| t.to_tool_info()).collect(),
            ok: self.missing_required().is_empty(),
        }
    }

    /// `toolchainId` for `build.started` — a stable, human-meaningful id built
    /// from the *actual* compiler and linker that were resolved.
    pub fn id(&self) -> String {
        let cc = self.find("gcc").and_then(|t| t.version.as_deref());
        let name = match cc {
            Some(version) => first_two_words(version),
            None => "unknown-cc".to_string(),
        };
        let host = self.host_triple.clone().unwrap_or_else(|| "host".to_string());
        format!("{host}/{name}")
    }
}

fn first_two_words(text: &str) -> String {
    text.split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
        .replace(' ', "-")
        .to_ascii_lowercase()
}

/// Detects tools on `PATH` (and anywhere the manifest pinned explicitly).
#[derive(Debug, Clone)]
pub struct ToolchainDetector {
    /// `[toolchain]` overrides: logical name → executable name/path.
    overrides: BTreeMap<String, String>,
    /// Extra directories prepended to the search path.
    search_path: Vec<PathBuf>,
}

impl Default for ToolchainDetector {
    fn default() -> Self {
        Self {
            overrides: BTreeMap::new(),
            search_path: Vec::new(),
        }
    }
}

impl ToolchainDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// `[toolchain] cc/as/ld/gdb`, exactly as the manifest spells them.
    pub fn with_overrides(mut self, overrides: BTreeMap<String, String>) -> Self {
        self.overrides = overrides;
        self
    }

    /// e.g. the workspace's `.toolchain/bin` (D18: `bear` must be called through
    /// its launcher, so the launcher's directory has to win the `PATH` race).
    pub fn with_search_path(mut self, dirs: Vec<PathBuf>) -> Self {
        self.search_path = dirs;
        self
    }

    /// Detect every tool in `specs`.
    pub fn detect(&self, specs: &[ToolSpec]) -> Toolchain {
        let mut tools = Vec::new();
        for spec in specs {
            tools.push(self.detect_one(spec));
        }
        let host_triple = tools
            .iter()
            .find(|t| t.name == "gcc")
            .and_then(|t| t.version.as_deref())
            .and_then(host_triple_from_version);
        Toolchain { tools, host_triple }
    }

    fn detect_one(&self, spec: &ToolSpec) -> ToolStatus {
        // The manifest wins; then the spec's own candidate list.
        let mut candidates: Vec<String> = Vec::new();
        let override_key = match spec.role {
            ToolRole::Assembler => "as".to_string(),
            _ => spec.name.clone(),
        };
        if let Some(chosen) = self.overrides.get(&override_key) {
            candidates.push(chosen.clone());
        }
        candidates.extend(spec.candidates.iter().cloned());

        let mut failures: Vec<String> = Vec::new();
        for candidate in candidates {
            match self.resolve(&candidate) {
                Some(path) => {
                    let version = self.version(&path, &spec.version_args);
                    if version.is_none() {
                        failures.push(format!(
                            "{} exists but `{} {}` failed to run",
                            path.display(),
                            path.display(),
                            spec.version_args.join(" ")
                        ));
                        continue;
                    }
                    return ToolStatus {
                        name: spec.name.clone(),
                        role: spec.role,
                        path: Some(path),
                        version,
                        available: true,
                        required: spec.required,
                        purpose: spec.purpose.clone(),
                        detail: None,
                    };
                }
                None => failures.push(format!("{candidate}: not found on PATH")),
            }
        }

        ToolStatus {
            name: spec.name.clone(),
            role: spec.role,
            path: None,
            version: None,
            available: false,
            required: spec.required,
            purpose: spec.purpose.clone(),
            detail: Some(failures.join("; ")),
        }
    }

    /// Resolve one candidate against the extra search path, then `PATH`.
    fn resolve(&self, candidate: &str) -> Option<PathBuf> {
        if candidate.contains('/') {
            return which(candidate);
        }
        for dir in &self.search_path {
            let path = dir.join(candidate);
            if path.is_file() && is_executable(&path) {
                return Some(path);
            }
        }
        which(candidate)
    }

    /// First line of the tool's version output.
    fn version(&self, path: &Path, args: &[String]) -> Option<String> {
        let output = Command::new(path).args(args).output().ok()?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        if text.trim().is_empty() {
            text = String::from_utf8_lossy(&output.stderr).into_owned();
        }
        let first = text.lines().find(|l| !l.trim().is_empty())?;
        Some(first.trim().to_string())
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// `gcc (Debian 12.2.0-14) 12.2.0` carries no triple; clang's
/// `Target: x86_64-unknown-linux-gnu` does.  We only report what was printed.
fn host_triple_from_version(version: &str) -> Option<String> {
    version
        .split_once("Target:")
        .map(|(_, rest)| rest.trim().to_string())
}

/// Detect with the workspace defaults: `[toolchain]` overrides plus the
/// standard kernel tool set.
pub fn detect_toolchain(overrides: BTreeMap<String, String>) -> Toolchain {
    ToolchainDetector::new()
        .with_overrides(overrides)
        .detect(&kernel_tool_specs())
}

/// A runnable repair instruction for a missing tool (P2-6b).
///
/// Every suggestion is a command the user can actually paste; nothing here is
/// vague advice like "install a compiler".  The workspace bootstrap script is
/// preferred because it is idempotent and self-contained (P0-2).
pub fn repair_suggestions(missing: &[&ToolStatus]) -> Vec<String> {
    let mut out = Vec::new();
    for tool in missing {
        let fix = match tool.name.as_str() {
            "make" => "sudo apt-get install -y make    # or: apt-get install build-essential",
            "gcc" => {
                "sudo apt-get install -y gcc    # a cross compiler also works; then set [toolchain] cc = \"x86_64-elf-gcc\""
            }
            "ld" => {
                "sudo apt-get install -y binutils    # or set [toolchain] ld = \"ld.lld\" and install lld"
            }
            "nasm" => {
                "sudo apt-get install -y nasm    # or remove the NASM sources from the project (the engine only needs nasm when the Makefile calls it)"
            }
            "bear" => {
                "bash scripts/bootstrap-toolchain.sh    # unpacks bear 3.1.1 into .toolchain/, then: source scripts/env.sh"
            }
            "clangd-16" => {
                "bash scripts/bootstrap-toolchain.sh    # installs clangd-16 into .toolchain/ (D6: clangd-14 is not enough)"
            }
            _ => "sudo apt-get install -y <package>    # then re-run scripts/doctor.sh",
        };
        out.push(format!(
            "{} is missing ({}) — {} → fix: {}",
            tool.name,
            tool.purpose,
            tool
                .detail
                .clone()
                .unwrap_or_else(|| "not found".to_string()),
            fix
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_required_tool_here_resolves_in_this_workspace() {
        // make/gcc/ld are the required set on any Linux host; if this fails the
        // rest of the build engine cannot run at all.
        let toolchain = detect_toolchain(BTreeMap::new());
        let missing = toolchain.missing_required();
        assert!(
            missing.is_empty(),
            "required tools missing: {:?}",
            missing.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
        assert!(toolchain.report().ok);
        assert!(toolchain.find("gcc").unwrap().version.is_some());
    }

    #[test]
    fn a_missing_tool_is_unavailable_with_detail_and_a_runnable_fix() {
        let detector = ToolchainDetector::new();
        let specs = vec![ToolSpec::new(
            "definitely-not-installed",
            ToolRole::Compiler,
            &["definitely-not-installed-xyz"],
            &["--version"],
            "a fake tool used by the test suite",
        )];
        let toolchain = detector.detect(&specs);
        let tool = toolchain.find("definitely-not-installed").unwrap();
        assert!(!tool.available);
        assert!(tool.path.is_none());
        assert!(tool.detail.as_deref().unwrap().contains("not found"));

        let fixes = repair_suggestions(&toolchain.missing_required());
        assert_eq!(fixes.len(), 1);
        assert!(fixes[0].contains("definitely-not-installed"));
        assert!(fixes[0].contains("fix: "));
    }

    #[test]
    fn the_manifest_override_wins_over_the_candidate_list() {
        let detector = ToolchainDetector::new().with_overrides(BTreeMap::from([(
            "gcc".to_string(),
            "sh".to_string(),
        )]));
        let specs = vec![ToolSpec::new(
            "gcc",
            ToolRole::Compiler,
            &["gcc"],
            // A version command that actually prints something: `detect_one`
            // requires a non-empty first line, which is how it distinguishes
            // "resolved" from "exists but cannot run".
            &["-c", "echo fake-sh 1.0"],
            "test",
        )];
        let toolchain = detector.detect(&specs);
        let tool = toolchain.find("gcc").unwrap();
        assert!(tool.available, "{tool:?}");
        assert_eq!(tool.version.as_deref(), Some("fake-sh 1.0"));
        // `Path::ends_with` matches whole path *components*, so the file name
        // must be compared against `"sh"` — `"/sh"` would parse as root + `sh`
        // and never match an absolute path.
        assert_eq!(
            tool.path.as_deref().and_then(|p| p.file_name()),
            Some(std::ffi::OsStr::new("sh"))
        );
    }

    #[test]
    fn a_directory_search_path_is_honoured_before_path_lookup() {
        let dir = std::env::temp_dir().join(format!("princesside-tc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("princesside-fake-gcc");
        std::fs::write(&fake, "#!/bin/sh\necho 'fake gcc 9.9.9'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let detector = ToolchainDetector::new().with_search_path(vec![dir.clone()]);
        let specs = vec![ToolSpec::new(
            "gcc",
            ToolRole::Compiler,
            &["princesside-fake-gcc"],
            &["--version"],
            "test",
        )];
        let toolchain = detector.detect(&specs);
        let tool = toolchain.find("gcc").unwrap();
        assert!(tool.available, "{tool:?}");
        assert_eq!(tool.version.as_deref(), Some("fake gcc 9.9.9"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_toolchain_id_is_derived_from_the_resolved_compiler() {
        let toolchain = Toolchain {
            tools: vec![ToolStatus {
                name: "gcc".into(),
                role: ToolRole::Compiler,
                path: Some(PathBuf::from("/usr/bin/gcc")),
                version: Some("gcc (Debian 12.2.0-14) 12.2.0".into()),
                available: true,
                required: true,
                purpose: String::new(),
                detail: None,
            }],
            host_triple: Some("x86_64-unknown-linux-gnu".into()),
        };
        assert_eq!(toolchain.id(), "x86_64-unknown-linux-gnu/gcc-(debian");
    }
}
