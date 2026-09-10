//! Error model — `docs/spec/10-contracts.md` §3.
//!
//! The error-code set is **closed**: it is frozen by the interface contract and
//! adding a variant requires amending that document.  Every variant maps to a
//! stable string (`E_TOOLCHAIN_MISSING`, ...) which is what crosses the
//! IPC boundary and what the front end switches on; the string form must never
//! change once shipped.
//!
//! Every fallible engine entry point returns [`Result`], and every failure
//! carries the original tool output in [`PrincessError::detail`] — the contract
//! forbids swallowing failures or silently degrading (principle 4 in §0).

use std::fmt;

use serde::{Deserialize, Serialize};

/// The closed set of engine error codes (contract §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ErrorCode {
    /// A required external tool was not found or is not executable.
    #[serde(rename = "E_TOOLCHAIN_MISSING")]
    ToolchainMissing,
    /// The build backend ran and reported failure.
    #[serde(rename = "E_BUILD_FAILED")]
    BuildFailed,
    /// QEMU (or another emulator) failed to start or died unexpectedly.
    #[serde(rename = "E_QEMU_FAILED")]
    QemuFailed,
    /// An operation exceeded its deadline and was terminated.
    #[serde(rename = "E_TIMEOUT")]
    Timeout,
    /// A requested file, symbol, project or operation id does not exist.
    #[serde(rename = "E_NOT_FOUND")]
    NotFound,
    /// `princess.toml` is missing, malformed, versionless or carries unknown keys.
    #[serde(rename = "E_INVALID_CONFIG")]
    InvalidConfig,
    /// A path/syscall was refused by the sandbox.
    #[serde(rename = "E_SANDBOX_DENIED")]
    SandboxDenied,
    /// The user (or a parent operation) cancelled the operation.
    #[serde(rename = "E_CANCELLED")]
    Cancelled,
    /// No AI provider is configured/reachable; the rest of the IDE is unaffected.
    #[serde(rename = "E_AI_UNAVAILABLE")]
    AiUnavailable,
    /// Anything that indicates a bug in the engine rather than a user mistake.
    #[serde(rename = "E_INTERNAL")]
    Internal,
}

impl ErrorCode {
    /// The stable wire string for this code.  Frozen by the contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::ToolchainMissing => "E_TOOLCHAIN_MISSING",
            ErrorCode::BuildFailed => "E_BUILD_FAILED",
            ErrorCode::QemuFailed => "E_QEMU_FAILED",
            ErrorCode::Timeout => "E_TIMEOUT",
            ErrorCode::NotFound => "E_NOT_FOUND",
            ErrorCode::InvalidConfig => "E_INVALID_CONFIG",
            ErrorCode::SandboxDenied => "E_SANDBOX_DENIED",
            ErrorCode::Cancelled => "E_CANCELLED",
            ErrorCode::AiUnavailable => "E_AI_UNAVAILABLE",
            ErrorCode::Internal => "E_INTERNAL",
        }
    }

    /// Every code in the closed set, in contract order.  Used by tests to prove
    /// the set has not grown or shrunk by accident.
    pub const ALL: [ErrorCode; 10] = [
        ErrorCode::ToolchainMissing,
        ErrorCode::BuildFailed,
        ErrorCode::QemuFailed,
        ErrorCode::Timeout,
        ErrorCode::NotFound,
        ErrorCode::InvalidConfig,
        ErrorCode::SandboxDenied,
        ErrorCode::Cancelled,
        ErrorCode::AiUnavailable,
        ErrorCode::Internal,
    ];
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single engine failure: code + human message + raw tool output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincessError {
    /// Stable machine-readable code.
    pub code: ErrorCode,
    /// One-line human summary (no trailing newline).
    pub message: String,
    /// Original tool output / offending value, verbatim.  Never paraphrased.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl PrincessError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            detail: None,
        }
    }

    /// Attach raw evidence (stderr, file excerpt, ...).  Replaces any previous
    /// detail: the newest evidence is the most specific.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if !detail.is_empty() {
            self.detail = Some(detail);
        }
        self
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn invalid_config(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidConfig, message)
    }

    pub fn cancelled(op_id: &str) -> Self {
        Self::new(ErrorCode::Cancelled, format!("operation {op_id} was cancelled"))
    }
}

impl fmt::Display for PrincessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(detail) = &self.detail {
            write!(f, " ({detail})")?;
        }
        Ok(())
    }
}

impl std::error::Error for PrincessError {}

/// The engine's uniform result alias (contract §3: failures are explicit).
pub type Result<T> = std::result::Result<T, PrincessError>;

impl From<serde_json::Error> for PrincessError {
    fn from(err: serde_json::Error) -> Self {
        PrincessError::internal(format!("json error: {err}"))
    }
}

impl From<std::io::Error> for PrincessError {
    fn from(err: std::io::Error) -> Self {
        let code = match err.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::SandboxDenied,
            _ => ErrorCode::Internal,
        };
        PrincessError::new(code, format!("io error: {err}"))
    }
}

/// The uniform IPC reply shape (contract §3).
///
/// ```jsonc
/// { "ok": true,  "data": { } }
/// { "ok": false, "error": { "code": "E_BUILD_FAILED", "message": "…", "detail": "…" } }
/// ```
///
/// Serialised as an internally tagged enum so the JSON is exactly the shape the
/// front end expects, with `data` absent on failures and `error` absent on
/// successes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IpcResponse<T> {
    Ok { ok: bool, data: T },
    Err { ok: bool, error: PrincessError },
}

impl<T> IpcResponse<T> {
    /// Build a successful reply.  `ok` is always `true`.
    pub fn ok(data: T) -> Self {
        IpcResponse::Ok { ok: true, data }
    }

    /// Build a failed reply.  `ok` is always `false`.
    pub fn err(error: PrincessError) -> Self {
        IpcResponse::Err { ok: false, error }
    }

    pub fn from_result(result: Result<T>) -> Self {
        match result {
            Ok(data) => IpcResponse::ok(data),
            Err(error) => IpcResponse::err(error),
        }
    }

    pub fn is_ok(&self) -> bool {
        match self {
            IpcResponse::Ok { ok, .. } => *ok,
            IpcResponse::Err { ok, .. } => *ok,
        }
    }

    pub fn error(&self) -> Option<&PrincessError> {
        match self {
            IpcResponse::Ok { .. } => None,
            IpcResponse::Err { error, .. } => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P2-8 adjacent: the wire strings are the contract.  If this test has to
    /// change, the interface contract changed and the front end must too.
    #[test]
    fn error_code_strings_are_frozen() {
        let expected = [
            (ErrorCode::ToolchainMissing, "E_TOOLCHAIN_MISSING"),
            (ErrorCode::BuildFailed, "E_BUILD_FAILED"),
            (ErrorCode::QemuFailed, "E_QEMU_FAILED"),
            (ErrorCode::Timeout, "E_TIMEOUT"),
            (ErrorCode::NotFound, "E_NOT_FOUND"),
            (ErrorCode::InvalidConfig, "E_INVALID_CONFIG"),
            (ErrorCode::SandboxDenied, "E_SANDBOX_DENIED"),
            (ErrorCode::Cancelled, "E_CANCELLED"),
            (ErrorCode::AiUnavailable, "E_AI_UNAVAILABLE"),
            (ErrorCode::Internal, "E_INTERNAL"),
        ];
        for (code, text) in expected {
            assert_eq!(code.as_str(), text);
            assert_eq!(code.to_string(), text);
            // serde must emit the bare string, not an externally tagged object.
            assert_eq!(serde_json::to_string(&code).unwrap(), format!("\"{text}\""));
            let back: ErrorCode = serde_json::from_str(&format!("\"{text}\"")).unwrap();
            assert_eq!(back, code);
        }
        assert_eq!(ErrorCode::ALL.len(), expected.len());
        // Uniqueness: two codes must never share a string.
        let mut strings: Vec<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        strings.sort_unstable();
        let before = strings.len();
        strings.dedup();
        assert_eq!(before, strings.len(), "duplicate error code strings");
    }

    #[test]
    fn ipc_response_shapes_match_contract() {
        let ok: IpcResponse<u32> = IpcResponse::ok(7);
        assert_eq!(serde_json::to_string(&ok).unwrap(), r#"{"ok":true,"data":7}"#);

        let err: IpcResponse<u32> = IpcResponse::err(
            PrincessError::new(ErrorCode::BuildFailed, "make exited 2").with_detail("kernel.c:12: error"),
        );
        assert_eq!(
            serde_json::to_string(&err).unwrap(),
            r#"{"ok":false,"error":{"code":"E_BUILD_FAILED","message":"make exited 2","detail":"kernel.c:12: error"}}"#
        );

        let parsed: IpcResponse<u32> = serde_json::from_str(r#"{"ok":false,"error":{"code":"E_TIMEOUT","message":"t"}}"#).unwrap();
        assert_eq!(parsed.error().unwrap().code, ErrorCode::Timeout);
        assert!(parsed.error().unwrap().detail.is_none());
    }

    #[test]
    fn detail_is_omitted_when_absent_and_never_empty() {
        let err = PrincessError::not_found("nope");
        assert_eq!(serde_json::to_string(&err).unwrap(), r#"{"code":"E_NOT_FOUND","message":"nope"}"#);
        let err = PrincessError::not_found("nope").with_detail("");
        assert!(err.detail.is_none(), "empty detail must not be recorded as evidence");
    }

    #[test]
    fn io_errors_map_to_stable_codes() {
        let missing = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        assert_eq!(PrincessError::from(missing).code, ErrorCode::NotFound);
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "no");
        assert_eq!(PrincessError::from(denied).code, ErrorCode::SandboxDenied);
    }
}
