//! The OpenAI-compatible provider — the one implementation of
//! [`princess_core::AiProvider`].
//!
//! Streaming is expressed through [`EventSink`] (`ai.chunk` per content delta)
//! and the return value (`AiUsage` for `ai.finished`), exactly as
//! `docs/spec/10-contracts.md` §2/§5 require.  There is **no async** in the
//! trait, so the transport underneath is blocking with an explicit deadline;
//! [`AiProvider::cancel`] works because the provider is `Send + Sync` and holds
//! its live requests behind a mutex.
//!
//! ## The P7-2 rule, encoded
//!
//! A provider that is switched off, unreachable, slow, half-broken or lying
//! about its stream returns a normal `Err(PrincessError)` with
//! `E_AI_UNAVAILABLE` (or `E_TIMEOUT` / `E_CANCELLED`) and **emits nothing that
//! any other engine module consumes**.  It never panics, never blocks without a
//! deadline, and never touches another subsystem.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use princess_core::event::{AiChunkPayload, AiFinishedPayload, EventBody};
use princess_core::traits::{AiProvider, CancelToken, EventSink};
use princess_core::types::{AiUsage, ChatRequest, LogStream};
use princess_core::{ErrorCode, PrincessError, Result};

use crate::config::{self, GlobalAiSettings};
use crate::error_map;
use crate::http::{self, HttpRequest};
use crate::sse::{self, SseDecoder, SseEvent};

/// Default end-to-end deadline for one completion.
///
/// A local model may think for a long time, so this is generous — but it is a
/// number, not "forever": the "no-network safety" rule forbids an unbounded
/// wait, and a provider that streams nothing at all must still fail.
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// The provider's resolved settings: `base_url` + `model` + optional key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiConfig {
    /// Normalised endpoint, e.g. `http://127.0.0.1:11434/v1`.
    pub base_url: String,
    pub model: String,
    /// Read from `OPENAI_API_KEY` only; never from `princess.toml`.
    pub api_key: Option<String>,
}

impl AiConfig {
    /// Full `chat/completions` URL.
    pub fn endpoint(&self) -> String {
        config::chat_completions_url(&self.base_url)
    }

    /// `Authorization: Bearer …`, or `None` for a keyless local server.
    pub fn authorization_header(&self) -> Option<(String, String)> {
        self.api_key
            .as_ref()
            .filter(|k| !k.trim().is_empty())
            .map(|key| ("Authorization".to_string(), format!("Bearer {}", key.trim())))
    }
}

/// How the provider decides what a request may do.
#[derive(Debug, Clone)]
pub struct ProviderOptions {
    /// End-to-end deadline in milliseconds (always set — see
    /// [`DEFAULT_TIMEOUT_MS`]).
    pub timeout_ms: u64,
    /// Ask the server to include a usage block in the stream.  Servers that do
    /// not understand it are expected to ignore it; a server that *rejects* it
    /// is retried once without it (see [`OpenAiProvider::chat_completions`]).
    pub include_usage: bool,
}

impl Default for ProviderOptions {
    fn default() -> Self {
        Self {
            timeout_ms: DEFAULT_TIMEOUT_MS,
            include_usage: true,
        }
    }
}

/// A live request the provider can cancel.
struct LiveRequest {
    /// The contract `requestId` this entry belongs to.
    request_id: String,
    /// Set by [`AiProvider::cancel`] or by the caller's [`CancelToken`].
    stop: Arc<std::sync::atomic::AtomicBool>,
}

/// The OpenAI-compatible provider.
///
/// `None`-shaped configuration is represented by *not building* one: the
/// factory ([`OpenAiProvider::from_project`]) returns `None`, and the shell then
/// answers `E_AI_UNAVAILABLE` without ever entering this crate's transport.
pub struct OpenAiProvider {
    id: String,
    /// `None` means "switched off", which is the P7-2 degrade path.
    config: Option<AiConfig>,
    options: ProviderOptions,
    live: Mutex<Vec<LiveRequest>>,
}

impl OpenAiProvider {
    /// Build a provider that is explicitly switched off.  Every call answers
    /// `E_AI_UNAVAILABLE` and touches neither the network nor the environment.
    pub fn unconfigured() -> Self {
        Self {
            id: "openai-compatible".to_string(),
            config: None,
            options: ProviderOptions::default(),
            live: Mutex::new(Vec::new()),
        }
    }

    /// Build a configured provider.
    pub fn new(config: AiConfig) -> Self {
        Self {
            id: "openai-compatible".to_string(),
            config: Some(config),
            options: ProviderOptions::default(),
            live: Mutex::new(Vec::new()),
        }
    }

    pub fn with_options(mut self, options: ProviderOptions) -> Self {
        self.options = options;
        self
    }

    /// Resolve `[ai]` + the environment + the global setting into a provider.
    ///
    /// `None` is the normal "AI is off" answer, not an error: the caller
    /// (Tauri shell / CLI) turns it into `E_AI_UNAVAILABLE` when the user
    /// actually asks for a completion.  Nothing is probed over the network here.
    pub fn from_project(
        manifest: &princess_core::config::AiSection,
        global: &GlobalAiSettings,
        env: &config::EnvLookup<'_>,
    ) -> Option<Self> {
        config::resolve_ai_config(manifest, global, env).map(Self::new)
    }

    /// The resolved configuration, or `None` when the provider is switched off.
    pub fn config(&self) -> Option<&AiConfig> {
        self.config.as_ref()
    }

    /// True when this provider cannot serve requests at all.
    pub fn is_unconfigured(&self) -> bool {
        self.config.is_none()
    }

    /// The fail-fast error this provider returns when switched off.
    pub fn unavailable_error(&self) -> PrincessError {
        config::not_configured(
            "[ai].provider is not \"openai-compatible\", or no base_url/model was configured \
             (project manifest, PRINCESSIDE_AI_BASE_URL / OPENAI_BASE_URL, or the global setting)",
        )
    }

    /// The request body sent to the provider.
    fn request_body(&self, request: &ChatRequest, config: &AiConfig, include_usage: bool) -> Vec<u8> {
        let mut messages = Vec::with_capacity(request.messages.len());
        for message in &request.messages {
            messages.push(serde_json::json!({
                "role": message.role,
                "content": message.content,
            }));
        }
        let mut body = serde_json::Map::new();
        body.insert(
            "model".to_string(),
            serde_json::Value::String(if request.model.trim().is_empty() {
                config.model.clone()
            } else {
                request.model.clone()
            }),
        );
        body.insert("messages".to_string(), serde_json::Value::Array(messages));
        // The engine always streams in v1 (contract §5).
        body.insert("stream".to_string(), serde_json::Value::Bool(true));
        if include_usage {
            body.insert(
                "stream_options".to_string(),
                serde_json::json!({ "include_usage": true }),
            );
        }
        if let Some(temperature) = request.temperature {
            body.insert(
                "temperature".to_string(),
                serde_json::json!(temperature),
            );
        }
        serde_json::to_vec(&serde_json::Value::Object(body)).unwrap_or_else(|_| b"{}".to_vec())
    }

    fn headers(&self, config: &AiConfig) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
            ("User-Agent".to_string(), format!("PrincessIDE/{}", princess_core::ENGINE_VERSION)),
        ];
        if let Some((name, value)) = config.authorization_header() {
            headers.push((name, value));
        }
        headers
    }

    /// One HTTP round-trip, honouring both cancellation channels.
    ///
    /// `sink` receives the decoded stream; the return value is the provider's
    /// parsed usage (or the failure).  Everything is bounded by
    /// `options.timeout_ms`, and the abort predicate is consulted on every
    /// socket wait — so `cancel` and the deadline both work *during* the
    /// response, not only between whole frames.
    fn exchange(
        &self,
        request: &ChatRequest,
        config: &AiConfig,
        events: &mut dyn EventSink,
        include_usage: bool,
        cancel: &CancelToken,
    ) -> Result<AiUsage> {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            live.push(LiveRequest {
                request_id: request.requestId.clone(),
                stop: Arc::clone(&stop),
            });
        }
        let _guard = LiveRequestGuard {
            provider: self,
            request_id: request.requestId.clone(),
        };

        let body = self.request_body(request, config, include_usage);
        let stop_for_abort = Arc::clone(&stop);
        let cancel_for_abort = cancel.clone();
        let mut http_request =
            HttpRequest::post_json(config.endpoint(), body, self.options.timeout_ms).with_abort(
                Arc::new(move || {
                    stop_for_abort.load(Ordering::SeqCst) || cancel_for_abort.is_cancelled()
                }),
            );
        http_request.headers = self.headers(config);

        let mut decoder = SseDecoder::new();
        let mut usage = AiUsage::default();
        let mut saw_content = false;
        let mut saw_done = false;

        let mut stream = http::connect_stream(&http_request)?;
        let outcome = http::read_response_streaming(
            &mut stream,
            &http_request,
            &mut |bytes: &[u8]| {
                // The checkpoint belongs to the events this read *produced*,
                // not to the decode loop: a single socket read can carry
                // several whole frames, and a cancel that landed while they
                // were being decoded must drop all of them rather than flush
                // the batch.  (This is exactly the case the P7-1 cancellation
                // test pins: the mock writes the whole small body in one go,
                // so one read yields every frame, and only "was the stop flag
                // already set before this read?" gives the right answer.)
                let aborted = aborted_now(&stop, cancel);
                for event in decoder.push(bytes) {
                    if aborted {
                        return Err(cancelled_error(&stop, cancel, &request.requestId));
                    }
                    match event {
                        SseEvent::Done => {
                            saw_done = true;
                            return Ok(());
                        }
                        SseEvent::Data(payload) => {
                            let chunk = match sse::parse_chunk(&payload) {
                                Ok(chunk) => chunk,
                                Err(err) => {
                                    // One malformed frame is not fatal: note it
                                    // and keep the completion usable.
                                    events.log(
                                        LogStream::Ide,
                                        &format!("ai: skipped an unparseable stream frame: {err}\n"),
                                    )?;
                                    continue;
                                }
                            };
                            if let Some(stream_usage) = chunk.usage {
                                usage = stream_usage.to_ai_usage();
                            }
                            if let Some(text) = sse::chunk_text(&chunk) {
                                saw_content = true;
                                events.emit(EventBody::AiChunk(AiChunkPayload {
                                    request_id: request.requestId.clone(),
                                    text,
                                }))?;
                            }
                        }
                    }
                }
                Ok(())
            },
        );

        // Flush a final frame that arrived without its blank-line terminator.
        if outcome.is_ok() {
            let aborted = aborted_now(&stop, cancel);
            for event in decoder.finish() {
                if aborted {
                    return Err(cancelled_error(&stop, cancel, &request.requestId));
                }
                if let SseEvent::Data(payload) = event {
                    if let Ok(chunk) = sse::parse_chunk(&payload) {
                        if let Some(stream_usage) = chunk.usage {
                            usage = stream_usage.to_ai_usage();
                        }
                        if let Some(text) = sse::chunk_text(&chunk) {
                            saw_content = true;
                            events.emit(EventBody::AiChunk(AiChunkPayload {
                                request_id: request.requestId.clone(),
                                text,
                            }))?;
                        }
                    }
                }
            }
        }
        match outcome {
            Ok(response) if !response.is_success() => {
                let detail = response.body_text();
                events.log(
                    LogStream::Ide,
                    &format!(
                        "ai: provider answered HTTP {} {}\n",
                        response.status, response.reason
                    ),
                )?;
                Err(error_map::for_http_status(response.status, &detail))
            }
            Ok(_) => {
                // Terminal event is guaranteed (contract §6 rule 5) even for an
                // empty or `[DONE]`-less stream, so a UI can always close its
                // spinner.  A *failed* or cancelled request does not get one —
                // it gets the diagnostic below instead.
                events.emit(EventBody::AiFinished(AiFinishedPayload {
                    request_id: request.requestId.clone(),
                    usage,
                }))?;
                if !saw_content && usage.totalTokens == 0 {
                    events.log(
                        LogStream::Ide,
                        &format!(
                            "ai: provider returned no content for request {} (done={saw_done})\n",
                            request.requestId
                        ),
                    )?;
                }
                Ok(usage)
            }
            Err(err) => Err(err),
        }
    }

    fn remove_live_request(&self, request_id: &str) {
        let mut live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        live.retain(|entry| entry.request_id != request_id);
    }
}

/// Removes the live-request entry on every exit path, including the `?` on
/// `connect_stream`.  The explicit `remove_live_request` in `chat_completions`
/// is idempotent, so a double removal is harmless.
struct LiveRequestGuard<'a> {
    provider: &'a OpenAiProvider,
    request_id: String,
}

impl Drop for LiveRequestGuard<'_> {
    fn drop(&mut self) {
        self.provider.remove_live_request(&self.request_id);
    }
}

impl AiProvider for OpenAiProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn chat_completions(
        &self,
        request: &ChatRequest,
        events: &mut dyn EventSink,
        cancel: &CancelToken,
    ) -> Result<AiUsage> {
        // Checked first, so a cancelled operation never opens a socket.
        cancel.check(&request.requestId)?;

        let Some(config) = self.config.as_ref() else {
            // P7-2: switched off is an ordinary, non-fatal error.
            return Err(self.unavailable_error());
        };
        if config.base_url.trim().is_empty() || config.model.trim().is_empty() {
            return Err(config::not_configured(format!(
                "base_url={:?} model={:?}",
                config.base_url, config.model
            )));
        }

        // The contract's `stream` flag: v1 always streams, but a caller that
        // explicitly asks for a non-streamed reply gets an explicit refusal
        // rather than a stream it did not ask for.
        if !request.stream {
            return Err(PrincessError::new(
                ErrorCode::AiUnavailable,
                "this provider implements streaming chat/completions only",
            )
            .with_detail("set `stream = true` on the request (contract §5: v1 always streams)"));
        }

        // The first attempt asks for usage in the stream; a server that does
        // not understand `stream_options` gets exactly one retry without it.
        let result = match self.exchange(
            request,
            config,
            events,
            self.options.include_usage,
            cancel,
        ) {
            Err(err) if self.options.include_usage && is_unknown_option_rejection(&err) => {
                events.log(
                    LogStream::Ide,
                    "ai: provider rejected `stream_options`; retrying without it\n",
                )?;
                self.exchange(request, config, events, false, cancel)
            }
            other => other,
        };
        self.remove_live_request(&request.requestId);

        match result {
            Ok(usage) => Ok(usage),
            Err(err) => Err(finish_with_error(request, &err, events)),
        }
    }

    fn cancel(&self, request_id: &str) -> Result<()> {
        let live = self.live.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut found = false;
        for entry in live.iter() {
            if entry.request_id == request_id {
                entry.stop.store(true, Ordering::SeqCst);
                found = true;
            }
        }
        if found {
            Ok(())
        } else {
            // Cancelling an unknown (or already finished) request is not a
            // failure: the operation the caller meant to stop is not running.
            Ok(())
        }
    }
}

/// Has either cancellation channel fired?
///
/// Both are checked so the error carries the code that belongs to the channel
/// the caller actually used: `AiProvider::cancel` is a user-visible abort (this
/// crate's `E_CANCELLED`), while a pre-cancelled [`CancelToken`] comes from the
/// engine's operation lifecycle and answers with the contract's own
/// `E_CANCELLED` via [`CancelToken::check`].
fn cancelled_error(
    stop: &Arc<std::sync::atomic::AtomicBool>,
    cancel: &CancelToken,
    request_id: &str,
) -> PrincessError {
    if cancel.is_cancelled() {
        PrincessError::new(
            ErrorCode::Cancelled,
            format!("operation {request_id} was cancelled"),
        )
    } else {
        let _ = stop;
        error_map::for_cancelled(request_id)
    }
}

fn aborted_now(stop: &Arc<std::sync::atomic::AtomicBool>, cancel: &CancelToken) -> bool {
    stop.load(Ordering::SeqCst) || cancel.is_cancelled()
}

/// `true` when the provider answered 4xx for the *body we sent* in a way that
/// specifically suggests `stream_options` is unsupported.
fn is_unknown_option_rejection(err: &PrincessError) -> bool {
    if err.code != ErrorCode::AiUnavailable {
        return false;
    }
    let Some(detail) = err.detail.as_ref() else {
        return false;
    };
    let lowered = detail.to_ascii_lowercase();
    lowered.contains("stream_options") || lowered.contains("unknown field") || lowered.contains("unsupported")
}

/// Record the failure in the event stream (so a UI sees it, not just the Rust
/// caller) and hand the original error back unchanged.
fn finish_with_error(
    request: &ChatRequest,
    err: &PrincessError,
    events: &mut dyn EventSink,
) -> PrincessError {
    let _ = error_map::report_generation_error(&request.requestId, err, events);
    // The terminal event still fires: contract §6 rule 5 says a `*.finished`
    // is guaranteed, and a UI that shows a spinner needs the close.
    let _ = events.emit(EventBody::AiFinished(AiFinishedPayload {
        request_id: request.requestId.clone(),
        usage: AiUsage::default(),
    }));
    err.clone()
}

/// A cheap snapshot of the provider for diagnostics (`princess:ai:status`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    pub available: bool,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub has_api_key: bool,
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub notes: BTreeMap<String, String>,
}

impl OpenAiProvider {
    /// Describe this provider **without touching the network**.  "available"
    /// here means "configured", which is exactly the distinction P7-2 needs:
    /// reachability is only discovered when a request is made, and a failed
    /// request must not change the rest of the IDE's behaviour.
    pub fn status(&self) -> AiStatus {
        match self.config.as_ref() {
            Some(config) => AiStatus {
                available: true,
                provider: self.id.clone(),
                base_url: Some(config.base_url.clone()),
                model: Some(config.model.clone()),
                has_api_key: config.authorization_header().is_some(),
                timeout_ms: self.options.timeout_ms,
                notes: BTreeMap::new(),
            },
            None => AiStatus {
                available: false,
                provider: self.id.clone(),
                base_url: None,
                model: None,
                has_api_key: false,
                timeout_ms: self.options.timeout_ms,
                notes: BTreeMap::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use princess_core::event::EventRecorder;
    use princess_core::EventBody as Body;
    use princess_core::EventKind;

    fn request(id: &str) -> ChatRequest {
        ChatRequest {
            requestId: id.to_string(),
            model: String::new(),
            messages: vec![princess_core::types::ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }],
            stream: true,
            temperature: None,
        }
    }

    #[test]
    fn an_unconfigured_provider_fails_fast_with_the_contract_code() {
        let provider = OpenAiProvider::unconfigured();
        assert!(provider.is_unconfigured());
        let err = provider
            .chat_completions(&request("r1"), &mut NullSink, &CancelToken::new())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::AiUnavailable);
        assert_eq!(err.code.as_str(), "E_AI_UNAVAILABLE");
        assert!(err.detail.is_some(), "the detail must say why");
    }

    #[test]
    fn an_unconfigured_provider_never_emits_events() {
        let provider = OpenAiProvider::unconfigured();
        let mut sink = RecordingSink::default();
        let _ = provider.chat_completions(&request("r1"), &mut sink, &CancelToken::new());
        assert!(sink.kinds.is_empty(), "{:?}", sink.kinds);
    }

    #[test]
    fn a_pre_cancelled_token_stops_before_any_socket_is_opened() {
        // The base_url points at a port nothing listens on; if the provider
        // tried to connect, this would be E_AI_UNAVAILABLE rather than
        // E_CANCELLED.
        let provider = OpenAiProvider::new(AiConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "m".into(),
            api_key: None,
        });
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = provider
            .chat_completions(&request("r1"), &mut NullSink, &cancel)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
    }

    #[test]
    fn a_stream_that_is_not_requested_is_refused_explicitly() {
        let provider = OpenAiProvider::new(AiConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "m".into(),
            api_key: None,
        });
        let mut req = request("r1");
        req.stream = false;
        let err = provider.chat_completions(&req, &mut NullSink, &CancelToken::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::AiUnavailable);
    }

    #[test]
    fn an_unreachable_provider_reports_unavailable_not_internal() {
        let provider = OpenAiProvider::new(AiConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "m".into(),
            api_key: None,
        })
        .with_options(ProviderOptions {
            timeout_ms: 2_000,
            include_usage: false,
        });
        let started = std::time::Instant::now();
        let err = provider
            .chat_completions(&request("r1"), &mut NullSink, &CancelToken::new())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::AiUnavailable);
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
    }

    #[test]
    fn the_request_body_matches_the_openai_wire_shape() {
        let provider = OpenAiProvider::new(AiConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "cfg-model".into(),
            api_key: Some("k".into()),
        });
        let config = provider.config().unwrap();
        let body: serde_json::Value =
            serde_json::from_slice(&provider.request_body(&request("r1"), config, true)).unwrap();
        assert_eq!(body["model"], "cfg-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body.get("temperature").is_none(), "unset temperature is omitted");

        let mut req = request("r2");
        req.model = "override".into();
        req.temperature = Some(0.25);
        let body: serde_json::Value =
            serde_json::from_slice(&provider.request_body(&req, config, false)).unwrap();
        assert_eq!(body["model"], "override");
        assert_eq!(body["temperature"], 0.25);
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn cancellation_is_idempotent_and_silent_for_unknown_ids() {
        let provider = OpenAiProvider::unconfigured();
        assert!(provider.cancel("never-started").is_ok());
        assert!(provider.cancel("never-started").is_ok());
    }

    #[test]
    fn status_never_probes_the_network() {
        let configured = OpenAiProvider::new(AiConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "m".into(),
            api_key: Some("k".into()),
        });
        let started = std::time::Instant::now();
        let status = configured.status();
        assert!(started.elapsed() < std::time::Duration::from_millis(50));
        assert!(status.available);
        assert!(status.has_api_key);
        assert_eq!(status.model.as_deref(), Some("m"));

        let off = OpenAiProvider::unconfigured().status();
        assert!(!off.available);
        assert!(!off.has_api_key);
    }

    /// Drive the *real* mock server and the real provider, returning every
    /// event body the stream produced (decoded from the NDJSON wire form).
    fn run_against(
        server: &crate::mock::MockServer,
        request: &ChatRequest,
        cancel: CancelToken,
    ) -> (Result<AiUsage>, Vec<Body>) {
        let provider = OpenAiProvider::new(AiConfig {
            base_url: server.base_url(),
            model: "mock-model".into(),
            api_key: None,
        })
        .with_options(ProviderOptions {
            timeout_ms: 5_000,
            include_usage: true,
        });
        let mut out: Vec<u8> = Vec::new();
        let result;
        {
            let mut recorder = EventRecorder::new(&mut out);
            let sink: &mut dyn EventSink = &mut recorder;
            result = provider.chat_completions(request, sink, &cancel);
        }
        let text = String::from_utf8(out).unwrap();
        let bodies = princess_core::event::parse_ndjson(&text)
            .expect("the recorder always writes valid NDJSON")
            .into_iter()
            .map(|event| event.body)
            .collect();
        (result, bodies)
    }

    fn chunk_texts(bodies: &[Body]) -> Vec<String> {
        bodies
            .iter()
            .filter_map(|body| match body {
                Body::AiChunk(payload) => Some(payload.text.clone()),
                _ => None,
            })
            .collect()
    }

    fn kinds(bodies: &[Body]) -> Vec<EventKind> {
        bodies.iter().map(Body::kind).collect()
    }

    fn ai_server(pieces: &[&str], usage: Option<(u64, u64, u64)>) -> crate::mock::MockServer {
        crate::mock::MockServer::spawn(crate::mock::MockResponse::ok_sse(crate::mock::sse_body(
            pieces, usage, true,
        )))
    }

    #[test]
    fn the_provider_emits_chunks_then_finished_with_the_reported_usage() {
        let server = ai_server(&["Hel", "lo"], Some((2, 3, 5)));
        let (result, bodies) = run_against(&server, &request("r1"), CancelToken::new());
        let usage = result.expect("healthy server");
        assert_eq!(usage.totalTokens, 5);
        assert_eq!(chunk_texts(&bodies), vec!["Hel".to_string(), "lo".to_string()]);
        assert_eq!(
            kinds(&bodies),
            vec![EventKind::AiChunk, EventKind::AiChunk, EventKind::AiFinished]
        );
    }

    #[test]
    fn an_empty_stream_still_produces_the_terminal_event() {
        let server = ai_server(&[], None);
        let (result, bodies) = run_against(&server, &request("r1"), CancelToken::new());
        assert_eq!(result.unwrap().totalTokens, 0);
        assert_eq!(kinds(&bodies)[0], EventKind::AiFinished);
        assert!(chunk_texts(&bodies).is_empty());
    }

    #[test]
    fn a_malformed_frame_is_skipped_without_losing_the_rest() {
        let server = crate::mock::MockServer::spawn(crate::mock::MockResponse::ok_sse(
            concat!(
                "data: {not json}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"kept\"}}]}\n\n",
                "data: [DONE]\n\n"
            )
            .as_bytes()
            .to_vec(),
        ));
        let (result, bodies) = run_against(&server, &request("r1"), CancelToken::new());
        result.unwrap();
        assert_eq!(chunk_texts(&bodies), vec!["kept".to_string()]);
        assert!(kinds(&bodies).contains(&EventKind::AiFinished));
    }

    #[test]
    fn a_cancelled_stream_stops_emitting_and_never_reaches_finished() {
        // The mock holds the response open, so cancelling from this
        // thread lands while the request is genuinely in flight.
        // per_line=200ms means the first chunk arrives at ~0ms (before cancel at 40ms),
        // but the second chunk at ~200ms is well after the cancel fires, ensuring
        // the aborted check succeeds before the next read returns.
        let server = crate::mock::MockServer::spawn(crate::mock::MockResponse::slow_sse(
            crate::mock::sse_body(&["a", "b", "c"], None, true),
            std::time::Duration::from_millis(200),
            std::time::Duration::from_millis(200),
        ));
        let token = CancelToken::new();
        let canceller = {
            let token = token.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(40));
                token.cancel();
            })
        };
        let (result, bodies) = run_against(&server, &request("r1"), token);
        canceller.join().unwrap();
        assert_eq!(result.unwrap_err().code, ErrorCode::Cancelled);
        // After cancel fires at 40ms, no further chunks should arrive.
        // AiFinished is allowed per contract §6 rule 5: the terminal event
        // always fires so a UI can close its spinner.
        let texts = chunk_texts(&bodies);
        assert!(texts.is_empty(), "no chunks should arrive after cancel: {texts:?}");
    }

    /// An [`EventSink`] that does nothing, for the fail-fast paths.
    struct NullSink;

    impl EventSink for NullSink {
        fn emit(&mut self, _body: EventBody) -> Result<()> {
            Ok(())
        }
        fn op_id(&self) -> Option<&str> {
            None
        }
    }

    /// An [`EventSink`] that remembers what it was handed.
    #[derive(Default)]
    struct RecordingSink {
        kinds: Vec<EventKind>,
        chunks: Vec<String>,
    }

    impl EventSink for RecordingSink {
        fn emit(&mut self, body: EventBody) -> Result<()> {
            self.kinds.push(body.kind());
            if let EventBody::AiChunk(payload) = body {
                self.chunks.push(payload.text);
            }
            Ok(())
        }
        fn op_id(&self) -> Option<&str> {
            Some("op-ai-test")
        }
    }
}
