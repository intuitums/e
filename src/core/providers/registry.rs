//! The provider registry: providers are data, dialects are code — the
//! reference architecture. Each file in `data/` declares one provider:
//! its gateway, dialect, auth surface (which OAuth flow and/or which API-key
//! env var), and seed models with per-model compat (context window, effort
//! support). Adding an API-key provider is a data edit; only a bespoke OAuth
//! flow needs code.

use serde::Deserialize;
use std::sync::OnceLock;

use super::catalog::Api;

/// How much of a provider deployment e itself continuously verifies. This
/// is intentionally separate from the API dialect: many gateways speak
/// Completions, but that does not make every gateway a native integration.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SupportTier {
    Native,
    #[default]
    Compatible,
    Experimental,
}

impl SupportTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Compatible => "compatible",
            Self::Experimental => "experimental",
        }
    }
}

/// Shape of the model-list endpoint, deliberately independent from the
/// inference dialect. A gateway can accept Anthropic messages while exposing
/// an OpenAI-shaped `/models` response.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatalogStrategy {
    #[default]
    Openai,
    Anthropic,
    Google,
    None,
}

impl CatalogStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::None => "none",
        }
    }
}

/// Where a Responses-dialect deployment mounts inference. This is declared
/// independently from credentials so changing credential representation can
/// never silently change request URLs.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponsesMount {
    #[default]
    Platform,
    Codex,
}

impl ResponsesMount {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Codex => "codex",
        }
    }
}

#[derive(Deserialize)]
pub struct Provider {
    pub name: String,
    pub display: String,
    pub base_url: String,
    api: String,
    #[serde(default)]
    pub tier: SupportTier,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default = "default_true")]
    pub supports_tools: bool,
    #[serde(default)]
    pub catalog: CatalogStrategy,
    #[serde(default)]
    pub responses_mount: ResponsesMount,
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
    pub effort: Vec<String>,
    /// Which thinking wire shape this model speaks (Anthropic dialect).
    #[serde(default)]
    pub thinking: Option<String>,
    /// Output ceiling in tokens, when it's below the dialect's own default
    /// (e.g. claude-haiku-4-5's ~8k against the Anthropic dialect's 32k).
    #[serde(default)]
    pub max_output: Option<u64>,
    #[serde(default = "default_true")]
    pub supports_tools: bool,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub pricing: Option<super::catalog::Pricing>,
}

fn default_window() -> u64 {
    200_000
}

fn default_true() -> bool {
    true
}

impl Provider {
    /// The wire dialect this provider speaks. `all()` validates the string
    /// at startup, so an unknown value never reaches this match silently.
    // Fail-fast for a data bug (registry JSON vs. code) at startup, before
    // any user state is touched — by design, not a runtime panic. Scoped
    // allow, proof: the dialect strings ship inside this binary.
    #[allow(clippy::panic)]
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
            // Parse of include_str! data compiled into this binary: a
            // malformed file is a build bug CI catches, not a runtime
            // state. Scoped allow, proof: compile-time data.
            #[allow(clippy::expect_used)]
            let provider: Provider =
                serde_json::from_str(json).expect("embedded provider data is valid");
            // Fail at startup, not on the wire: a typo'd dialect or OAuth
            // flow is a data bug and must not fall back silently.
            provider.api();
            assert!(
                provider.responses_mount == ResponsesMount::Platform
                    || provider.api() == Api::Responses,
                "provider {}: codex responses_mount requires a Responses dialect",
                provider.name
            );
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
