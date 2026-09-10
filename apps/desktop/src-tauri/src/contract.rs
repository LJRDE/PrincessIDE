//! Frozen IPC contract — Rust mirror of `docs/spec/10-contracts.md` §3.
//!
//! `CONTRACT_COMMANDS` and `ERROR_CODES` are copied verbatim from the spec.
//! `apps/desktop/scripts/check-contract.mjs` extracts them from this file and
//! diffs them against the spec and against the TypeScript mirror, so drift in
//! either direction fails the build's contract test.

use serde::Serialize;
use serde_json::{json, Value};

/// Every command name of the v1 minimal set (§3), verbatim, in spec order.
pub const CONTRACT_COMMANDS: &[&str] = &[
    "princess:project:open",
    "princess:project:validate",
    "princess:tools:detect",
    "princess:build:start",
    "princess:build:cancel",
    "princess:run:start",
    "princess:run:stop",
    "princess:debug:attach",
    "princess:debug:setBreakpoints",
    "princess:debug:continue",
    "princess:debug:stepOver",
    "princess:debug:stepInto",
    "princess:debug:stackTrace",
    "princess:debug:scopes",
    "princess:debug:variables",
    "princess:debug:readMemory",
    "princess:debug:writeMemory",
    "princess:debug:disassemble",
    "princess:debug:registers",
    "princess:op:cancel",
    "princess:op:replay",
    // Added by the D17 amendment (engine owns the clangd process + framing).
    "princess:lsp:start",
    "princess:lsp:send",
    "princess:lsp:stop",
];

/// Commands this phase actually implements end to end.
///
/// The three `lsp` commands are deliberately **not** here: the D17 amendment
/// assigns the clangd process and the frame headers to the engine (P2), so in
/// P3 they must answer `E_NOT_FOUND` + "not implemented yet", never pretend to work.
pub const IMPLEMENTED_COMMANDS: &[&str] = &[
    "princess:tools:detect",
    "princess:op:cancel",
    "princess:op:replay",
];

/// Error-code closed set (§3).  Adding a code requires changing the contract.
pub const ERROR_CODES: &[&str] = &[
    "E_TOOLCHAIN_MISSING",
    "E_BUILD_FAILED",
    "E_QEMU_FAILED",
    "E_TIMEOUT",
    "E_NOT_FOUND",
    "E_INVALID_CONFIG",
    "E_SANDBOX_DENIED",
    "E_CANCELLED",
    "E_AI_UNAVAILABLE",
    "E_INTERNAL",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ToolchainMissing,
    BuildFailed,
    QemuFailed,
    Timeout,
    NotFound,
    InvalidConfig,
    SandboxDenied,
    Cancelled,
    AiUnavailable,
    Internal,
}

impl ErrorCode {
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
}

/// `{ "ok": true, "data": … }` (§3).
pub fn ok<T: Serialize>(data: T) -> Value {
    json!({ "ok": true, "data": data })
}

/// `{ "ok": false, "error": { "code", "message", "detail" } }` (§3).
///
/// `detail` carries the raw stderr / underlying cause verbatim: the contract
/// forbids swallowing failures and forbids paraphrasing the evidence (§0.4).
pub fn err(code: ErrorCode, message: impl Into<String>, detail: impl Into<String>) -> Value {
    json!({
        "ok": false,
        "error": {
            "code": code.as_str(),
            "message": message.into(),
            "detail": detail.into(),
        }
    })
}

/// A structured failure that can be converted into the wire envelope.
#[derive(Debug, Clone)]
pub struct IpcFailure {
    pub code: ErrorCode,
    pub message: String,
    pub detail: String,
}

impl IpcFailure {
    pub fn new(code: ErrorCode, message: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { code, message: message.into(), detail: detail.into() }
    }

    pub fn into_value(self) -> Value {
        err(self.code, self.message, self.detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_unique_and_prefixed() {
        let mut seen = std::collections::HashSet::new();
        for code in ERROR_CODES {
            assert!(code.starts_with("E_"), "{code} must start with E_");
            assert!(seen.insert(*code), "duplicate error code {code}");
        }
        assert_eq!(seen.len(), 10, "the contract lists exactly 10 error codes");
    }

    #[test]
    fn implemented_commands_are_part_of_the_contract() {
        for cmd in IMPLEMENTED_COMMANDS {
            assert!(CONTRACT_COMMANDS.contains(cmd), "{cmd} is not in §3");
        }
    }

    #[test]
    fn contract_commands_are_unique_and_namespaced() {
        let mut seen = std::collections::HashSet::new();
        for cmd in CONTRACT_COMMANDS {
            assert!(cmd.starts_with("princess:"), "{cmd} must be namespaced");
            assert_eq!(cmd.matches(':').count(), 2, "{cmd} must be princess:<domain>:<action>");
            assert!(seen.insert(*cmd), "duplicate command {cmd}");
        }
        assert_eq!(seen.len(), 24, "the v1 set is 21 commands + 3 lsp commands (D17)");
    }

    #[test]
    fn envelope_shapes_match_the_contract() {
        let good = ok(json!({ "a": 1 }));
        assert_eq!(good["ok"], json!(true));
        assert_eq!(good["data"]["a"], json!(1));

        let bad = err(ErrorCode::BuildFailed, "make exited 2", "make: *** [Makefile:9] Error 1");
        assert_eq!(bad["ok"], json!(false));
        assert_eq!(bad["error"]["code"], json!("E_BUILD_FAILED"));
        assert_eq!(bad["error"]["detail"], json!("make: *** [Makefile:9] Error 1"));
    }
}
