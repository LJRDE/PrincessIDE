//! Shared value types used by the event model and the backend traits.
//!
//! Everything here is `serde`-serialisable with the exact key spelling the
//! contract uses (`10-contracts.md` §2/§3), because the web front end consumes
//! these structures directly with no translation layer (§0 principle 5).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Where a chunk of streamed text came from (contract §2, `log.append.stream`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LogStream {
    /// Build backend output (compiler / linker / make).
    #[serde(rename = "build")]
    Build,
    /// Guest COM1 serial output.
    #[serde(rename = "serial.com1")]
    SerialCom1,
    /// QEMU monitor channel.
    #[serde(rename = "qemu.monitor")]
    QemuMonitor,
    /// GDB / MI console.
    #[serde(rename = "gdb.console")]
    GdbConsole,
    /// The engine itself talking to the user.
    #[serde(rename = "ide")]
    Ide,
}

impl LogStream {
    pub const fn as_str(self) -> &'static str {
        match self {
            LogStream::Build => "build",
            LogStream::SerialCom1 => "serial.com1",
            LogStream::QemuMonitor => "qemu.monitor",
            LogStream::GdbConsole => "gdb.console",
            LogStream::Ide => "ide",
        }
    }
}

/// Encoding fidelity marker for a streamed chunk (contract §2).
///
/// The engine guarantees it never splits a UTF-8 code point across chunks; if
/// the producer emitted bytes that are not valid UTF-8 they are replaced with
/// U+FFFD and the chunk is marked [`TextEncoding::Utf8Lossy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TextEncoding {
    #[serde(rename = "utf8")]
    Utf8,
    #[serde(rename = "utf8-lossy")]
    Utf8Lossy,
}

impl TextEncoding {
    pub const fn as_str(self) -> &'static str {
        match self {
            TextEncoding::Utf8 => "utf8",
            TextEncoding::Utf8Lossy => "utf8-lossy",
        }
    }
}

/// Severity of a `build.diagnostic` (contract §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DiagnosticSeverity {
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "warning")]
    Warning,
    #[serde(rename = "note")]
    Note,
}

/// Which producer emitted a `build.diagnostic` (contract §2, `source`).
///
/// The contract lists `clang|ld|nasm`; `gcc` is an **addition** required by the
/// reference fixture, whose `Makefile` builds with GNU `gcc`/`ld`.  Labelling
/// GCC output as `clang` would be a lie in the event stream, so the variant was
/// added instead (flagged for contract amendment in the P2 report).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DiagnosticSource {
    #[serde(rename = "clang")]
    Clang,
    #[serde(rename = "gcc")]
    Gcc,
    #[serde(rename = "ld")]
    Ld,
    #[serde(rename = "nasm")]
    Nasm,
}

impl DiagnosticSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            DiagnosticSource::Clang => "clang",
            DiagnosticSource::Gcc => "gcc",
            DiagnosticSource::Ld => "ld",
            DiagnosticSource::Nasm => "nasm",
        }
    }
}

/// Terminal status of a build (contract §2, `build.finished.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum BuildStatus {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "cancelled")]
    Cancelled,
}

impl BuildStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            BuildStatus::Ok => "ok",
            BuildStatus::Failed => "failed",
            BuildStatus::Cancelled => "cancelled",
        }
    }
}

/// Why an emulator run ended (contract §2, `run.exited.reason`).
///
/// Attributed by the engine only — the UI must never guess (§6 rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ExitReason {
    /// The guest asked the machine to power off and the emulator exited cleanly.
    #[serde(rename = "guest-shutdown")]
    GuestShutdown,
    /// The CPU could not deliver an exception: reset with no recovery.
    #[serde(rename = "triple-fault")]
    TripleFault,
    /// The run exceeded `run.timeout_ms` and was terminated by the engine.
    #[serde(rename = "timeout")]
    Timeout,
    /// The run was cancelled (user, `run:stop`, or `op:cancel`).
    #[serde(rename = "killed")]
    Killed,
}

impl ExitReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            ExitReason::GuestShutdown => "guest-shutdown",
            ExitReason::TripleFault => "triple-fault",
            ExitReason::Timeout => "timeout",
            ExitReason::Killed => "killed",
        }
    }
}

/// What kind of file an entry in `build.finished.artifacts[]` is.
///
/// Also used by `artifact.changed` so the hex/ELF/disassembly views can decide
/// whether a refresh concerns them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// A linked ELF kernel / executable image.
    Elf,
    /// A bootable ISO image (GRUB path for x86_64 multiboot2).
    Iso,
    /// A raw disk/floppy image.
    Image,
    /// A relocatable object file.
    Object,
    /// A static library.
    Archive,
    /// Anything else the build declared.
    Other,
}

impl ArtifactKind {
    /// Best-effort classification from the file name extension.
    pub fn from_path(path: &std::path::Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("elf") => ArtifactKind::Elf,
            Some("iso") => ArtifactKind::Iso,
            Some("img") | Some("bin") => ArtifactKind::Image,
            Some("o") | Some("obj") => ArtifactKind::Object,
            Some("a") => ArtifactKind::Archive,
            _ => ArtifactKind::Other,
        }
    }
}

/// One build product (contract §2, `build.finished.artifacts[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// Absolute path.
    pub path: String,
    pub kind: ArtifactKind,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the file contents.
    pub sha256: String,
}

/// A source location: used by diagnostics, symbolication and stack frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocation {
    /// Absolute path where the engine could resolve it.
    pub file: String,
    pub line: u32,
    /// 1-based column when the producer reports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// Symbolication result attached to `run.fault.symbolicated` (contract §2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolicatedLocation {
    /// Demangled function name.
    pub symbol: String,
    /// Absolute source path.
    pub file: String,
    pub line: u32,
}

/// Program counter + register snapshot.
///
/// Registers are keyed by their canonical upper-case name (`RIP`, `CR3`, ...)
/// and carry the producer's own hex text so nothing is truncated on the way to
/// the UI.  The engine never invents a register value it did not read.
pub type RegisterFile = BTreeMap<String, String>;

/// A stub endpoint the debugger connects to (contract §4 `[debug] stub`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GdbStub {
    pub host: String,
    pub port: u16,
    /// `launch` | `attach`.
    pub mode: StubMode,
}

impl GdbStub {
    /// `host:port`, the form `run.started.gdbStub` carries.
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum StubMode {
    #[default]
    #[serde(rename = "launch")]
    Launch,
    #[serde(rename = "attach")]
    Attach,
}

/// Which character device carries guest serial output (contract §4 `[run] serial`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialCapture {
    /// `com1` in v1.
    pub device: String,
    /// When set, the engine writes every byte here *before* emitting the event
    /// (contract §2: "serial.com1 必须先落盘再推送").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tee_to_file: Option<String>,
}

impl Default for SerialCapture {
    fn default() -> Self {
        Self {
            device: "com1".to_string(),
            tee_to_file: None,
        }
    }
}

/// A tool resolved by `princess:tools:detect` (contract §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    /// Logical name (`qemu-system-x86_64`, `nasm`, ...).
    pub name: String,
    /// Absolute path, or `null` when not found.
    pub path: Option<String>,
    /// First line of the tool's `--version` output, or `null`.
    pub version: Option<String>,
    /// Whether the engine may rely on it for v1 features.
    pub available: bool,
    /// Whether a missing tool is an error for this project.
    pub required: bool,
}

/// Result of `princess:tools:detect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainReport {
    pub tools: Vec<ToolInfo>,
    /// `true` when every required tool resolved.
    pub ok: bool,
}

impl ToolchainReport {
    pub fn find(&self, name: &str) -> Option<&ToolInfo> {
        self.tools.iter().find(|t| t.name == name)
    }

    pub fn missing_required(&self) -> Vec<&ToolInfo> {
        self.tools
            .iter()
            .filter(|t| t.required && !t.available)
            .collect()
    }
}

/// One frame of a `stackTrace` reply (contract §3 `princess:debug:stackTrace`).
#[allow(non_snake_case)] // wire-mirror field names, serde renames would hide them
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackFrame {
    pub id: u64,
    /// Function name as reported by the backend (never synthesised).
    pub name: String,
    /// Source location when debug info covers the frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
    /// Instructions pointer, hex text as reported by the backend.
    pub instructionPointer: String,
}

/// A named variable scope (`princess:debug:scopes`).
#[allow(non_snake_case)] // wire-mirror field names, serde renames would hide them
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub name: String,
    /// Backend handle, echoed back to `variables`.
    pub variablesReference: i64,
    #[serde(default)]
    pub expensive: bool,
}

/// A variable (`princess:debug:variables`).
#[allow(non_snake_case)] // wire-mirror field names, serde renames would hide them
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(default)]
    pub variablesReference: i64,
}

/// A breakpoint as the backend sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakpointSpec {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    pub verified: bool,
    /// Where the backend actually planted it (may differ from the request).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<SourceLocation>,
}

/// ELF section/segment entry (`princess:bin:*`, P5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionInfo {
    pub name: String,
    pub address: u64,
    pub size: u64,
    pub flags: String,
}

/// Symbol table entry (`princess:bin:*`, P5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolInfo {
    pub name: String,
    pub address: u64,
    pub size: u64,
    /// `FUNC` | `OBJECT` | `SECTION` | `FILE` | `NOTYPE` | ...
    pub kind: String,
}

/// One disassembled instruction (`princess:debug:disassemble`, P5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisassembledInstruction {
    pub address: String,
    /// Raw instruction bytes, hex without separators.
    pub bytes: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceLocation>,
}

/// Token accounting for one AI completion (contract §2, `ai.finished.usage`).
#[allow(non_snake_case)] // wire-mirror field names, serde renames would hide them
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiUsage {
    #[serde(default)]
    pub promptTokens: u64,
    #[serde(default)]
    pub completionTokens: u64,
    #[serde(default)]
    pub totalTokens: u64,
}

/// One message of an OpenAI-compatible chat request (contract §5, `AiProvider`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `system` | `user` | `assistant`.
    pub role: String,
    pub content: String,
}

/// An OpenAI-compatible `chat/completions` request.
#[allow(non_snake_case)] // wire-mirror field names, serde renames would hide them
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub requestId: String,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    /// Ask the provider to stream; the engine always does for v1.
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_stream_and_reason_strings_are_frozen() {
        let streams = [
            (LogStream::Build, "build"),
            (LogStream::SerialCom1, "serial.com1"),
            (LogStream::QemuMonitor, "qemu.monitor"),
            (LogStream::GdbConsole, "gdb.console"),
            (LogStream::Ide, "ide"),
        ];
        for (value, text) in streams {
            assert_eq!(serde_json::to_string(&value).unwrap(), format!("\"{text}\""));
            assert_eq!(value.as_str(), text);
        }

        let reasons = [
            (ExitReason::GuestShutdown, "guest-shutdown"),
            (ExitReason::TripleFault, "triple-fault"),
            (ExitReason::Timeout, "timeout"),
            (ExitReason::Killed, "killed"),
        ];
        for (value, text) in reasons {
            assert_eq!(serde_json::to_string(&value).unwrap(), format!("\"{text}\""));
            assert_eq!(value.as_str(), text);
        }

        let statuses = [
            (BuildStatus::Ok, "ok"),
            (BuildStatus::Failed, "failed"),
            (BuildStatus::Cancelled, "cancelled"),
        ];
        for (value, text) in statuses {
            assert_eq!(serde_json::to_string(&value).unwrap(), format!("\"{text}\""));
        }

        let severities = [
            (DiagnosticSeverity::Error, "error"),
            (DiagnosticSeverity::Warning, "warning"),
            (DiagnosticSeverity::Note, "note"),
        ];
        for (value, text) in severities {
            assert_eq!(serde_json::to_string(&value).unwrap(), format!("\"{text}\""));
        }

        let sources = [
            (DiagnosticSource::Clang, "clang"),
            (DiagnosticSource::Gcc, "gcc"),
            (DiagnosticSource::Ld, "ld"),
            (DiagnosticSource::Nasm, "nasm"),
        ];
        for (value, text) in sources {
            assert_eq!(serde_json::to_string(&value).unwrap(), format!("\"{text}\""));
        }

        assert_eq!(TextEncoding::Utf8.as_str(), "utf8");
        assert_eq!(
            serde_json::to_string(&TextEncoding::Utf8Lossy).unwrap(),
            "\"utf8-lossy\""
        );
    }

    #[test]
    fn artifact_kind_is_classified_from_extension() {
        use std::path::Path;
        assert_eq!(ArtifactKind::from_path(Path::new("build/refkernel.elf")), ArtifactKind::Elf);
        assert_eq!(ArtifactKind::from_path(Path::new("build/refkernel.iso")), ArtifactKind::Iso);
        assert_eq!(ArtifactKind::from_path(Path::new("a/b.img")), ArtifactKind::Image);
        assert_eq!(ArtifactKind::from_path(Path::new("a/kernel.o")), ArtifactKind::Object);
        assert_eq!(ArtifactKind::from_path(Path::new("lib.a")), ArtifactKind::Archive);
        assert_eq!(ArtifactKind::from_path(Path::new("README")), ArtifactKind::Other);
        assert_eq!(serde_json::to_string(&ArtifactKind::Elf).unwrap(), "\"elf\"");
    }

    #[test]
    fn toolchain_report_helpers() {
        let report = ToolchainReport {
            tools: vec![
                ToolInfo {
                    name: "nasm".into(),
                    path: Some("/usr/bin/nasm".into()),
                    version: Some("NASM version 2.16.01".into()),
                    available: true,
                    required: true,
                },
                ToolInfo {
                    name: "zig".into(),
                    path: None,
                    version: None,
                    available: false,
                    required: false,
                },
            ],
            ok: true,
        };
        assert!(report.find("nasm").unwrap().available);
        assert!(report.find("nope").is_none());
        assert!(report.missing_required().is_empty());
    }
}
