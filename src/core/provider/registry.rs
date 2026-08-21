//! The provider registry: providers are data, dialects are code — the
//! reference architecture. Each file in `providers/` declares one provider:
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
}

#[derive(Deserialize)]
pub struct ModelDecl {
    pub id: String,
    #[serde(default = "default_window")]
    pub context_window: u64,
    #[serde(default)]
    pub efforts: Vec<String>,
}

fn default_window() -> u64 {
    200_000
}

impl Provider {
    pub fn api(&self) -> Api {
        match self.api.as_str() {
            "codex-responses" | "openai-responses" | "responses" => Api::Responses,
            "anthropic-messages" | "anthropic" => Api::Anthropic,
            _ => Api::Completions,
        }
    }
}

/// Every built-in provider, parsed once from the embedded data.
pub fn all() -> &'static [Provider] {
    static REGISTRY: OnceLock<Vec<Provider>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        [
            include_str!("providers/opencode-go.json"),
            include_str!("providers/opencode.json"),
            include_str!("providers/openai-codex.json"),
            include_str!("providers/xai.json"),
            include_str!("providers/openai.json"),
            include_str!("providers/anthropic.json"),
        ]
        .iter()
        .map(|json| serde_json::from_str(json).expect("embedded provider data is valid"))
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
