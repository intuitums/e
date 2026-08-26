//! The provider registry: providers are data, dialects are code — the
//! reference architecture. Each file in `data/` declares one provider:
//! its gateway, dialect, auth surface (which OAuth flow and/or which API-key
//! env var), and seed models with per-model compat (context window, effort
//! support). Adding an API-key provider is a data edit; only a bespoke OAuth
//! flow needs code.

use serde::Deserialize;
use std::sync::OnceLock;

use super::catalog::Api;

#[derive(Deserialize)]
pub struct Provider {
    pub name: String,
    pub display: String,
    pub base_url: String,
    api: String,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub models: Vec<ModelDecl>,
}

#[derive(Deserialize, Default)]
pub struct Auth {
    /// The OAuth flow this provider signs in with ("codex", "xai-device").
    #[serde(default)]
    pub oauth: Option<String>,
    #[serde(default)]
    pub oauth_hint: String,
    /// Whether an API key signs in here.
    #[serde(default)]
    pub key: bool,
    /// The conventional environment variable holding that key.
    #[serde(default)]
    pub key_env: Option<String>,
    #[serde(default)]
    pub key_hint: String,
    /// No credential at all — a local backend (Ollama, LM Studio) that
    /// accepts any bearer. Such providers count as signed in always.
    #[serde(default)]
    pub none: bool,
}

#[derive(Deserialize)]
pub struct ModelDecl {
    pub id: String,
    #[serde(default = "default_window")]
    pub context_window: u64,
    #[serde(default)]
    pub efforts: Vec<String>,
    /// Which thinking wire shape this model speaks (Anthropic dialect).
    #[serde(default)]
    pub thinking: Option<String>,
    /// Output ceiling in tokens, when it's below the dialect's own default
    /// (e.g. claude-haiku-4-5's ~8k against the Anthropic dialect's 32k).
    #[serde(default)]
    pub max_output: Option<u64>,
}

fn default_window() -> u64 {
    200_000
}

impl Provider {
    /// The wire dialect this provider speaks. `all()` validates the string
    /// at startup, so an unknown value never reaches this match silently.
    pub fn api(&self) -> Api {
        Api::parse(&self.api)
            .unwrap_or_else(|| panic!("provider {}: unknown api dialect `{}`", self.name, self.api))
    }
}

/// Every built-in provider, parsed once from the embedded data.
pub fn all() -> &'static [Provider] {
    static REGISTRY: OnceLock<Vec<Provider>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        [
            include_str!("data/opencode-go.json"),
            include_str!("data/opencode-zen.json"),
            include_str!("data/openai-codex.json"),
            include_str!("data/xai.json"),
            include_str!("data/openai.json"),
            include_str!("data/anthropic.json"),
            include_str!("data/vercel.json"),
            include_str!("data/google.json"),
            include_str!("data/groq.json"),
            include_str!("data/mistral.json"),
            include_str!("data/deepseek.json"),
            include_str!("data/cerebras.json"),
            include_str!("data/openrouter.json"),
            include_str!("data/together.json"),
            include_str!("data/fireworks.json"),
            include_str!("data/ollama.json"),
            include_str!("data/lmstudio.json"),
        ]
        .iter()
        .map(|json| {
            let provider: Provider =
                serde_json::from_str(json).expect("embedded provider data is valid");
            // Fail at startup, not on the wire: a typo'd dialect or OAuth
            // flow is a data bug and must not fall back silently.
            provider.api();
            if let Some(flow) = &provider.auth.oauth {
                assert!(
                    matches!(flow.as_str(), "codex" | "xai-device"),
                    "provider {}: unknown oauth flow `{flow}`",
                    provider.name
                );
            }
            for decl in &provider.models {
                assert!(
                    matches!(
                        decl.thinking.as_deref(),
                        None | Some("adaptive") | Some("manual")
                    ),
                    "provider {}: model {}: unknown thinking mode `{:?}`",
                    provider.name,
                    decl.id,
                    decl.thinking
                );
            }
            provider
        })
        .collect()
    })
}

pub fn find(name: &str) -> Option<&'static Provider> {
    all().iter().find(|p| p.name == name)
}

/// Providers the account panel offers (an OAuth flow exists).
pub fn oauth_providers() -> Vec<&'static Provider> {
    all().iter().filter(|p| p.auth.oauth.is_some()).collect()
}

/// Providers the API-key panel offers.
pub fn key_providers() -> Vec<&'static Provider> {
    all().iter().filter(|p| p.auth.key).collect()
}
