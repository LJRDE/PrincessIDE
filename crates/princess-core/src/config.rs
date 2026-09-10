//! `princess.toml` — the declarative project manifest (contract §4).
//!
//! Rules the contract fixes and this module enforces:
//!
//! * `schema = 1` is **required**; a manifest without a version is refused
//!   (`E_INVALID_CONFIG`) instead of being silently guessed at.
//! * **unknown keys are an error** (`E_INVALID_CONFIG`) — fail loud, never
//!   silently ignore a typo like `targts = [...]`.
//! * every relative path is relative to the *project root*;
//! * `~` and `$ENV` / `${ENV}` are expanded by the engine, not by the shell;
//! * missing optional fields fall back to engine defaults
//!   ([`ProjectConfig`] documents each one), which the UI is expected to label
//!   as "using default value".

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ErrorCode, PrincessError, Result};

/// The only manifest schema version this engine understands.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// Manifest file name at the project root.
pub const CONFIG_FILE_NAME: &str = "princess.toml";

/// Default run deadline when `[run] timeout_ms` is not set.
pub const DEFAULT_RUN_TIMEOUT_MS: u64 = 15_000;

/// Default boot protocol for x86_64 kernels (`[run] boot`).
pub const DEFAULT_BOOT: BootProtocol = BootProtocol::Multiboot2;

/// Default `[run] serial` block.
pub const DEFAULT_SERIAL_TEE: &str = "build/serial.log";

/// Source language of the project (`[project] language`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    #[serde(rename = "c")]
    C,
    #[serde(rename = "asm")]
    Asm,
    #[serde(rename = "cpp")]
    Cpp,
    #[serde(rename = "rust")]
    Rust,
    #[serde(rename = "zig")]
    Zig,
}

impl Default for Language {
    fn default() -> Self {
        Language::C
    }
}

/// Target architecture (`[project] arch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Arch {
    #[serde(rename = "x86_64")]
    X86_64,
    #[serde(rename = "i386")]
    I386,
    #[serde(rename = "aarch64")]
    Aarch64,
    #[serde(rename = "riscv64")]
    Riscv64,
}

impl Default for Arch {
    fn default() -> Self {
        Arch::X86_64
    }
}

/// Build backend selector (`[build] backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildBackendKind {
    #[serde(rename = "make")]
    Make,
    #[serde(rename = "cmake")]
    Cmake,
    #[serde(rename = "cargo")]
    Cargo,
    #[serde(rename = "zig")]
    Zig,
    /// Escape hatch: `backend = "custom"` + `command`.
    #[serde(rename = "custom")]
    Custom,
}

impl Default for BuildBackendKind {
    fn default() -> Self {
        BuildBackendKind::Make
    }
}

impl BuildBackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            BuildBackendKind::Make => "make",
            BuildBackendKind::Cmake => "cmake",
            BuildBackendKind::Cargo => "cargo",
            BuildBackendKind::Zig => "zig",
            BuildBackendKind::Custom => "custom",
        }
    }
}

/// Run backend selector (`[run] backend`).  Only QEMU exists in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunBackendKind {
    #[serde(rename = "qemu")]
    Qemu,
}

impl Default for RunBackendKind {
    fn default() -> Self {
        RunBackendKind::Qemu
    }
}

impl RunBackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            RunBackendKind::Qemu => "qemu",
        }
    }
}

/// Boot protocol (`[run] boot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BootProtocol {
    #[serde(rename = "multiboot1")]
    Multiboot1,
    #[serde(rename = "multiboot2")]
    Multiboot2,
    #[serde(rename = "uefi")]
    Uefi,
    #[serde(rename = "raw")]
    Raw,
}

impl BootProtocol {
    pub const fn as_str(self) -> &'static str {
        match self {
            BootProtocol::Multiboot1 => "multiboot1",
            BootProtocol::Multiboot2 => "multiboot2",
            BootProtocol::Uefi => "uefi",
            BootProtocol::Raw => "raw",
        }
    }
}

/// Debug backend selector (`[debug] backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebugBackendKind {
    #[serde(rename = "gdb")]
    Gdb,
}

impl Default for DebugBackendKind {
    fn default() -> Self {
        DebugBackendKind::Gdb
    }
}

impl DebugBackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            DebugBackendKind::Gdb => "gdb",
        }
    }
}

/// AI provider selector (`[ai] provider`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AiProviderKind {
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    /// Explicitly no provider; `princess:ai:*` must answer `E_AI_UNAVAILABLE`.
    #[serde(rename = "none")]
    None,
}

impl Default for AiProviderKind {
    fn default() -> Self {
        AiProviderKind::None
    }
}

impl AiProviderKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            AiProviderKind::OpenAiCompatible => "openai-compatible",
            AiProviderKind::None => "none",
        }
    }
}

// ----------------------------------------------------------------- sections ---

/// `[project]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    /// Defaults to the project directory name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub language: Language,
    #[serde(default)]
    pub arch: Arch,
}

impl Default for ProjectSection {
    fn default() -> Self {
        Self {
            name: None,
            language: Language::default(),
            arch: Arch::default(),
        }
    }
}

/// `[build]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSection {
    #[serde(default)]
    pub backend: BuildBackendKind,
    /// Overrides the default invocation (`make`, `cmake --build build`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Defaults to `.` (the project root).
    #[serde(default = "default_dot")]
    pub cwd: PathBuf,
    /// Backend targets; empty means "the backend's own default target".
    #[serde(default)]
    pub targets: Vec<String>,
    /// Declared build products.  Empty means "discover what the build wrote".
    #[serde(default)]
    pub artifacts: Vec<PathBuf>,
    /// Compilation database consumed by clangd.
    #[serde(
        default = "default_compile_commands",
        skip_serializing_if = "Option::is_none"
    )]
    pub compile_commands: Option<PathBuf>,
    /// Build parallelism.  When `None`, the engine injects **nothing** and lets
    /// make/cargo decide; when set, the engine injects `MAKEFLAGS=-jN` and
    /// `CARGO_BUILD_JOBS=N`.  (D25: dev-box memory discipline must not leak
    /// into product behaviour.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jobs: Option<u32>,
}

fn default_dot() -> PathBuf {
    PathBuf::from(".")
}

/// `[build] compile_commands` default: `compile_commands.json` at the root.
fn default_compile_commands() -> Option<PathBuf> {
    Some(PathBuf::from("compile_commands.json"))
}

impl Default for BuildSection {
    fn default() -> Self {
        Self {
            backend: BuildBackendKind::default(),
            command: None,
            cwd: default_dot(),
            targets: Vec::new(),
            artifacts: Vec::new(),
            compile_commands: Some(PathBuf::from("compile_commands.json")),
            jobs: None,
        }
    }
}

/// `[run] serial`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialSection {
    /// Serial device the engine captures.  `com1` in v1.
    #[serde(default = "default_com1")]
    pub device: String,
    /// Every captured byte is written here before its event is emitted
    /// (contract §2: serial must be on disk before it is pushed).
    #[serde(default = "default_serial_tee", skip_serializing_if = "Option::is_none")]
    pub tee_to_file: Option<PathBuf>,
}

fn default_com1() -> String {
    "com1".to_string()
}

/// `[run] serial.tee_to_file` default: `build/serial.log`.
fn default_serial_tee() -> Option<PathBuf> {
    Some(PathBuf::from(DEFAULT_SERIAL_TEE))
}

impl Default for SerialSection {
    fn default() -> Self {
        Self {
            device: default_com1(),
            tee_to_file: Some(PathBuf::from(DEFAULT_SERIAL_TEE)),
        }
    }
}

/// `[run]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSection {
    #[serde(default)]
    pub backend: RunBackendKind,
    /// Kernel image.  Defaults to the ELF build artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<PathBuf>,
    #[serde(default = "default_boot")]
    pub boot: BootProtocol,
    /// Extra emulator arguments.  Serial/monitor/display flags are owned by the
    /// engine and are stripped from here (they are reported when stripped).
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub serial: SerialSection,
}

fn default_boot() -> BootProtocol {
    DEFAULT_BOOT
}

fn default_timeout_ms() -> u64 {
    DEFAULT_RUN_TIMEOUT_MS
}

impl Default for RunSection {
    fn default() -> Self {
        Self {
            backend: RunBackendKind::default(),
            kernel: None,
            boot: default_boot(),
            args: Vec::new(),
            timeout_ms: default_timeout_ms(),
            serial: SerialSection::default(),
        }
    }
}

/// `[debug] stub`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StubSection {
    #[serde(default = "default_stub_host")]
    pub host: String,
    #[serde(default = "default_stub_port")]
    pub port: u16,
    #[serde(default)]
    pub mode: crate::types::StubMode,
}

fn default_stub_host() -> String {
    "127.0.0.1".to_string()
}

fn default_stub_port() -> u16 {
    1234
}

impl Default for StubSection {
    fn default() -> Self {
        Self {
            host: default_stub_host(),
            port: default_stub_port(),
            mode: crate::types::StubMode::Launch,
        }
    }
}

/// `[debug]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DebugSection {
    #[serde(default)]
    pub backend: DebugBackendKind,
    /// Symbol file; defaults to the same ELF the run uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbols: Option<PathBuf>,
    #[serde(default)]
    pub stub: StubSection,
}

impl Default for DebugSection {
    fn default() -> Self {
        Self {
            backend: DebugBackendKind::default(),
            symbols: None,
            stub: StubSection::default(),
        }
    }
}

/// `[toolchain]` — explicit tool overrides.  Unset means "whatever the engine
/// detects on `PATH`".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<String>,
    #[serde(rename = "as", default, skip_serializing_if = "Option::is_none")]
    pub assembler: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ld: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gdb: Option<String>,
}

/// `[ai]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiSection {
    #[serde(default)]
    pub provider: AiProviderKind,
    /// Empty means "use the global application setting".
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
}

impl Default for AiSection {
    fn default() -> Self {
        Self {
            provider: AiProviderKind::default(),
            base_url: String::new(),
            model: String::new(),
        }
    }
}

// ----------------------------------------------------------------- manifest ---

/// A parsed `princess.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    /// Manifest schema version.  Required: a versionless manifest is refused.
    pub schema: u32,
    #[serde(default)]
    pub project: ProjectSection,
    #[serde(default)]
    pub build: BuildSection,
    #[serde(default)]
    pub run: RunSection,
    #[serde(default)]
    pub debug: DebugSection,
    #[serde(default)]
    pub toolchain: ToolchainSection,
    #[serde(default)]
    pub ai: AiSection,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            schema: CONFIG_SCHEMA_VERSION,
            project: ProjectSection::default(),
            build: BuildSection::default(),
            run: RunSection::default(),
            debug: DebugSection::default(),
            toolchain: ToolchainSection::default(),
            ai: AiSection::default(),
        }
    }
}

/// Where a [`ProjectConfig`] came from — surfaced to the UI so "using default
/// values" is visible instead of implicit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    /// Parsed from this file.
    Manifest(PathBuf),
    /// No manifest: the engine synthesised documented defaults.
    Defaults {
        /// One-line explanation of why the defaults were synthesised.
        reason: String,
    },
}

impl fmt::Display for ConfigSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigSource::Manifest(path) => write!(f, "{}", path.display()),
            ConfigSource::Defaults { reason } => write!(f, "engine defaults ({reason})"),
        }
    }
}

/// A config plus its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedConfig {
    pub config: ProjectConfig,
    pub source: ConfigSource,
    /// Absolute project root the config was loaded for.
    pub root: PathBuf,
}

impl ProjectConfig {
    /// Parse a manifest, mapping every failure to `E_INVALID_CONFIG`.
    ///
    /// Unknown keys and a missing/invalid `schema` are errors, per §4.
    pub fn from_toml_str(text: &str) -> Result<ProjectConfig> {
        let config: ProjectConfig = toml::from_str(text).map_err(|err| {
            let message = err.message().to_string();
            let mut error = PrincessError::new(
                ErrorCode::InvalidConfig,
                format!("{CONFIG_FILE_NAME} is invalid: {message}"),
            );
            if let Some(span) = err.span() {
                let line = text[..span.start.min(text.len())].lines().count().max(1);
                error = error.with_detail(format!(
                    "at line {line}: {}",
                    text.lines().nth(line - 1).unwrap_or("").trim()
                ));
            }
            error
        })?;

        if config.schema != CONFIG_SCHEMA_VERSION {
            return Err(PrincessError::new(
                ErrorCode::InvalidConfig,
                format!(
                    "unsupported {CONFIG_FILE_NAME} schema {}: this engine implements schema {} \
                     (refusing to guess at an unknown format)",
                    config.schema, CONFIG_SCHEMA_VERSION
                ),
            ));
        }
        Ok(config)
    }

    /// Read `<root>/princess.toml`.
    pub fn load(root: impl AsRef<Path>) -> Result<LoadedConfig> {
        let root = root.as_ref();
        let path = root.join(CONFIG_FILE_NAME);
        let text = std::fs::read_to_string(&path).map_err(|err| {
            PrincessError::new(
                ErrorCode::InvalidConfig,
                format!("cannot read {}: {err}", path.display()),
            )
        })?;
        Ok(LoadedConfig {
            config: ProjectConfig::from_toml_str(&text)?,
            source: ConfigSource::Manifest(path),
            root: root.to_path_buf(),
        })
    }

    /// Load the manifest for a project directory, or synthesise documented
    /// defaults when the directory has none.
    ///
    /// The reference fixture (`fixtures/refkernel/`) deliberately ships without
    /// a manifest, so `--project <dir>` must still work on it.  The synthesised
    /// config is *explicit* about what it invents:
    ///
    /// * `build.backend = "make"`, targets `["iso"]` when the `Makefile` has an
    ///   `iso:` target (GRUB ISO path, required for x86_64 multiboot2),
    /// * `build.artifacts = []` → the engine records what the build actually
    ///   wrote (see `princess-build::discover_artifacts`),
    /// * `run.kernel` unset → the ELF build artifact,
    /// * `run.timeout_ms = 15000`, serial teed to `build/serial.log`.
    ///
    /// The caller receives [`ConfigSource::Defaults`] and is expected to tell
    /// the user; nothing is silently assumed.
    pub fn load_or_default(root: impl AsRef<Path>) -> Result<LoadedConfig> {
        let root = root.as_ref();
        let manifest = root.join(CONFIG_FILE_NAME);
        if manifest.is_file() {
            return ProjectConfig::load(root);
        }

        if !root.is_dir() {
            return Err(PrincessError::not_found(format!(
                "project directory {} does not exist",
                root.display()
            )));
        }

        let makefile = root.join("Makefile");
        if !makefile.is_file() {
            return Err(PrincessError::new(
                ErrorCode::InvalidConfig,
                format!(
                    "{} has no {CONFIG_FILE_NAME} and no Makefile: the engine cannot guess how to build it",
                    root.display()
                ),
            ));
        }

        let makefile_text = std::fs::read_to_string(&makefile).unwrap_or_default();
        let has_iso_target = makefile_text
            .lines()
            .any(|line| line.trim_start().starts_with("iso:"));
        let mut config = ProjectConfig::default();
        config.build.backend = BuildBackendKind::Make;
        config.build.targets = if has_iso_target {
            vec!["iso".to_string()]
        } else {
            Vec::new()
        };
        config.build.artifacts = Vec::new();
        config.run.args = vec![
            "-m".to_string(),
            "256M".to_string(),
            "-display".to_string(),
            "none".to_string(),
            "-no-reboot".to_string(),
        ];

        Ok(LoadedConfig {
            config,
            source: ConfigSource::Defaults {
                reason: format!(
                    "no {CONFIG_FILE_NAME} in {}; using Makefile-based engine defaults{}",
                    root.display(),
                    if has_iso_target { " with target `iso`" } else { "" }
                ),
            },
            root: root.to_path_buf(),
        })
    }

    /// Resolve every path in this config against `root` (contract §4:
    /// relative paths are relative to the project root; `~` and `$ENV` are
    /// expanded by the engine).
    pub fn resolve(&self, root: impl AsRef<Path>) -> ResolvedProject {
        let root = absolute(root.as_ref());
        let build_cwd = resolve_path(&self.build.cwd, &root);
        ResolvedProject {
            root: root.clone(),
            build_cwd,
            build_command: self.build.command.clone(),
            targets: self.build.targets.clone(),
            declared_artifacts: self
                .build
                .artifacts
                .iter()
                .map(|p| resolve_path(p, &root))
                .collect(),
            compile_commands: self
                .build
                .compile_commands
                .as_ref()
                .map(|p| resolve_path(p, &root)),
            kernel: self.run.kernel.as_ref().map(|p| resolve_path(p, &root)),
            serial_tee_to_file: self
                .run
                .serial
                .tee_to_file
                .as_ref()
                .map(|p| resolve_path(p, &root)),
            symbols: self.debug.symbols.as_ref().map(|p| resolve_path(p, &root)),
            config: self.clone(),
        }
    }
}

/// A [`ProjectConfig`] with every path made absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProject {
    /// Absolute project root.
    pub root: PathBuf,
    /// Absolute build working directory.
    pub build_cwd: PathBuf,
    /// `[build] command` override, verbatim.
    pub build_command: Option<String>,
    /// `[build] targets`.
    pub targets: Vec<String>,
    /// `[build] artifacts`, absolute.
    pub declared_artifacts: Vec<PathBuf>,
    /// `[build] compile_commands`, absolute.
    pub compile_commands: Option<PathBuf>,
    /// `[run] kernel`, absolute.
    pub kernel: Option<PathBuf>,
    /// `[run] serial.tee_to_file`, absolute.
    pub serial_tee_to_file: Option<PathBuf>,
    /// `[debug] symbols`, absolute.
    pub symbols: Option<PathBuf>,
    /// The config this was resolved from.
    pub config: ProjectConfig,
}

impl ResolvedProject {
    /// Project name: `[project] name`, else the directory name.
    pub fn name(&self) -> String {
        self.config
            .project
            .name
            .clone()
            .or_else(|| {
                self.root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "project".to_string())
    }

    /// Environment overrides the build backend should apply, if any.
    pub fn build_env(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
}

// ------------------------------------------------------------------- paths ----

/// Make a path absolute without touching the filesystem semantics
/// (`std::fs::canonicalize` would fail for paths that do not exist yet).
pub fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return normalize(path);
    }
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    normalize(&base.join(path))
}

/// Lexically normalise `.` and `..` components (no symlink resolution).
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Expand `~`, `~user` (as `$HOME`) and `$VAR` / `${VAR}` in a config path, then
/// resolve it relative to `root`.
///
/// An unset variable expands to the empty string — the engine must not invent a
/// value; the resulting path simply will not exist and the failing operation
/// reports it with the original text in `detail`.
pub fn resolve_path(path: &Path, root: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let expanded = expand_env(&text);
    let expanded_path = Path::new(&expanded);
    let joined = if expanded_path.is_absolute() {
        expanded_path.to_path_buf()
    } else {
        root.join(expanded_path)
    };
    normalize(&joined)
}

/// Shell-style `~` / `$VAR` expansion without a shell.
pub fn expand_env(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    // Leading `~` (or `~/...`) expands to $HOME.
    if text == "~" || text.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            out.push_str(&home.to_string_lossy());
        }
        if text == "~" {
            return out;
        }
        chars.next(); // the '~'
    }

    while let Some(ch) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some('{') => {
                chars.next();
                let mut name = String::new();
                let mut closed = false;
                for inner in chars.by_ref() {
                    if inner == '}' {
                        closed = true;
                        break;
                    }
                    name.push(inner);
                }
                if closed {
                    out.push_str(&std::env::var(&name).unwrap_or_default());
                } else {
                    // Unterminated ${...}: keep the text verbatim so the user
                    // sees exactly what they typed.
                    out.push_str("${");
                    out.push_str(&name);
                }
            }
            Some(next) if next.is_ascii_alphabetic() || *next == '_' => {
                let mut name = String::new();
                while let Some(c) = chars.peek() {
                    if c.is_ascii_alphanumeric() || *c == '_' {
                        name.push(*c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                out.push_str(&std::env::var(&name).unwrap_or_default());
            }
            // A lone `$` is literal.
            _ => out.push('$'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example manifest from `docs/spec/10-contracts.md` §4, verbatim.
    const CONTRACT_EXAMPLE: &str = r#"
schema = 1

[project]
name = "mykernel"
language = "c"
arch = "x86_64"

[build]
backend  = "make"
command  = "make"
cwd      = "."
targets  = ["all"]
artifacts = ["build/kernel.elf"]
compile_commands = "compile_commands.json"

[run]
backend   = "qemu"
kernel    = "build/kernel.elf"
boot      = "multiboot1"
args      = ["-m", "512M", "-serial", "stdio", "-display", "none", "-no-reboot"]
timeout_ms = 15000
serial    = { device = "com1", tee_to_file = "build/serial.log" }

[debug]
backend = "gdb"
symbols = "build/kernel.elf"
stub    = { host = "127.0.0.1", port = 1234, mode = "launch" }

[toolchain]
cc = "x86_64-elf-gcc"
as = "nasm"
ld = "x86_64-elf-ld"
gdb = "gdb"

[ai]
provider = "openai-compatible"
base_url = ""
model    = ""
"#;

    #[test]
    fn contract_example_parses_field_by_field() {
        let cfg = ProjectConfig::from_toml_str(CONTRACT_EXAMPLE).unwrap();
        assert_eq!(cfg.schema, 1);
        assert_eq!(cfg.project.name.as_deref(), Some("mykernel"));
        assert_eq!(cfg.project.language, Language::C);
        assert_eq!(cfg.project.arch, Arch::X86_64);
        assert_eq!(cfg.build.backend, BuildBackendKind::Make);
        assert_eq!(cfg.build.command.as_deref(), Some("make"));
        assert_eq!(cfg.build.cwd, PathBuf::from("."));
        assert_eq!(cfg.build.targets, vec!["all".to_string()]);
        assert_eq!(cfg.build.artifacts, vec![PathBuf::from("build/kernel.elf")]);
        assert_eq!(
            cfg.build.compile_commands,
            Some(PathBuf::from("compile_commands.json"))
        );
        assert_eq!(cfg.run.backend, RunBackendKind::Qemu);
        assert_eq!(cfg.run.kernel, Some(PathBuf::from("build/kernel.elf")));
        assert_eq!(cfg.run.boot, BootProtocol::Multiboot1);
        assert_eq!(
            cfg.run.args,
            vec!["-m", "512M", "-serial", "stdio", "-display", "none", "-no-reboot"]
        );
        assert_eq!(cfg.run.timeout_ms, 15000);
        assert_eq!(cfg.run.serial.device, "com1");
        assert_eq!(
            cfg.run.serial.tee_to_file,
            Some(PathBuf::from("build/serial.log"))
        );
        assert_eq!(cfg.debug.backend, DebugBackendKind::Gdb);
        assert_eq!(cfg.debug.symbols, Some(PathBuf::from("build/kernel.elf")));
        assert_eq!(cfg.debug.stub.host, "127.0.0.1");
        assert_eq!(cfg.debug.stub.port, 1234);
        assert_eq!(cfg.debug.stub.mode, crate::types::StubMode::Launch);
        assert_eq!(cfg.toolchain.cc.as_deref(), Some("x86_64-elf-gcc"));
        assert_eq!(cfg.toolchain.assembler.as_deref(), Some("nasm"));
        assert_eq!(cfg.toolchain.ld.as_deref(), Some("x86_64-elf-ld"));
        assert_eq!(cfg.toolchain.gdb.as_deref(), Some("gdb"));
        assert_eq!(cfg.ai.provider, AiProviderKind::OpenAiCompatible);
        assert_eq!(cfg.ai.base_url, "");
        assert_eq!(cfg.ai.model, "");
    }

    #[test]
    fn minimal_manifest_uses_documented_defaults() {
        let cfg = ProjectConfig::from_toml_str("schema = 1\n").unwrap();
        assert_eq!(cfg.build.backend, BuildBackendKind::Make);
        assert_eq!(cfg.build.cwd, PathBuf::from("."));
        assert!(cfg.build.targets.is_empty());
        assert!(cfg.build.artifacts.is_empty());
        assert_eq!(cfg.run.timeout_ms, DEFAULT_RUN_TIMEOUT_MS);
        assert_eq!(cfg.run.boot, DEFAULT_BOOT);
        assert_eq!(cfg.run.serial.device, "com1");
        assert_eq!(
            cfg.run.serial.tee_to_file,
            Some(PathBuf::from(DEFAULT_SERIAL_TEE))
        );
        assert_eq!(cfg.ai.provider, AiProviderKind::None);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        // top level
        let err = ProjectConfig::from_toml_str("schema = 1\nnope = 3\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        assert!(err.message.contains("unknown field"), "{err}");
        assert!(err.message.contains("nope"), "{err}");

        // inside a section (a plausible typo)
        let err = ProjectConfig::from_toml_str(
            "schema = 1\n[build]\nbackend = \"make\"\ntargts = [\"all\"]\n",
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        assert!(err.message.contains("targts"), "{err}");
        assert!(err.detail.is_some());

        // unknown key nested in an inline table
        let err = ProjectConfig::from_toml_str(
            "schema = 1\n[run]\nserial = { device = \"com1\", baud = 115200 }\n",
        )
        .unwrap_err();
        assert!(err.message.contains("baud"), "{err}");
    }

    #[test]
    fn missing_or_wrong_schema_is_rejected() {
        let err = ProjectConfig::from_toml_str("[project]\nname = \"x\"\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        assert!(err.message.contains("schema"), "{err}");

        let err = ProjectConfig::from_toml_str("schema = \"1\"\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);

        let err = ProjectConfig::from_toml_str("schema = 2\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        assert!(err.message.contains("schema 2"), "{err}");

        let err = ProjectConfig::from_toml_str("schema = 1\n[project]\narch = \"sparc\"\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);

        let err = ProjectConfig::from_toml_str("schema = 1\nthis is not toml\n").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
    }

    #[test]
    fn relative_paths_resolve_against_the_project_root() {
        let cfg = ProjectConfig::from_toml_str(CONTRACT_EXAMPLE).unwrap();
        let resolved = cfg.resolve("/work/mykernel");
        assert_eq!(resolved.root, PathBuf::from("/work/mykernel"));
        assert_eq!(resolved.build_cwd, PathBuf::from("/work/mykernel"));
        assert_eq!(
            resolved.declared_artifacts,
            vec![PathBuf::from("/work/mykernel/build/kernel.elf")]
        );
        assert_eq!(
            resolved.compile_commands,
            Some(PathBuf::from("/work/mykernel/compile_commands.json"))
        );
        assert_eq!(
            resolved.kernel,
            Some(PathBuf::from("/work/mykernel/build/kernel.elf"))
        );
        assert_eq!(
            resolved.serial_tee_to_file,
            Some(PathBuf::from("/work/mykernel/build/serial.log"))
        );
        assert_eq!(resolved.name(), "mykernel");
    }

    #[test]
    fn relative_paths_are_normalised_and_subdirectories_are_honoured() {
        let cfg = ProjectConfig::from_toml_str(
            "schema = 1\n[build]\ncwd = \"./sub/../out\"\nartifacts = [\"../upstream/k.elf\"]\n",
        )
        .unwrap();
        let resolved = cfg.resolve("/p");
        assert_eq!(resolved.build_cwd, PathBuf::from("/p/out"));
        assert_eq!(resolved.declared_artifacts, vec![PathBuf::from("/upstream/k.elf")]);

        let cfg = ProjectConfig::from_toml_str("schema = 1\n[build]\ncwd = \"/abs\"\n").unwrap();
        assert_eq!(cfg.resolve("/p").build_cwd, PathBuf::from("/abs"));
    }

    #[test]
    fn env_and_tilde_expansion() {
        std::env::set_var("PRINCESSIDE_TEST_ROOT", "/tmp/envroot");
        assert_eq!(expand_env("$PRINCESSIDE_TEST_ROOT/k"), "/tmp/envroot/k");
        assert_eq!(expand_env("${PRINCESSIDE_TEST_ROOT}/k"), "/tmp/envroot/k");
        assert_eq!(expand_env("$PRINCESSIDE_UNSET_XYZ/k"), "/k");
        assert_eq!(expand_env("a$b"), "a"); // `$b` is an unset variable
        assert_eq!(expand_env("100$"), "100$"); // trailing lone `$`
        assert_eq!(expand_env("${UNTERMINATED"), "${UNTERMINATED");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_env("~/k"), format!("{home}/k"));
        assert_eq!(expand_env("~"), home);

        let cfg = ProjectConfig::from_toml_str(
            "schema = 1\n[run]\nkernel = \"$PRINCESSIDE_TEST_ROOT/k.elf\"\n",
        )
        .unwrap();
        let resolved = cfg.resolve("/p");
        assert_eq!(resolved.kernel, Some(PathBuf::from("/tmp/envroot/k.elf")));
    }

    #[test]
    fn a_directory_without_a_manifest_gets_documented_defaults() {
        let dir = std::env::temp_dir().join(format!("princesside-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // No Makefile at all -> refuse to guess.
        let err = ProjectConfig::load_or_default(&dir).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
        assert!(err.message.contains("no princess.toml"), "{err}");

        std::fs::write(dir.join("Makefile"), "all:\n\t@true\niso:\n\t@true\n").unwrap();
        let loaded = ProjectConfig::load_or_default(&dir).unwrap();
        assert!(matches!(loaded.source, ConfigSource::Defaults { .. }));
        assert_eq!(loaded.config.build.backend, BuildBackendKind::Make);
        assert_eq!(loaded.config.build.targets, vec!["iso".to_string()]);
        assert!(loaded.config.build.artifacts.is_empty());
        assert_eq!(loaded.config.run.timeout_ms, DEFAULT_RUN_TIMEOUT_MS);
        let shown = loaded.source.to_string();
        assert!(shown.contains("engine defaults"), "{shown}");

        // A manifest wins over the fallback.
        std::fs::write(dir.join(CONFIG_FILE_NAME), "schema = 1\n[build]\ntargets = [\"all\"]\n").unwrap();
        let loaded = ProjectConfig::load_or_default(&dir).unwrap();
        assert!(matches!(loaded.source, ConfigSource::Manifest(_)));
        assert_eq!(loaded.config.build.targets, vec!["all".to_string()]);

        // ... and a broken manifest is still an error, not a fallback.
        std::fs::write(dir.join(CONFIG_FILE_NAME), "schema = 1\nbogus = 1\n").unwrap();
        let err = ProjectConfig::load_or_default(&dir).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_directory_is_not_found() {
        let err = ProjectConfig::load_or_default("/definitely/not/here/at/all").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
    }
}
