//! Failure classification for the AI layer.
//!
//! The contract's error-code set is closed (`docs/spec/10-contracts.md` §3), so
//! every way this layer can fail has to land on one of its codes.  The mapping
//! is written down here and pinned by tests instead of being scattered through
//! the transport code:
//!
//! | situation | code | why |
//! |---|---|---|
//! | no provider configured / transport failure / unparseable reply | `E_AI_UNAVAILABLE` | the AI feature is unavailable; **nothing else in the IDE may be affected** (P7-2) |
//! | HTTP 401 / 403 | `E_AI_UNAVAILABLE` | a rejected key cannot be fixed by retrying; the feature is simply off |
//! | HTTP 404 | `E_AI_UNAVAILABLE` | wrong `base_url`; also a configuration problem, not a transient one |
//! | HTTP 408 / 429 / 5xx | `E_TIMEOUT` / `E_AI_UNAVAILABLE` | the server answered but could not serve this request |
//! | request deadline exceeded, connect timeout, read timeout | `E_TIMEOUT` | one of the contract's dedicated codes |
//! | cancellation (token or `AiProvider::cancel`) | `E_CANCELLED` | the user asked for it |
//! | a malformed *manifest* `[ai]` field | `E_INVALID_CONFIG` | produced by `princess-core` before this layer runs |
//!
//! **Design rule**: `E_AI_UNAVAILABLE` is the widest bucket on purpose.  Its
//! documented meaning is "no AI provider is configured/reachable; the rest of
//! the IDE is unaffected", which is exactly the P7-2 promise — an AI failure
//! must never turn into a blocking engine error.

use princess_core::{ErrorCode, PrincessError, Result};

/// Classify a non-2xx HTTP status.
///
/// `body` is the raw response text (usually the provider's JSON error object)
/// and is attached verbatim as `detail`; it is never paraphrased.
pub fn for_http_status(status: u16, body: &str) -> PrincessError {
    let snippet = truncate_for_detail(body);
    let (code, message) = match status {
        // A bad key or a bad base_url is a configuration state, not a transient
        // failure: the AI feature is unavailable until someone fixes it.
        401 | 403 => (
            ErrorCode::AiUnavailable,
            format!("AI provider rejected the credentials (HTTP {status})"),
        ),
        404 => (
            ErrorCode::AiUnavailable,
            format!("AI provider endpoint not found (HTTP {status}); check `[ai].base_url`"),
        ),
        408 => (
            ErrorCode::Timeout,
            format!("AI provider timed out the request (HTTP {status})"),
        ),
        429 => (
            ErrorCode::AiUnavailable,
            format!("AI provider rate-limited the request (HTTP {status})"),
        ),
        s if (500..600).contains(&s) => (
            ErrorCode::AiUnavailable,
            format!("AI provider failed server-side (HTTP {s})"),
        ),
        s => (
            ErrorCode::AiUnavailable,
            format!("AI provider returned an unexpected status (HTTP {s})"),
        ),
    };
    let mut err = PrincessError::new(code, message);
    if !snippet.is_empty() {
        err = err.with_detail(snippet);
    }
    err
}

/// A transport-level failure: DNS, refused connection, TLS, a read that ended
/// early.  Every one of them means "no provider right now", and every one of
/// them is non-fatal to the rest of the engine.
pub fn for_transport(context: &str, err: &dyn std::fmt::Display) -> PrincessError {
    PrincessError::new(
        ErrorCode::AiUnavailable,
        format!("AI provider is unreachable ({context})"),
    )
    .with_detail(err.to_string())
}

/// The request deadline fired.
pub fn for_timeout(timeout_ms: u64) -> PrincessError {
    PrincessError::new(
        ErrorCode::Timeout,
        format!("AI request exceeded its {timeout_ms} ms deadline"),
    )
}

/// The caller cancelled.
pub fn for_cancelled(request_id: &str) -> PrincessError {
    PrincessError::new(
        ErrorCode::Cancelled,
        format!("AI request {request_id} was cancelled"),
    )
}

/// The provider answered, but the bytes are not a stream we understand.
pub fn for_malformed(context: &str, err: &dyn std::fmt::Display) -> PrincessError {
    PrincessError::new(
        ErrorCode::AiUnavailable,
        format!("AI provider sent a reply this engine cannot parse ({context})"),
    )
    .with_detail(err.to_string())
}

/// Failures that come out of a blocking socket read.
pub fn for_io(context: &str, err: &std::io::Error) -> PrincessError {
    use std::io::ErrorKind;
    match err.kind() {
        // A read deadline is expressed as `WouldBlock`/`TimedOut` by our own
        // transport layer (the socket is in non-blocking mode with an explicit
        // deadline), so it is a timeout rather than an availability problem.
        ErrorKind::TimedOut | ErrorKind::WouldBlock => {
            PrincessError::new(ErrorCode::Timeout, format!("AI provider {context} timed out"))
                .with_detail(err.to_string())
        }
        _ => for_transport(context, err),
    }
}

impl From<serde_json::Error> for AiJsonError {
    fn from(err: serde_json::Error) -> Self {
        AiJsonError(err)
    }
}

/// Newtype so a `serde_json::Error` inside this crate maps to
/// `E_AI_UNAVAILABLE` instead of `princess-core`'s blanket `E_INTERNAL`
/// (`From<serde_json::Error>`).  A provider sending bad JSON is an availability
/// problem, not an engine bug — and it must never escalate into a hard engine
/// failure (P7-2).
#[derive(Debug)]
pub struct AiJsonError(pub serde_json::Error);

impl std::fmt::Display for AiJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Truncate a provider error body so a huge HTML error page cannot end up in an
/// IPC reply, while keeping enough of it to be actionable.
pub fn truncate_for_detail(body: &str) -> String {
    const LIMIT: usize = 2048;
    let trimmed = body.trim();
    if trimmed.len() <= LIMIT {
        return trimmed.to_string();
    }
    let mut end = LIMIT;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [{} bytes total]", &trimmed[..end], trimmed.len())
}

/// The uniform "publish this failure through the event stream" helper.
///
/// `AiProvider::chat_completions` returns `Result<AiUsage>` and the front end
/// learns about a failure through `ai.finished`; without this the error would
/// only exist as a Rust value.  The helper keeps the outcome `Err` — the
/// contract forbids silent degradation — but also records the original
/// message/detail in the event stream.
pub fn report_generation_error(
    request_id: &str,
    err: &PrincessError,
    events: &mut dyn princess_core::EventSink,
) -> Result<()> {
    let line = match &err.detail {
        Some(detail) => format!("ai {}: {} ({})\n", err.code, err.message, detail),
        None => format!("ai {}: {}\n", err.code, err.message),
    };
    let _ = request_id;
    events.note(&line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_statuses_map_to_the_contract_codes() {
        let cases = [
            (401, ErrorCode::AiUnavailable),
            (403, ErrorCode::AiUnavailable),
            (404, ErrorCode::AiUnavailable),
            (408, ErrorCode::Timeout),
            (429, ErrorCode::AiUnavailable),
            (500, ErrorCode::AiUnavailable),
            (502, ErrorCode::AiUnavailable),
            (503, ErrorCode::AiUnavailable),
            (418, ErrorCode::AiUnavailable),
        ];
        for (status, expected) in cases {
            let err = for_http_status(status, "{\"error\":{\"message\":\"nope\"}}");
            assert_eq!(err.code, expected, "HTTP {status}");
            assert!(
                err.message.contains(&status.to_string()),
                "the status must be visible: {}",
                err.message
            );
            assert_eq!(err.detail.as_deref(), Some("{\"error\":{\"message\":\"nope\"}}"));
        }
    }

    #[test]
    fn a_bodyless_error_still_maps_and_carries_no_empty_detail() {
        let err = for_http_status(500, "   \n ");
        assert_eq!(err.code, ErrorCode::AiUnavailable);
        assert!(err.detail.is_none(), "empty detail is not evidence");
    }

    #[test]
    fn a_huge_error_page_is_truncated_on_a_char_boundary() {
        let body = format!("{}é{}", "a".repeat(2047), "b".repeat(100));
        let detail = truncate_for_detail(&body);
        assert!(detail.contains("bytes total"), "{detail}");
        assert!(detail.len() < body.len());
        // Still valid UTF-8 (a byte-slice would have panicked or split the é).
        assert!(detail.chars().count() > 0);
    }

    #[test]
    fn every_ai_failure_lands_on_a_closed_set_code() {
        let codes = [
            for_http_status(401, "").code,
            for_transport("connect", &"refused").code,
            for_timeout(1500).code,
            for_cancelled("req-1").code,
            for_malformed("sse", &"bad").code,
            for_io("read", &std::io::Error::new(std::io::ErrorKind::WouldBlock, "x")).code,
        ];
        for code in codes {
            assert!(
                ErrorCode::ALL.contains(&code),
                "{code} is not part of the frozen contract set"
            );
        }
        assert_eq!(for_timeout(1).code, ErrorCode::Timeout);
        assert_eq!(for_cancelled("r").code, ErrorCode::Cancelled);
        assert_eq!(for_malformed("sse", &"x").code, ErrorCode::AiUnavailable);
    }

    #[test]
    fn api_errors_are_never_internal_errors() {
        // The P7-2 rule as a test: a provider failure may not masquerade as an
        // engine bug, because E_INTERNAL is the code the shell treats as fatal.
        for err in [
            for_http_status(500, "boom"),
            for_transport("connect", &"refused"),
            for_malformed("json", &"bad"),
        ] {
            assert_ne!(err.code, ErrorCode::Internal);
        }
    }
}
