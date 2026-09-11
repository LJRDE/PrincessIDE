//! `[ai]` configuration resolution — `docs/spec/10-contracts.md` §4.
//!
//! The manifest section is deliberately tiny and vendor-neutral:
//!
//! ```toml
//! [ai]
//! provider = "openai-compatible"   # | "none"
//! base_url = ""                    # empty => global setting, else env, else unavailable
//! model    = ""                    # empty => global setting, else E_AI_UNAVAILABLE
//! ```
//!
//! Everything the network layer needs is resolved **here and only here**, so
//! there is exactly one place that decides "do we have a usable provider?".
//! That matters for **P7-2**: when the answer is no, the caller gets an
//! `E_AI_UNAVAILABLE` [`PrincessError`] and nothing else in the IDE changes.
//!
//! ## Precedence (first non-empty wins)
//!
//! 1. `princess.toml` `[ai].base_url` / `[ai].model`
//! 2. environment — an [OpenAI client convention]:
//!    `PRINCESSIDE_AI_BASE_URL`, then `OPENAI_BASE_URL`, then `OPENAI_API_BASE`
//!    (and the `*_MODEL` equivalents).
//! 3. the global application setting, passed in by the caller (the Tauri shell
//!    owns that store; this crate never reads a file the engine did not hand it).
//!
//! A `base_url` that does not carry a scheme gets `http://` — this is a *local*
//! convention, not a guess about the vendor, and it is what makes
//! `base_url = "localhost:11434/v1"` work for ollama / llama.cpp.
//!
//! [OpenAI client convention]: https://platform.openai.com/docs/api-reference

use princess_core::config::{AiProviderKind, AiSection, ProjectConfig};
use princess_core::{ErrorCode, PrincessError};

use crate::AiConfig;

/// Environment variable consulted first for the endpoint, before the
/// vendor-standard `OPENAI_BASE_URL` / `OPENAI_API_BASE`.
pub const ENV_BASE_URL: &str = "PRINCESSIDE_AI_BASE_URL";
/// Environment variable consulted first for the model name.
pub const ENV_MODEL: &str = "PRINCESSIDE_AI_MODEL";
/// Vendor-standard endpoint variables, consulted in this order.
pub const ENV_OPENAI_BASE_URLS: [&str; 2] = ["OPENAI_BASE_URL", "OPENAI_API_BASE"];
/// Vendor-standard model variable.
pub const ENV_OPENAI_MODEL: &str = "OPENAI_MODEL";
/// The API key.  It lives **only** in the environment: `princess.toml` is a
/// project file that gets committed, so it must never be the place a secret is
/// read from (the manifest has no key field at all — see contract §4).
pub const ENV_API_KEY: &str = "OPENAI_API_KEY";
/// Endpoint used when the manifest, the environment and the global setting are
/// all silent.  `http://`, not `https://`: this default exists for local
/// inference servers (ollama, llama.cpp, vLLM), and a remote vendor always
/// needs an explicit `base_url` anyway.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434/v1";
/// Default endpoint path appended to `base_url`.
pub const DEFAULT_CHAT_PATH: &str = "/chat/completions";

/// The global application setting, as owned by the front end / Tauri shell.
///
/// Both fields are optional and empty means "not set", mirroring the manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalAiSettings {
    pub base_url: Option<String>,
    pub model: Option<String>,
}

impl GlobalAiSettings {
    pub fn new(base_url: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: Some(base_url.into()),
            model: Some(model.into()),
        }
    }

    fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref().filter(|s| !s.trim().is_empty())
    }

    fn model(&self) -> Option<&str> {
        self.model.as_deref().filter(|s| !s.trim().is_empty())
    }
}

/// A borrowed environment lookup, so tests never have to mutate the process
/// environment (which is racy and, since Rust 2024, unsafe).
pub type EnvLookup<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Resolve the manifest's `[ai]` section plus the ambient environment and the
/// global setting into an [`AiConfig`].
///
/// Returns `Ok(None)` when no provider is configured — the caller then answers
/// `E_AI_UNAVAILABLE` (P7-2).  A *malformed* manifest field is an
/// `E_INVALID_CONFIG` error raised by `princess-core` before we get here.
pub fn resolve_ai_config(
    manifest: &AiSection,
    global: &GlobalAiSettings,
    env: &EnvLookup<'_>,
) -> Option<AiConfig> {
    if manifest.provider == AiProviderKind::None {
        return None;
    }

    let base_url = pick([
        Some(manifest.base_url.as_str()),
        env_base_url(env).as_deref(),
        global.base_url(),
    ])?;
    let model = pick([
        Some(manifest.model.as_str()),
        env_model(env).as_deref(),
        global.model(),
    ])?;

    Some(AiConfig {
        base_url: normalize_base_url(&base_url),
        model: model.to_string(),
        api_key: env(ENV_API_KEY),
    })
}

/// Convenience: resolve straight from a parsed manifest.
pub fn resolve_from_project(
    config: &ProjectConfig,
    global: &GlobalAiSettings,
    env: &EnvLookup<'_>,
) -> Option<AiConfig> {
    resolve_ai_config(&config.ai, global, env)
}

/// The environment as a closure, for callers that really do want the process
/// environment.
pub fn process_env() -> impl Fn(&str) -> Option<String> {
    |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_base_url(env: &EnvLookup<'_>) -> Option<String> {
    let mut candidates = vec![env(ENV_BASE_URL)];
    candidates.extend(ENV_OPENAI_BASE_URLS.iter().map(|name| env(name)));
    candidates.into_iter().flatten().find(|v| !v.trim().is_empty())
}

fn env_model(env: &EnvLookup<'_>) -> Option<String> {
    env(ENV_MODEL)
        .or_else(|| env(ENV_OPENAI_MODEL))
        .filter(|v| !v.trim().is_empty())
}

fn pick<const N: usize>(candidates: [Option<&str>; N]) -> Option<String> {
    candidates
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

/// Normalise a user-supplied endpoint.
///
/// * surrounding whitespace is dropped;
/// * a missing scheme becomes `http://` (local inference servers);
/// * trailing slashes are dropped so `join_path` can be mechanical.
pub fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

/// Append the chat-completions path to a normalised base URL.
///
/// The contract writes the endpoint as `base_url` + `chat/completions`; a
/// `base_url` that already ends in `/chat/completions` (some vendor docs quote
/// the full URL) is used verbatim rather than doubled up.
pub fn chat_completions_url(base_url: &str) -> String {
    let base = normalize_base_url(base_url);
    if base.ends_with("/chat/completions") {
        base
    } else {
        format!("{base}{DEFAULT_CHAT_PATH}")
    }
}

/// The single error every "no provider" path produces (P7-2).
///
/// `detail` carries the raw evidence — which fields were missing, or the
/// original transport failure text — because the contract forbids paraphrasing
/// (§0 principle 4).
pub fn unavailable(message: impl Into<String>) -> PrincessError {
    PrincessError::new(ErrorCode::AiUnavailable, message)
}

/// "AI is switched off in this project" — not an error condition the IDE acts
/// on beyond hiding the AI affordances.
pub fn not_configured(detail: impl Into<String>) -> PrincessError {
    unavailable("no AI provider is configured for this project").with_detail(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn manifest(provider: AiProviderKind, base_url: &str, model: &str) -> AiSection {
        AiSection {
            provider,
            base_url: base_url.to_string(),
            model: model.to_string(),
        }
    }

    #[test]
    fn provider_none_never_resolves_even_with_a_full_environment() {
        let env = env_of(&[
            (ENV_BASE_URL, "http://127.0.0.1:9/v1"),
            (ENV_MODEL, "m"),
        ]);
        let resolved = resolve_ai_config(&manifest(AiProviderKind::None, "", ""), &GlobalAiSettings::default(), &env);
        assert_eq!(resolved, None, "provider = \"none\" must win over the environment");
    }

    #[test]
    fn manifest_beats_environment_beats_global() {
        let env = env_of(&[
            (ENV_BASE_URL, "http://127.0.0.1:2/v1"),
            (ENV_MODEL, "env-model"),
        ]);
        let global = GlobalAiSettings::new("http://127.0.0.1:3/v1", "global-model");

        let from_manifest = resolve_ai_config(
            &manifest(AiProviderKind::OpenAiCompatible, "http://127.0.0.1:1/v1", "manifest-model"),
            &global,
            &env,
        )
        .unwrap();
        assert_eq!(from_manifest.base_url, "http://127.0.0.1:1/v1");
        assert_eq!(from_manifest.model, "manifest-model");

        let from_env = resolve_ai_config(&manifest(AiProviderKind::OpenAiCompatible, "", ""), &global, &env).unwrap();
        assert_eq!(from_env.base_url, "http://127.0.0.1:2/v1");
        assert_eq!(from_env.model, "env-model");

        let from_global = resolve_ai_config(
            &manifest(AiProviderKind::OpenAiCompatible, "  ", ""),
            &global,
            &env_of(&[]),
        )
        .unwrap();
        assert_eq!(from_global.base_url, "http://127.0.0.1:3/v1");
        assert_eq!(from_global.model, "global-model");
    }

    #[test]
    fn a_missing_model_degrades_instead_of_guessing_one() {
        let resolved = resolve_ai_config(
            &manifest(AiProviderKind::OpenAiCompatible, "http://127.0.0.1:1/v1", ""),
            &GlobalAiSettings::default(),
            &env_of(&[]),
        );
        assert_eq!(resolved, None, "the engine must not invent a model name");
    }

    #[test]
    fn a_missing_base_url_invents_nothing_and_env_only_cannot_resolve() {
        // No base_url anywhere: the model alone is not enough to talk to a server.
        let resolved = resolve_ai_config(
            &manifest(AiProviderKind::OpenAiCompatible, "", "some-model"),
            &GlobalAiSettings::default(),
            &env_of(&[(ENV_MODEL, "from-env")]),
        );
        assert_eq!(resolved, None);
    }

    #[test]
    fn globally_configured_provider_with_no_project_fields_still_resolves() {
        // The realistic desktop case: everything lives in the global setting.
        let resolved = resolve_ai_config(
            &manifest(AiProviderKind::OpenAiCompatible, "", ""),
            &GlobalAiSettings::new("http://127.0.0.1:11434/v1", "llama3"),
            &env_of(&[]),
        )
        .unwrap();
        assert_eq!(resolved.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(resolved.model, "llama3");
    }

    #[test]
    fn base_url_is_normalised_for_local_servers() {
        assert_eq!(normalize_base_url("127.0.0.1:11434/v1/"), "http://127.0.0.1:11434/v1");
        assert_eq!(normalize_base_url("  http://x/v1/  "), "http://x/v1");
        assert_eq!(normalize_base_url("https://api.example.com/v1"), "https://api.example.com/v1");
        assert_eq!(
            chat_completions_url("http://127.0.0.1:11434/v1"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("http://x/v1/chat/completions"),
            "http://x/v1/chat/completions",
            "an already-complete endpoint must not be doubled"
        );
    }

    #[test]
    fn the_api_key_only_ever_comes_from_the_environment() {
        let env = env_of(&[
            (ENV_BASE_URL, "http://127.0.0.1:1/v1"),
            (ENV_MODEL, "m"),
            (ENV_API_KEY, "sk-test"),
        ]);
        let resolved = resolve_ai_config(&manifest(AiProviderKind::OpenAiCompatible, "", ""), &GlobalAiSettings::default(), &env).unwrap();
        assert_eq!(resolved.api_key.as_deref(), Some("sk-test"));

        let no_key = resolve_ai_config(
            &manifest(AiProviderKind::OpenAiCompatible, "", ""),
            &GlobalAiSettings::new("http://127.0.0.1:1/v1", "m"),
            &env_of(&[]),
        )
        .unwrap();
        assert!(no_key.api_key.is_none());
        assert!(!no_key.authorization_header().is_some());
    }

    #[test]
    fn vendor_standard_openai_variables_are_honoured() {
        let env = env_of(&[
            ("OPENAI_BASE_URL", "http://127.0.0.1:7/v1"),
            ("OPENAI_MODEL", "vendor-model"),
        ]);
        let resolved = resolve_ai_config(&manifest(AiProviderKind::OpenAiCompatible, "", ""), &GlobalAiSettings::default(), &env).unwrap();
        assert_eq!(resolved.base_url, "http://127.0.0.1:7/v1");
        assert_eq!(resolved.model, "vendor-model");
    }

    #[test]
    fn the_unavailable_error_carries_the_contract_code() {
        let err = not_configured("[ai].provider = \"none\"");
        assert_eq!(err.code, ErrorCode::AiUnavailable);
        assert_eq!(err.code.as_str(), "E_AI_UNAVAILABLE");
        assert_eq!(err.detail.as_deref(), Some("[ai].provider = \"none\""));
    }
}
