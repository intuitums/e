//! The live half of the catalog: every signed-in provider's own
//! `GET /models` is fetched in the background, cached in
//! `~/.e/models-store.json`, and merged over the declared tables — new ids
//! appear with no e release, and the provider's reported context window
//! wins over any seed value. Silent on failure: an offline launch must not
//! care.

use super::{catalog, Model, Thinking};

/// How long a provider's fetched model list stays fresh (the reference's
/// refresh interval).
pub const REMOTE_REFRESH_MS: u64 = 4 * 60 * 60 * 1000;

fn store_path() -> std::path::PathBuf {
    crate::core::config::home::home().join("models-store.json")
}

/// Model ids each provider reported, from the cache. A new model a gateway
/// ships appears here on the next refresh — no e release involved.
pub(super) fn remote_overlay(models: &mut Vec<Model>) {
    let object = crate::core::config::store::read_object(&store_path()).unwrap_or_default();
    for (provider, entry) in object {
        let Some((base, api)) = models
            .iter()
            .find(|m| m.provider == provider)
            .map(|m| (m.base_url.clone(), m.api))
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
            // The gateway's own report wins for the window — the model
            // chooses its context window, not our tables. Everything else
            // about a claimed id stays as declared.
            match models
                .iter_mut()
                .find(|m| m.provider == provider && m.id == id)
            {
                Some(existing) => {
                    if let Some(w) = item["context_window"].as_u64() {
                        existing.context_window = w;
                    }
                }
                None => models.push(Model {
                    provider: provider.clone(),
                    id: id.to_string(),
                    base_url: base.clone(),
                    api,
                    efforts: Vec::new(),
                    thinking: Thinking::Manual,
                    context_window: item["context_window"].as_u64().unwrap_or(200_000),
                }),
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
    let stored = crate::core::config::store::read_object(&store_path()).unwrap_or_default();
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
        // Providers that report a type list embeddings, images, video, and
        // speech beside chat models. Keep the picker for language models;
        // fall back to the id heuristic when the provider doesn't say.
        if let Some(kind) = entry["type"].as_str() {
            if kind != "language" {
                continue;
            }
        }
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
