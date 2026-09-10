//! The REPL escape hatch: the only way to reach the GDB commands the built-in
//! DAP has no request for (`hbreak`, `monitor xp`, `info registers`, ...).
//!
//! # The trap this module exists to close
//!
//! GDB's built-in DAP implements `evaluate(context = "repl")` by feeding the
//! expression straight to the GDB command interpreter.  When that command
//! fails, GDB still answers **`success: true`** and puts the error in the
//! message text.  Measured against gdb 16.3 in this workspace:
//!
//! ```jsonc
//! // request:  evaluate { expression: "hbreak no_such_symbol_xyz", context: "repl" }
//! { "success": true,
//!   "body": { "result": "Function \"no_such_symbol_xyz\" not defined.\n
//!                        Hardware assisted breakpoint 1 (no_such_symbol_xyz) pending.\n",
//!             "variablesReference": 0 } }
//! ```
//!
//! A client that trusts `success` would report `E_OK` and a *pending* hardware
//! breakpoint for a symbol that does not exist — the exact "看似合理的假数据"
//! that P4-4 forbids.  So this layer:
//!
//! 1. still requires `success: true` (a `false` is a hard failure), **and**
//! 2. scans the returned text for GDB's error idioms, treating them as failures,
//!    **and**
//! 3. returns a [`ReplOutcome`] that makes the evidence explicit so callers can
//!    decide what is suspicious.
//!
//! # Why a denylist is the right shape here
//!
//! GDB's CLI has no machine-readable "did that work" channel — that is precisely
//! why MI exists, and D11 rules MI out.  The alternative to a text denylist is
//! to infer success from the *positive* output of a follow-up query
//! (`info breakpoints`), which the capability layer does for breakpoints
//! specifically ([`crate::capability`]).  This module handles the general case
//! and is deliberately conservative: an unrecognised error is *kept* in the
//! text and surfaced, never silently dropped.

use princess_core::{ErrorCode, PrincessError, Result};
use serde_json::{json, Value};

/// Text fragments that mean "GDB refused the command".
///
/// Every entry was observed from gdb 16.3 or is the canonical GDB diagnostic
/// for that condition.  Matching is case-sensitive on purpose: these are GDB's
/// own strings.
const ERROR_SIGNS: &[&str] = &[
    // Breakpoints on symbols that do not exist.
    "not defined.",
    "No symbol table is loaded",
    "No symbol \"",
    // General command failures.
    "Undefined command:",
    "Undefined item:",
    "Invalid syntax",
    "is not a valid",
    "Cannot access memory at address",
    "Cannot insert breakpoint",
    "Warning: ",
    "error: ",
    "Error: ",
    "No registers.",
    "The program has no registers now.",
    "You can't do that",
    "Requires an argument",
    "not supported by this target",
    // Symbol/architecture mismatches (D10).
    "is not the same architecture",
    "Architecture rejected",
];

/// The result of one REPL command.
///
/// The fields are deliberately *not* collapsed into a `Result<()>`: the
/// capability layer needs to inspect the text (to parse breakpoint numbers, or
/// to double-check a `pending` verdict) even on the success path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplOutcome {
    /// The GDB CLI command that was run, verbatim.
    pub command: String,
    /// Everything GDB printed, verbatim (trailing newline included).
    pub text: String,
    /// A GDB error idiom was found in `text`.
    pub saw_error_text: bool,
    /// The specific fragment that triggered [`Self::saw_error_text`].
    pub error_sign: Option<String>,
}

impl ReplOutcome {
    /// The output with trailing whitespace removed — what a caller usually
    /// wants to parse.
    pub fn trimmed(&self) -> &str {
        self.text.trim_end()
    }

    /// True when GDB reported success textually (no error idiom present).
    pub fn looks_ok(&self) -> bool {
        !self.saw_error_text
    }

    /// Turn a textual error into a `PrincessError`, keeping both the command and
    /// the raw output as evidence.
    pub fn into_error(self, context: impl AsRef<str>) -> PrincessError {
        let context = context.as_ref();
        let sign = self.error_sign.clone().unwrap_or_default();
        PrincessError::new(
            ErrorCode::NotFound,
            format!("{context}: GDB rejected `{}` ({sign})", self.command),
        )
        .with_detail(format!("command: {}\noutput:\n{}", self.command, self.text))
    }

    /// Fail unless the command produced no error text.
    pub fn require_ok(self, context: impl AsRef<str>) -> Result<Self> {
        if self.saw_error_text {
            return Err(self.into_error(context.as_ref()));
        }
        Ok(self)
    }
}

/// Interpret the body of an `evaluate(context="repl")` response.
///
/// `command` is the CLI string we sent; it is echoed into errors so the report
/// never has to guess which line failed.
pub fn interpret_response(command: &str, response: &Value) -> Result<ReplOutcome> {
    // Step 1 — an explicit `success: false` is always fatal.  `evaluate` in the
    // repl context does this for genuinely unknown *commands*
    // ("evaluate": the whole expression is not a command), which is a different
    // failure mode from a command that ran and reported its own error.
    let success = response.get("success").and_then(Value::as_bool).unwrap_or(false);
    if !success {
        let message = response
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("(no message)");
        return Err(PrincessError::new(
            ErrorCode::NotFound,
            format!("GDB rejected the repl command `{command}`: {message}"),
        )
        .with_detail(serde_json::to_string_pretty(response).unwrap_or_else(|_| response.to_string())));
    }

    // Step 2 — the text itself may carry an error even though `success` is true.
    // This is the documented gdb behaviour this module exists to defeat.
    let text = response
        .get("body")
        .and_then(|b| b.get("result"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let error_sign = ERROR_SIGNS
        .iter()
        .find(|sign| text.contains(**sign))
        .map(|sign| (*sign).to_string());

    Ok(ReplOutcome {
        command: command.to_string(),
        text,
        saw_error_text: error_sign.is_some(),
        error_sign,
    })
}

/// Run one GDB CLI command through the `evaluate` repl context.
///
/// This is the single choke point for the escape hatch; every capability in
/// [`crate::capability`] goes through it, so the `success`-is-a-lie behaviour is
/// handled exactly once.
pub fn run(transport: &mut crate::transport::Transport, command: &str) -> Result<ReplOutcome> {
    let response = transport.request(
        "evaluate",
        Some(json!({ "expression": command, "context": "repl" })),
    )?;
    interpret_response(command, &response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repl_response(text: &str) -> Value {
        json!({
            "type": "response",
            "command": "evaluate",
            "success": true,
            "body": { "result": text, "variablesReference": 0 }
        })
    }

    /// The measured P4-4 trap: gdb says success while the command failed.
    #[test]
    fn success_true_with_an_error_in_the_text_is_treated_as_a_failure() {
        let response = repl_response(
            "Function \"no_such_symbol_xyz\" not defined.\n\
             Hardware assisted breakpoint 1 (no_such_symbol_xyz) pending.\n",
        );
        let outcome = interpret_response("hbreak no_such_symbol_xyz", &response).unwrap();
        assert!(
            outcome.saw_error_text,
            "the `not defined.` text must be recognised as a failure"
        );
        assert_eq!(outcome.error_sign.as_deref(), Some("not defined."));
        assert!(!outcome.looks_ok());

        let err = outcome.require_ok("hardware breakpoint").unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(err.message.contains("no_such_symbol_xyz"), "{err}");
        let detail = err.detail.as_deref().unwrap();
        assert!(detail.contains("not defined."), "{detail}");
    }

    #[test]
    fn plain_success_text_is_not_flagged() {
        let response = repl_response("Hardware assisted breakpoint 1 at 0x1011c9: file kernel.c, line 120.\n");
        let outcome = interpret_response("hbreak paging_fault_probe", &response).unwrap();
        assert!(outcome.looks_ok(), "{outcome:?}");
        assert_eq!(
            outcome.trimmed(),
            "Hardware assisted breakpoint 1 at 0x1011c9: file kernel.c, line 120."
        );
        assert!(outcome.require_ok("hardware breakpoint").is_ok());
    }

    #[test]
    fn success_false_is_a_hard_failure_even_without_error_text() {
        let response = json!({
            "success": false,
            "command": "evaluate",
            "message": "Undefined command: \"$cr3\".  Try \"help\"."
        });
        let err = interpret_response("$cr3", &response).unwrap_err();
        assert_eq!(err.code, ErrorCode::NotFound);
        assert!(err.message.contains("$cr3"), "{err}");
    }

    #[test]
    fn a_missing_success_flag_is_not_assumed_to_be_success() {
        let err = interpret_response("info registers", &json!({"body": {}})).unwrap_err();
        assert!(err.message.contains("info registers"));
    }

    /// `notStopped` arrives as `success: false` from gdb when the target is
    /// running; it must surface as an error, never as an empty register file.
    #[test]
    fn running_target_errors_are_not_silently_emptied() {
        let err = interpret_response("info registers", &json!({"success": false, "message": "notStopped"}))
            .unwrap_err();
        assert!(err.message.contains("notStopped"), "{err}");
    }

    #[test]
    fn undefined_command_text_is_caught() {
        let response = repl_response("Undefined command: \"monitor\".  Try \"help\".\n");
        let outcome = interpret_response("monitor info cpus", &response).unwrap();
        assert!(outcome.saw_error_text);
        assert_eq!(outcome.error_sign.as_deref(), Some("Undefined command:"));
    }

    #[test]
    fn architecture_mismatch_wording_is_recognised() {
        let response = repl_response("The program has no registers now.\n");
        assert!(interpret_response("info registers", &response).unwrap().saw_error_text);
        let response = repl_response("\"/tmp/k.elf\": not in executable format: file format not recognized\n");
        // Not in the denylist by design; it must still be *visible* to callers
        // rather than dropped, which is what `text` guarantees.
        let outcome = interpret_response("symbol-file /tmp/k.elf", &response).unwrap();
        assert!(outcome.text.contains("not in executable format"));
    }

    #[test]
    fn empty_output_is_a_clean_success() {
        let outcome = interpret_response("set pagination off", &repl_response("")).unwrap();
        assert!(outcome.looks_ok());
        assert_eq!(outcome.trimmed(), "");
    }

    #[test]
    fn error_keeps_the_command_and_the_full_output_as_evidence() {
        let response = repl_response("Function \"nope\" not defined.\n");
        let outcome = interpret_response("hbreak nope", &response).unwrap();
        let err = outcome.into_error("hardware breakpoint");
        let detail = err.detail.unwrap();
        assert!(detail.contains("command: hbreak nope"), "{detail}");
        assert!(detail.contains("Function \"nope\" not defined."), "{detail}");
    }
}
