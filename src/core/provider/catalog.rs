//! The model catalog: a built-in table of the providers e speaks natively,
//! overridable by `~/.e/models.json` (same shape, merged over the built-ins).
//! The active model comes from `~/.e/settings.json` `{"model": "provider/id"}`
//! or a `/model` switch at runtime.

use serde::Deserialize;

use crate::core::config::home;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Api {
    /// OpenAI chat-completions dialect (`/chat/completions`, SSE deltas).
    Completions,
    /// The responses dialect behind the ChatGPT backend (OAuth + account id).
    Responses,
    /// The Anthropic Messages dialect (`/v1/messages`, x-api-key).
    Anthropic,
}

#[derive(Clone, Debug)]
pub struct Model {
    pub provider: String,
    pub id: String,
    pub base_url: String,
    pub api: Api,
    /// Effort values the backend accepts for its reasoning knob, if any.
    pub efforts: &'static [&'static str],
    /// Context window in tokens. A conservative default until per-model
    /// numbers are sourced; overridable via models.json later.
    pub context_window: u64,
}

pub fn slug(model: &Model) -> String {
    format!("{}/{}", model.provider, model.id)
}

const OPENCODE_GO_BASE: &str = "https://opencode.ai/zen/go/v1";
const OPENCODE_ZEN_BASE: &str = "https://opencode.ai/zen/v1";
const XAI_BASE: &str = "https://api.x.ai/v1";
const OPENAI_BASE: &str = "https://api.openai.com/v1";
const ANTHROPIC_BASE: &str = "https://api.anthropic.com";
const CODEX_BASE: &str = "https://chatgpt.com/backend-api";
const EFFORTS: &[&str] = &["low", "medium", "high"];

pub fn builtin_catalog() -> Vec<Model> {
    let completions = |id: &str| Model {
        provider: "opencode-go".into(),
        id: id.into(),
        base_url: OPENCODE_GO_BASE.into(),
        api: Api::Completions,
        efforts: &[],
        context_window: 200_000,
    };
    let zen = |id: &str, context_window: u64| Model {
        provider: "opencode".into(),
        id: id.into(),
        base_url: OPENCODE_ZEN_BASE.into(),
        api: Api::Completions,
        efforts: &[],
        context_window,
    };
    let xai = |id: &str, context_window: u64| Model {
        provider: "xai".into(),
        id: id.into(),
        base_url: XAI_BASE.into(),
        api: Api::Completions,
        efforts: &[],
        context_window,
    };
    let openai = |id: &str, context_window: u64| Model {
        provider: "openai".into(),
        id: id.into(),
        base_url: OPENAI_BASE.into(),
        api: Api::Responses,
        efforts: EFFORTS,
        context_window,
    };
    let anthropic = |id: &str, context_window: u64| Model {
        provider: "anthropic".into(),
        id: id.into(),
        base_url: ANTHROPIC_BASE.into(),
        api: Api::Anthropic,
        efforts: EFFORTS,
        context_window,
    };
    let codex = |id: &str| Model {
        provider: "openai-codex".into(),
        id: id.into(),
        base_url: CODEX_BASE.into(),
        api: Api::Responses,
        efforts: EFFORTS,
        context_window: 272_000,
    };
    vec![
        completions("deepseek-v4-flash"),
        completions("deepseek-v4-pro"),
        completions("minimax-m3"),
        completions("qwen3.7-plus"),
        completions("qwen3.7-max"),
        completions("glm-5.2"),
        completions("glm-5.3"),
        completions("kimi-k3"),
        completions("hy3"),
        completions("muse-spark-1.2-contributor"),
        codex("gpt-5.6-sol"),
        codex("gpt-5.6-terra"),
        codex("gpt-5.6-luna"),
        codex("gpt-5.5"),
        codex("gpt-5.4"),
        zen("deepseek-v4-flash", 1_000_000),
        zen("kimi-k3", 262_144),
        zen("minimax-m3", 1_000_000),
        zen("glm-5.2", 1_000_000),
        xai("grok-4.6", 500_000),
        xai("grok-4.3", 1_000_000),
        xai("grok-build-0.1", 256_000),
        openai("gpt-5.5", 272_000),
        openai("gpt-5.5-pro", 1_050_000),
        openai("gpt-5.4", 272_000),
        openai("gpt-5.3-codex", 400_000),
        openai("gpt-5.2", 400_000),
        anthropic("claude-fable-5", 1_000_000),
        anthropic("claude-opus-5", 1_000_000),
        anthropic("claude-sonnet-5", 1_000_000),
        anthropic("claude-opus-4-8", 1_000_000),
        anthropic("claude-haiku-4-5", 200_000),
    ]
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
    /// Default window for this provider's models; each model may override.
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    models: Vec<ModelEntry>,
}

/// A model in models.json: a bare id string, or an object when the model
/// needs its own context window.
#[derive(Deserialize)]
#[serde(untagged)]
enum ModelEntry {
    Id(String),
    Detailed {
        id: String,
        #[serde(default)]
        context_window: Option<u64>,
    },
}

/// How long a provider's fetched model list stays fresh (the reference's
/// refresh interval).
pub const REMOTE_REFRESH_MS: u64 = 4 * 60 * 60 * 1000;

fn store_path() -> std::path::PathBuf {
    crate::core::config::home::home().join("models-store.json")
}

/// Model ids each provider reported, from the cache. A new model a gateway
/// ships appears here on the next refresh — no e release involved.
fn remote_overlay(models: &mut Vec<Model>) {
    let object = crate::core::config::store::read_object(&store_path());
    for (provider, entry) in object {
        let Some(base) = models
            .iter()
            .find(|m| m.provider == provider)
            .map(|m| m.base_url.clone())
        else {
            continue; // only providers e knows how to speak to
        };
        let Some(ids) = entry.get("ids").and_then(|v| v.as_array()) else {
            continue;
        };
        for id in ids.iter().filter_map(|v| v.as_str()) {
            if !models.iter().any(|m| m.provider == provider && m.id == id) {
                models.push(Model {
                    provider: provider.clone(),
                    id: id.to_string(),
                    base_url: base.clone(),
                    api: Api::Completions,
                    efforts: &[],
                    context_window: 200_000,
                });
            }
        }
    }
}

/// Refresh the cached model lists from every signed-in provider that serves
/// the standard `GET {base}/models`. Silent on failure — an offline launch
/// must not care. Skips providers refreshed within the freshness window.
pub async fn refresh_remote() {
    refresh_remote_within(REMOTE_REFRESH_MS).await
}

/// Refresh providers whose cache is older than `max_age_ms` — the /models
/// picker calls this with a short window so a gateway's brand-new model
/// appears the moment someone looks for it.
pub async fn refresh_remote_within(max_age_ms: u64) {
    let auth = crate::core::auth::load();
    let now = crate::core::auth::now_ms();
    let stored = crate::core::config::store::read_object(&store_path());
    // One representative model per signed-in provider gives base + auth kind.
    let mut providers: Vec<(String, String)> = Vec::new();
    for m in catalog() {
        if auth.contains_key(&m.provider) && !providers.iter().any(|(p, _)| *p == m.provider) {
            providers.push((m.provider.clone(), m.base_url.clone()));
        }
    }
    for (provider, base) in providers {
        let fresh = stored
            .get(&provider)
            .and_then(|e| e.get("checked_at"))
            .and_then(|v| v.as_u64())
            .map(|at| now.saturating_sub(at) < max_age_ms)
            .unwrap_or(false);
        if fresh {
            continue;
        }
        if let Some(ids) = fetch_model_ids(&provider, &base).await {
            let entry = serde_json::json!({ "checked_at": now, "ids": ids });
            let _ = crate::core::config::store::update(&store_path(), 0o644, |obj| {
                obj.insert(provider.clone(), entry);
            });
        }
    }
}

/// `GET {base}/models` with the provider's credential; None on any failure.
async fn fetch_model_ids(provider: &str, base: &str) -> Option<Vec<String>> {
    let auth = crate::core::auth::load();
    let credential = auth.get(provider)?;
    let key = match credential {
        crate::core::auth::Credential::ApiKey { key } => key.clone(),
        crate::core::auth::Credential::OAuth { access, .. } => access.clone(),
    };
    let mut request = crate::core::provider::http()
        .get(format!("{base}/models"))
        .timeout(std::time::Duration::from_secs(15));
    request = if provider == "anthropic" {
        request
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
    } else {
        request.bearer_auth(&key)
    };
    let body: serde_json::Value = request.send().await.ok()?.json().await.ok()?;
    let ids: Vec<String> = body["data"]
        .as_array()?
        .iter()
        .filter_map(|m| m["id"].as_str().map(String::from))
        .collect();
    if ids.is_empty() {
        None
    } else {
        Some(ids)
    }
}

/// Built-ins plus `~/.e/models.json` — and the file wins on a name clash,
/// the same rule as themes: never override what the user declared.
pub fn catalog() -> Vec<Model> {
    let mut models = builtin_catalog();
    remote_overlay(&mut models);
    if let Ok(json) = std::fs::read_to_string(home::home().join("models.json")) {
        if let Ok(file) = serde_json::from_str::<ModelsFile>(&json) {
            for (provider, entry) in file.providers {
                let api = match entry.api.as_deref() {
                    Some("codex-responses") | Some("responses") | Some("openai-responses") => {
                        Api::Responses
                    }
                    Some("anthropic") | Some("anthropic-messages") => Api::Anthropic,
                    _ => Api::Completions,
                };
                let base = entry
                    .base_url
                    .clone()
                    .unwrap_or_else(|| OPENCODE_GO_BASE.into());
                for model in entry.models {
                    let (id, window) = match model {
                        ModelEntry::Id(id) => (id, None),
                        ModelEntry::Detailed { id, context_window } => (id, context_window),
                    };
                    let resolved = Model {
                        provider: provider.clone(),
                        id,
                        base_url: base.clone(),
                        api,
                        efforts: &[],
                        context_window: window.or(entry.context_window).unwrap_or(200_000),
                    };
                    models.retain(|m| !(m.provider == resolved.provider && m.id == resolved.id));
                    models.push(resolved);
                }
            }
        }
    }
    // The overlay runs last so it can attach to user-declared providers
    // too; it only adds ids nothing else claimed, so built-ins and the
    // user's file always win a clash.
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
pub fn display_name(provider: &str) -> &str {
    match provider {
        "openai-codex" => "OpenAI Codex",
        "openai" => "OpenAI",
        "anthropic" => "Anthropic",
        "opencode-go" => "OpenCode Go",
        "opencode" => "OpenCode Zen",
        "xai" => "xAI",
        other => other,
    }
}
