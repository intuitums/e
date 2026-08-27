//! The model catalog: which models exist, what they speak, and resolving a
//! pick to a Model. Built-ins come from the provider registry (data),
//! `~/.e/models.json` overrides them, and the live remote sync
//! (`remote.rs`) adds ids and refreshes context windows. The active model
//! comes from `~/.e/settings.json` `{"model": "provider/id"}` or a `/model`
//! switch at runtime.

use serde::Deserialize;

use crate::core::config::home;

mod remote;
use remote::remote_overlay;
pub use remote::{refresh_remote, refresh_remote_within, REMOTE_REFRESH_MS};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Api {
    /// OpenAI chat-completions dialect (`/chat/completions`, SSE deltas).
    Completions,
    /// The responses dialect behind the ChatGPT backend (OAuth + account id).
    Responses,
    /// The Anthropic Messages dialect (`/v1/messages`, x-api-key).
    Anthropic,
    /// The Gemini dialect (`:streamGenerateContent?alt=sse`, x-goog-api-key).
    Google,
}

impl Api {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completions => "openai-completions",
            Self::Responses => "openai-responses",
            Self::Anthropic => "anthropic-messages",
            Self::Google => "google-generative-ai",
        }
    }

    /// Parse a dialect name from provider JSON / models.json. Unknown strings
    /// are `None` — callers decide whether to panic (built-ins) or fall back
    /// (user file).
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "openai-completions" | "completions" => Some(Self::Completions),
            "codex-responses" | "openai-responses" | "responses" => Some(Self::Responses),
            "anthropic-messages" | "anthropic" => Some(Self::Anthropic),
            "google-generative-ai" | "google" => Some(Self::Google),
            _ => None,
        }
    }
}

/// How the model takes its reasoning knob. Adaptive models (Claude 4.7+)
/// reject the legacy manual-thinking shape with a 400, so this is declared
/// per model in provider data and rides the request through to the
/// Anthropic dialect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Thinking {
    /// `thinking: {"type": "adaptive"}` plus `output_config.effort`.
    Adaptive,
    /// Legacy `thinking: {"type": "enabled", "budget_tokens": N}`.
    Manual,
}

impl Thinking {
    fn parse(value: &str) -> Option<Thinking> {
        match value {
            "adaptive" => Some(Thinking::Adaptive),
            "manual" => Some(Thinking::Manual),
            _ => None,
        }
    }

    fn from_decl(value: Option<&str>) -> Thinking {
        match value {
            Some("adaptive") => Thinking::Adaptive,
            // Undeclared keeps today's wire shape; only data opts into
            // adaptive, so user-declared models never change behavior.
            _ => Thinking::Manual,
        }
    }
}

/// Optional USD rates per million tokens. Pricing changes independently of
/// protocol support, so built-ins may omit it; users and gateways can declare
/// authoritative rates in models.json without waiting for an e release.
#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq)]
pub struct Pricing {
    pub input_per_million: f64,
    pub output_per_million: f64,
    #[serde(default)]
    pub cache_read_per_million: Option<f64>,
}

impl Pricing {
    pub fn estimate(&self, input: u64, output: u64, cache_read: u64) -> f64 {
        let cached = cache_read.min(input);
        let uncached = input.saturating_sub(cached);
        let cached_rate = self
            .cache_read_per_million
            .unwrap_or(self.input_per_million);
        (uncached as f64 * self.input_per_million
            + cached as f64 * cached_rate
            + output as f64 * self.output_per_million)
            / 1_000_000.0
    }
}

#[derive(Clone, Debug)]
pub struct Model {
    pub provider: String,
    pub id: String,
    pub base_url: String,
    pub api: Api,
    pub catalog: crate::core::providers::registry::CatalogStrategy,
    pub responses_mount: crate::core::providers::registry::ResponsesMount,
    /// Provider-wide defaults retained separately from per-model overrides,
    /// so a remotely discovered id never inherits an arbitrary sibling's
    /// narrower compatibility declaration.
    pub provider_supports_tools: bool,
    pub provider_image_input: bool,
    /// Effort values the backend accepts for its reasoning knob, if any.
    pub efforts: Vec<String>,
    /// Which thinking wire shape the backend accepts for this model.
    pub thinking: Thinking,
    /// Context window in tokens. The seed value is a fallback: the
    /// provider's own reported window wins once a refresh has seen it.
    pub context_window: u64,
    /// Output ceiling in tokens, when the model's own limit is below the
    /// dialect's default. `None` leaves the dialect's own constant in force.
    pub max_output: Option<u64>,
    /// Whether tool schemas/calls are supported for this model deployment.
    pub supports_tools: bool,
    /// Whether this model accepts image input (the frontend also needs an
    /// image-capable message path before it advertises an attachment).
    pub image_input: bool,
    pub pricing: Option<Pricing>,
}

pub fn slug(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

pub fn builtin_catalog() -> Vec<Model> {
    // Providers are data (providers/data/*.json); this just projects the
    // registry into models.
    crate::core::providers::registry::all()
        .iter()
        .flat_map(|provider| {
            provider.models.iter().map(|decl| Model {
                provider: provider.name.clone(),
                id: decl.id.clone(),
                base_url: provider.base_url.clone(),
                api: provider.api(),
                catalog: provider.catalog,
                responses_mount: provider.responses_mount,
                provider_supports_tools: provider.supports_tools,
                provider_image_input: provider.image_input,
                efforts: decl.efforts.clone(),
                thinking: Thinking::from_decl(decl.thinking.as_deref()),
                context_window: decl.context_window,
                max_output: decl.max_output,
                supports_tools: decl.supports_tools && provider.supports_tools,
                image_input: decl.image_input || provider.image_input,
                pricing: decl.pricing.clone(),
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct ModelsFile {
    providers: std::collections::BTreeMap<String, ProviderEntry>,
}

#[derive(Deserialize)]
struct ProviderEntry {
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    catalog: Option<crate::core::providers::registry::CatalogStrategy>,
    #[serde(default)]
    responses_mount: Option<crate::core::providers::registry::ResponsesMount>,
    #[serde(default)]
    efforts: Option<Vec<String>>,
    /// Default window for this provider's models; each model may override.
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    thinking: Option<String>,
    /// Default output ceiling for this provider's models; each model may
    /// override.
    #[serde(default)]
    max_output: Option<u64>,
    #[serde(default)]
    supports_tools: Option<bool>,
    #[serde(default)]
    image_input: Option<bool>,
    #[serde(default)]
    pricing: Option<Pricing>,
    #[serde(default)]
    models: Vec<ModelEntry>,
}

/// A model in models.json: a bare id string, or an object when the model
/// needs its own context window or effort levels.
#[derive(Deserialize)]
#[serde(untagged)]
enum ModelEntry {
    Id(String),
    Detailed {
        id: String,
        #[serde(default)]
        context_window: Option<u64>,
        #[serde(default)]
        efforts: Vec<String>,
        #[serde(default)]
        thinking: Option<String>,
        #[serde(default)]
        max_output: Option<u64>,
        #[serde(default)]
        supports_tools: Option<bool>,
        #[serde(default)]
        image_input: Option<bool>,
        #[serde(default)]
        pricing: Option<Pricing>,
    },
}

/// Configuration problems that caused user-declared providers to be omitted.
/// Callers surface these in their own UI; the catalog itself stays data-only.
pub fn config_warnings() -> Vec<String> {
    let path = home::home().join("models.json");
    let json = match std::fs::read_to_string(path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => return vec![format!("models.json: cannot read configuration: {error}")],
    };
    let file = match serde_json::from_str::<ModelsFile>(&json) {
        Ok(file) => file,
        Err(error) => return vec![format!("models.json: invalid configuration: {error}")],
    };
    let mut warnings = Vec::new();
    for (provider, entry) in file.providers {
        if entry.base_url.is_none() && crate::core::providers::registry::find(&provider).is_none() {
            warnings.push(format!(
                "models.json: provider {provider} requires an explicit base_url"
            ));
        }
        if let Some(api) = entry.api.as_deref().filter(|api| Api::parse(api).is_none()) {
            warnings.push(format!(
                "models.json: provider {provider}: unknown api dialect `{api}`"
            ));
        }
        if let Some(thinking) = entry
            .thinking
            .as_deref()
            .filter(|thinking| Thinking::parse(thinking).is_none())
        {
            warnings.push(format!(
                "models.json: provider {provider}: unknown thinking mode `{thinking}`"
            ));
        }
        for model in entry.models {
            if let ModelEntry::Detailed {
                id,
                thinking: Some(thinking),
                ..
            } = model
            {
                if Thinking::parse(&thinking).is_none() {
                    warnings.push(format!("models.json: provider {provider}, model {id}: unknown thinking mode `{thinking}`"));
                }
            }
        }
    }
    warnings
}
/// Built-ins plus `~/.e/models.json` — and the file wins on a name clash,
/// the same rule as themes: never override what the user declared.
pub fn catalog() -> Vec<Model> {
    let mut models = builtin_catalog();
    if let Ok(json) = std::fs::read_to_string(home::home().join("models.json")) {
        if let Ok(file) = serde_json::from_str::<ModelsFile>(&json) {
            for (provider, entry) in file.providers {
                // A partial entry — "correct this one field" — must inherit
                // the built-in provider's transport and defaults rather than
                // silently swapping dialect and endpoint. Otherwise tweaking
                // a context window on an Anthropic model would send that
                // model's requests (and its credential) to an unrelated
                // gateway's Chat Completions endpoint.
                let builtin = crate::core::providers::registry::find(&provider);
                let api = match entry.api.as_deref() {
                    Some(name) => match Api::parse(name) {
                        Some(api) => api,
                        // Keep the built-in provider intact; an invalid user
                        // override is a configuration warning, never a
                        // process-wide panic.
                        None => continue,
                    },
                    None => builtin.map(|p| p.api()).unwrap_or(Api::Completions),
                };
                let catalog_strategy = entry
                    .catalog
                    .or_else(|| builtin.map(|provider| provider.catalog))
                    .unwrap_or_default();
                let responses_mount = entry
                    .responses_mount
                    .or_else(|| builtin.map(|provider| provider.responses_mount))
                    .unwrap_or_default();
                let provider_supports_tools = entry
                    .supports_tools
                    .or_else(|| builtin.map(|provider| provider.supports_tools))
                    .unwrap_or(true);
                let provider_image_input = entry
                    .image_input
                    .or_else(|| builtin.map(|provider| provider.image_input))
                    .unwrap_or(false);
                let Some(base) = entry
                    .base_url
                    .clone()
                    .or_else(|| builtin.map(|p| p.base_url.clone()))
                else {
                    continue;
                };
                // Provider fields describe one deployment and apply to its
                // built-in seed models too. Per-model declarations below can
                // still narrow capabilities without changing what newly
                // discovered sibling ids inherit.
                for existing in models.iter_mut().filter(|m| m.provider == provider) {
                    existing.base_url = base.clone();
                    existing.api = api;
                    existing.catalog = catalog_strategy;
                    existing.responses_mount = responses_mount;
                    existing.provider_supports_tools = provider_supports_tools;
                    existing.provider_image_input = provider_image_input;
                    if let Some(supports_tools) = entry.supports_tools {
                        existing.supports_tools = supports_tools;
                    }
                    if let Some(image_input) = entry.image_input {
                        existing.image_input = image_input;
                    }
                }
                for model in entry.models {
                    let (
                        id,
                        window,
                        efforts,
                        thinking,
                        max_output,
                        supports_tools,
                        image_input,
                        pricing,
                    ) = match model {
                        ModelEntry::Id(id) => (id, None, Vec::new(), None, None, None, None, None),
                        ModelEntry::Detailed {
                            id,
                            context_window,
                            efforts,
                            thinking,
                            max_output,
                            supports_tools,
                            image_input,
                            pricing,
                        } => (
                            id,
                            context_window,
                            efforts,
                            thinking,
                            max_output,
                            supports_tools,
                            image_input,
                            pricing,
                        ),
                    };
                    let declared = builtin.and_then(|p| p.models.iter().find(|decl| decl.id == id));
                    let resolved = Model {
                        provider: provider.clone(),
                        id,
                        base_url: base.clone(),
                        api,
                        catalog: catalog_strategy,
                        responses_mount,
                        provider_supports_tools,
                        provider_image_input,
                        efforts: match (&entry.efforts, &efforts) {
                            // Per-model declaration wins…
                            (None, e) if !e.is_empty() => e.clone(),
                            // …then the per-provider default from the file…
                            (Some(e), _) if !e.is_empty() => e.clone(),
                            // …then the built-in's own efforts.
                            _ => declared.map(|d| d.efforts.clone()).unwrap_or_default(),
                        },
                        thinking: match (&thinking, &entry.thinking) {
                            (Some(t), _) | (_, Some(t)) => match Thinking::parse(t) {
                                Some(thinking) => thinking,
                                // A malformed per-model declaration should
                                // not make `doctor` or startup unusable.
                                None => continue,
                            },
                            // …then the built-in's own declaration.
                            _ => declared
                                .map(|d| Thinking::from_decl(d.thinking.as_deref()))
                                .unwrap_or(Thinking::Manual),
                        },
                        context_window: window
                            .or(entry.context_window)
                            .or_else(|| declared.map(|d| d.context_window))
                            .unwrap_or(200_000),
                        max_output: max_output
                            .or(entry.max_output)
                            .or_else(|| declared.and_then(|d| d.max_output)),
                        supports_tools: supports_tools
                            .or(entry.supports_tools)
                            .or_else(|| declared.map(|d| d.supports_tools))
                            .unwrap_or(true),
                        image_input: image_input
                            .or(entry.image_input)
                            .or_else(|| {
                                builtin.map(|provider| {
                                    provider.image_input
                                        || declared.is_some_and(|model| model.image_input)
                                })
                            })
                            .unwrap_or(false),
                        pricing: pricing
                            .or_else(|| entry.pricing.clone())
                            .or_else(|| declared.and_then(|d| d.pricing.clone())),
                    };
                    models.retain(|m| !(m.provider == resolved.provider && m.id == resolved.id));
                    models.push(resolved);
                }
            }
        }
    }
    // The overlay runs last so it can attach to user-declared providers
    // too. It adds ids nothing else claimed, and refreshes context windows
    // from the live report — the one field the model owns. Which models
    // exist, their dialects, and their efforts stay with the built-ins and
    // the user's file.
    remote_overlay(&mut models);
    models
}

#[derive(Deserialize, Default)]
struct Settings {
    #[serde(default)]
    model: Option<String>,
}

pub const DEFAULT_MODEL: &str = "opencode-go/deepseek-v4-flash";

/// The catalog cut to providers with credentials — the models e can
/// actually serve. Everything user-facing (the picker, resolution, the
/// default) works on this set; the full catalog is data, not a menu.
pub fn available() -> Vec<Model> {
    let auth = crate::core::auth::load();
    catalog()
        .into_iter()
        .filter(|m| crate::core::auth::signed_in(&auth, &m.provider))
        .collect()
}

/// The configured model if its provider is signed in; otherwise the first
/// available model; with no credentials at all, the catalog default (the
/// startup warning covers that state).
pub fn default_model() -> Model {
    let wanted = std::fs::read_to_string(home::settings_path())
        .ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
        .and_then(|s| s.model);
    if let Some(wanted) = &wanted {
        if let Some(m) = resolve(wanted) {
            return m;
        }
    }
    available()
        .into_iter()
        .next()
        .or_else(|| resolve_in(&catalog(), DEFAULT_MODEL))
        .expect("builtin default")
}

/// Resolve `provider/id`, a bare id, or a unique substring — among the
/// available models only, so a pick is always usable.
pub fn resolve(query: &str) -> Option<Model> {
    resolve_in(&available(), query)
}

fn resolve_in(models: &[Model], query: &str) -> Option<Model> {
    if let Some(m) = models.iter().find(|m| slug(m) == query || m.id == query) {
        return Some(m.clone());
    }
    let matches: Vec<&Model> = models.iter().filter(|m| slug(m).contains(query)).collect();
    if matches.len() == 1 {
        return Some(matches[0].clone());
    }
    None
}

/// The scoped-model ids ("provider/id"), or None when no scope is set.
pub fn scope() -> Option<Vec<String>> {
    crate::core::config::settings::get_strings("scoped_models")
}

/// Back to no scope at all: ctrl+p cycles everything again.
pub fn clear_scope() -> std::io::Result<()> {
    crate::core::config::settings::remove("scoped_models")
}

/// Sort models for a picker: grouped by provider in registry order (unknown
/// providers after, alphabetically), original order within a provider.
pub fn provider_grouped(mut models: Vec<Model>) -> Vec<Model> {
    let registry_pos = |provider: &str| {
        crate::core::providers::registry::all()
            .iter()
            .position(|p| p.name == provider)
            .unwrap_or(usize::MAX)
    };
    models.sort_by(|a, b| {
        registry_pos(&a.provider)
            .cmp(&registry_pos(&b.provider))
            .then_with(|| a.provider.cmp(&b.provider))
    });
    models
}

pub fn set_scope(ids: &[String]) -> std::io::Result<()> {
    crate::core::config::settings::set_strings("scoped_models", ids)
}

/// The models ctrl+p cycles: the scope filtered to what is signed in, or —
/// with no scope — everything available (the reference behavior).
pub fn cycle_pool() -> Vec<Model> {
    let available = available();
    match scope() {
        Some(ids) if !ids.is_empty() => available
            .into_iter()
            .filter(|m| ids.iter().any(|id| *id == slug(m)))
            .collect(),
        _ => available,
    }
}

/// Human name for a provider, for panels: capitalized, no dashes.
pub fn display_name(provider: &str) -> String {
    crate::core::providers::registry::find(provider)
        .map(|p| p.display.clone())
        .unwrap_or_else(|| provider.to_string())
}

#[cfg(test)]
mod pricing_tests {
    use super::Pricing;

    #[test]
    fn cached_input_uses_its_own_rate_without_double_charging() {
        let pricing = Pricing {
            input_per_million: 2.0,
            output_per_million: 8.0,
            cache_read_per_million: Some(0.5),
        };
        // 750k uncached + 250k cached + 100k output.
        let cost = pricing.estimate(1_000_000, 100_000, 250_000);
        assert!((cost - 2.425).abs() < f64::EPSILON);
    }
}
