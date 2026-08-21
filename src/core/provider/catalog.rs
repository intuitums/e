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
    pub efforts: Vec<String>,
    /// Context window in tokens. A conservative default until per-model
    /// numbers are sourced; overridable via models.json later.
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
        let listed = entry.get("models").and_then(|v| v.as_array()).cloned();
        let legacy = entry.get("ids").and_then(|v| v.as_array()).map(|ids| {
            ids.iter()
                .filter_map(|v| v.as_str())
                .map(|id| serde_json::json!({ "id": id }))
                .collect::<Vec<_>>()
        });
        for item in listed.or(legacy).unwrap_or_default() {
            let Some(id) = item["id"].as_str() else {
                continue;
            };
            if !models.iter().any(|m| m.provider == provider && m.id == id) {
                models.push(Model {
                    provider: provider.clone(),
                    id: id.to_string(),
                    base_url: base.clone(),
                    api: Api::Completions,
                    efforts: Vec::new(),
                    context_window: item["context_window"].as_u64().unwrap_or(200_000),
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
    // Serialize refreshes in-process: launch, sign-in, and picker-open can
    // race, and interleaved read-merge-writes could drop a provider's entry.
    static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = REFRESH_LOCK.lock().await;
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
        if let Some(models) = fetch_models(&provider, &base).await {
            let listed: Vec<serde_json::Value> = models
                .iter()
                .map(|(id, window)| match window {
                    Some(w) => serde_json::json!({ "id": id, "context_window": w }),
                    None => serde_json::json!({ "id": id }),
                })
                .collect();
            let entry = serde_json::json!({ "checked_at": now, "models": listed });
            let _ = crate::core::config::store::update(&store_path(), 0o644, |obj| {
                obj.insert(provider.clone(), entry);
            });
        }
    }
}

/// Ids that are plainly not chat models — keep the picker for models a
/// coding agent can actually talk to.
fn looks_like_chat_model(id: &str) -> bool {
    const NOISE: &[&str] = &[
        "embed",
        "whisper",
        "tts",
        "audio",
        "image",
        "dall-e",
        "moderation",
        "rerank",
    ];
    let lower = id.to_lowercase();
    !NOISE.iter().any(|n| lower.contains(n))
}

/// "model-20251001" / "model-2024-05-13" is a dated alias; drop it when the
/// undated base is also in the list.
fn dated_alias_of(id: &str) -> Option<&str> {
    let (base, suffix) = id.rsplit_once('-')?;
    if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
        return Some(base);
    }
    // -YYYY-MM-DD
    if suffix.len() == 2 {
        if let Some((b2, mid)) = base.rsplit_once('-') {
            if mid.len() == 2 {
                if let Some((b3, year)) = b2.rsplit_once('-') {
                    if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) {
                        return Some(b3);
                    }
                }
            }
        }
    }
    None
}

/// `GET {base}/models` with the provider's credential — (id, window?) per
/// listed model; None on any failure.
async fn fetch_models(provider: &str, base: &str) -> Option<Vec<(String, Option<u64>)>> {
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
    let entries = body["data"].as_array()?;
    let all_ids: Vec<&str> = entries.iter().filter_map(|m| m["id"].as_str()).collect();
    let mut out = Vec::new();
    for entry in entries {
        let Some(id) = entry["id"].as_str() else {
            continue;
        };
        if !looks_like_chat_model(id) {
            continue;
        }
        if let Some(base_id) = dated_alias_of(id) {
            if all_ids.contains(&base_id) {
                continue;
            }
        }
        // Some gateways report the window; keep it when they do.
        let window = entry["context_length"]
            .as_u64()
            .or(entry["context_window"].as_u64())
            .or(entry["max_context_length"].as_u64());
        out.push((id.to_string(), window));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
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
                let base = entry.base_url.clone().unwrap_or_else(|| {
                    crate::core::provider::registry::find("opencode-go")
                        .map(|p| p.base_url.clone())
                        .unwrap_or_default()
                });
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
                        efforts: Vec::new(),
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
pub fn display_name(provider: &str) -> String {
    crate::core::provider::registry::find(provider)
        .map(|p| p.display.clone())
        .unwrap_or_else(|| provider.to_string())
}
