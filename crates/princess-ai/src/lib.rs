//! # princess-ai
//!
//! The P7 AI layer: **one vendor-neutral provider abstraction and one
//! OpenAI-compatible implementation** of it, plus the configuration plumbing and
//! the degrade path that keeps the rest of the IDE alive when AI is not.
//!
//! Contract: `docs/spec/10-contracts.md` §2 (`ai.chunk` / `ai.finished`), §3
//! (`E_AI_UNAVAILABLE`), §4 (`[ai]`), §5 ([`princess_core::AiProvider`]).
//!
//! ## The shape of the thing
//!
//! | module | responsibility |
//! |---|---|
//! | [`config`] | resolve `[ai]` + environment + global setting into [`AiConfig`] |
//! | [`openai`] | [`OpenAiProvider`]: request building, streaming, cancellation |
//! | [`sse`] | SSE framing and the OpenAI `chat.completion.chunk` shape |
//! | [`http`] | a bounded, deadline-everywhere HTTP/1.1 POST (no external client) |
//! | [`error_map`] | every failure classified onto the closed contract code set |
//! | [`mock`] | the local mock server the P7-1 tests run against |
//!
//! ## The two rules this crate exists to satisfy
//!
//! **P7-2 — AI being down may not touch anything else.**  Everything that can go
//! wrong ends in one of three contract codes (`E_AI_UNAVAILABLE`, `E_TIMEOUT`,
//! `E_CANCELLED`) and returns as a plain `Err`.  There is no panic path, no
//! unbounded wait ([`openai::DEFAULT_TIMEOUT_MS`] or the caller's deadline), no
//! global state another module can observe, and no behaviour change anywhere
//! else in the engine.  A project with `provider = "none"` never opens a socket
//! at all.
//!
//! **P7-3 — real calls are opt-in.**  Nothing in this crate performs a network
//! request at construction time: [`openai::OpenAiProvider::from_project`] only
//! resolves strings, and [`openai::OpenAiProvider::status`] is explicitly
//! offline.  The one test that talks to a real endpoint is gated behind
//! [`REAL_AI_ENV`] and skipped by default.
//!
//! ## Example
//!
//! ```no_run
//! use princess_ai::{GlobalAiSettings, OpenAiProvider};
//! use princess_core::config::ProjectConfig;
//! use princess_core::traits::CancelToken;
//! use princess_core::types::{ChatMessage, ChatRequest};
//!
//! let config = ProjectConfig::default();
//! let provider = OpenAiProvider::from_project(
//!     &config.ai,
//!     &GlobalAiSettings::default(),
//!     &princess_ai::config::process_env(),
//! )
//! .unwrap_or_else(OpenAiProvider::unconfigured);
//!
//! let request = ChatRequest {
//!     requestId: "req-1".into(),
//!     model: String::new(),
//!     messages: vec![ChatMessage { role: "user".into(), content: "hi".into() }],
//!     stream: true,
//!     temperature: None,
//! };
//! // `events` is any `princess_core::EventSink`; `chat_completions` streams
//! // `ai.chunk` and finishes with the usage for `ai.finished`.
//! ```

pub mod config;
pub mod error_map;
pub mod http;
pub mod mock;
pub mod openai;
pub mod sse;

pub use config::{
    chat_completions_url, normalize_base_url, resolve_ai_config, unavailable, GlobalAiSettings,
    DEFAULT_BASE_URL, DEFAULT_CHAT_PATH, ENV_API_KEY, ENV_BASE_URL, ENV_MODEL,
};
pub use error_map::{for_cancelled, for_http_status, for_timeout, for_transport};
pub use openai::{AiConfig, AiStatus, OpenAiProvider, ProviderOptions, DEFAULT_TIMEOUT_MS};

/// Environment variable that opts the real-endpoint test in (P7-3).
///
/// The value is the model name to ask for; the endpoint comes from
/// `PRINCESSIDE_AI_BASE_URL` / `OPENAI_BASE_URL` and the key from
/// `OPENAI_API_KEY`.  Tests that need this variable are `#[ignore]`d, so plain
/// `cargo test -p princess-ai` never reaches the network even on a machine where
/// the variable happens to be set.
pub const REAL_AI_ENV: &str = "PRINCESSIDE_AI_REAL_TEST";
