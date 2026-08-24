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
}

impl Api {
    /// Parse a dialect name from provider JSON / models.json. Unknown strings
    /// are `None` — callers decide whether to panic (built-ins) or fall back
    /// (user file).
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "openai-completions" | "completions" => Some(Self::Completions),
            "codex-responses" | "openai-responses" | "responses" => Some(Self::Responses),
            "anthropic-messages" | "anthropic" => Some(Self::Anthropic),
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
    fn from_decl(value: Option<&str>) -> Thinking {
        match value {
            Some("adaptive") => Thinking::Adaptive,
            // Undeclared keeps today's wire shape; only data opts into
            // adaptive, so user-declared models never change behavior.
            _ => Thinking::Manual,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Model {
    pub provider: String,
    pub id: String,
    pub base_url: String,
    pub api: Api,
    /// Effort values the backend accepts for its reasoning knob, if any.
    pub efforts: Vec<String>,
    /// Which thinking wire shape the backend accepts for this model.
    pub thinking: Thinking,
    /// Context window in tokens. The seed value is a fallback: the
    /// provider's own reported window wins once a refresh has seen it.
    pub context_window: u64,
}

pub fn slug(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

pub fn builtin_catalog() -> Vec<Model> {
    // Providers are data (provider/providers/*.json); this just projects the
    // registry into models.
    crate::core::provider::registry::all()
        .iter()
        .flat_map(|provider| {
            provider.models.iter().map(|decl| Model {
                provider: provider.name.clone(),
                id: decl.id.clone(),
                base_url: provider.base_url.clone(),
                api: provider.api(),
                efforts: decl.efforts.clone(),
                thinking: Thinking::from_decl(decl.thinking.as_deref()),
                context_window: decl.context_window,
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
    efforts: Option<Vec<String>>,
    /// Default window for this provider's models; each model may override.
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    thinking: Option<String>,
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
    file.providers
        .into_iter()
        .filter_map(|(provider, entry)| {
            (entry.base_url.is_none() && crate::core::provider::registry::find(&provider).is_none())
                .then(|| format!("models.json: provider {provider} requires an explicit base_url"))
        })
        .collect()
}
/// Built-ins plus `~/.e/models.json` — and the file wins on a name clash,
/// the same rule as themes: never override what the user declared.
pub fn catalog() -> Vec<Model> {
    let mut models = builtin_catalog();
    remote_overlay(&mut models);
    if let Ok(json) = std::fs::read_to_string(home::home().join("models.json")) {
        if let Ok(file) = serde_json::from_str::<ModelsFile>(&json) {
            for (provider, entry) in file.providers {
                // A partial entry — "correct this one field" — must inherit
                // the built-in provider's transport and defaults rather than
                // silently swapping dialect and endpoint. Otherwise tweaking
                // a context window on an Anthropic model would send that
                // model's requests (and its credential) to an unrelated
                // gateway's Chat Completions endpoint.
                let builtin = crate::core::provider::registry::find(&provider);
                let api = match entry.api.as_deref() {
                    Some(name) => Api::parse(name).unwrap_or_else(|| {
                        panic!("models.json: provider {provider}: unknown api dialect `{name}`")
                    }),
                    None => builtin.map(|p| p.api()).unwrap_or(Api::Completions),
                };
                let Some(base) = entry
                    .base_url
                    .clone()
                    .or_else(|| builtin.map(|p| p.base_url.clone()))
                else {
                    continue;
                };
                for model in entry.models {
                    let (id, window, efforts, thinking) = match model {
                        ModelEntry::Id(id) => (id, None, Vec::new(), None),
                        ModelEntry::Detailed {
                            id,
                            context_window,
                            efforts,
                            thinking,
                        } => (id, context_window, efforts, thinking),
                    };
                    let declared = builtin.and_then(|p| p.models.iter().find(|decl| decl.id == id));
                    let resolved = Model {
                        provider: provider.clone(),
                        id,
                        base_url: base.clone(),
                        api,
                        efforts: match (&entry.efforts, &efforts) {
                            // Per-model declaration wins…
                            (None, e) if !e.is_empty() => e.clone(),
                            // …then the per-provider default from the file…
                            (Some(e), _) if !e.is_empty() => e.clone(),
                            // …then the built-in's own efforts.
                            _ => declared.map(|d| d.efforts.clone()).unwrap_or_default(),
                        },
                        thinking: match (&thinking, &entry.thinking) {
                            (Some(t), _) | (_, Some(t)) => match t.as_str() {
                                "adaptive" => Thinking::Adaptive,
                                "manual" => Thinking::Manual,
                                other => panic!(
                                    "models.json: provider {provider}: unknown thinking mode `{other}`"
                                ),
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
        .filter(|m| auth.contains_key(&m.provider))
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
pub fn clear_scope() {
    crate::core::config::settings::remove("scoped_models");
}

/// Sort models for a picker: grouped by provider in registry order (unknown
/// providers after, alphabetically), original order within a provider.
pub fn provider_grouped(mut models: Vec<Model>) -> Vec<Model> {
    let registry_pos = |provider: &str| {
        crate::core::provider::registry::all()
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

pub fn set_scope(ids: &[String]) {
    crate::core::config::settings::set_strings("scoped_models", ids);
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
    crate::core::provider::registry::find(provider)
        .map(|p| p.display.clone())
        .unwrap_or_else(|| provider.to_string())
}
