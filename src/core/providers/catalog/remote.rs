//! The live half of the catalog: every signed-in provider's own
//! `GET /models` is fetched in the background, cached in
//! `~/.e/models-store.json`, and merged with the declared tables. New ids
//! appear with no e release, and provider-reported context windows replace
//! built-in seeds but not explicit user overrides. Failures stay silent so an
//! offline launch does not care.

use super::{catalog, Model, Thinking};

/// How long a provider's fetched model list stays fresh (the reference's
/// refresh interval).
pub const REMOTE_REFRESH_MS: u64 = 4 * 60 * 60 * 1000;

fn store_path() -> std::path::PathBuf {
    crate::core::config::home::home().join("models-store.json")
}

/// Model ids each provider reported, from the cache. A new model a gateway
/// ships appears here on the next refresh — no e release involved.
pub(super) fn remote_overlay(
    models: &mut Vec<Model>,
    context_overrides: &std::collections::HashSet<(String, String)>,
) {
    let object = crate::core::config::store::read_object(&store_path()).unwrap_or_default();
    for (provider, entry) in object {
        // Transport from an existing model of the provider (models.json
        // overrides included), else from the registry — a keyless local's
        // whole catalog is this overlay, so it has no model to copy from.
        let Some((base, api, catalog_strategy, responses_mount, supports_tools, image_input)) =
            models
                .iter()
                .find(|m| m.provider == provider)
                .map(|m| {
                    (
                        m.base_url.clone(),
                        m.api,
                        m.catalog,
                        m.responses_mount,
                        m.provider_supports_tools,
                        m.provider_image_input,
                    )
                })
                .or_else(|| {
                    crate::core::providers::registry::find(&provider).map(|p| {
                        (
                            p.base_url.clone(),
                            p.api(),
                            p.catalog,
                            p.responses_mount,
                            p.supports_tools,
                            p.image_input,
                        )
                    })
                })
        else {
            continue; // only providers e knows how to speak to
        };
        if catalog_strategy == crate::core::providers::registry::CatalogStrategy::None {
            // The provider's live discovery is off. A cache entry can
            // outlive that setting (written before it changed, or left
            // over from a prior config) — never resurrect it into the
            // catalog, or `catalog: "none"` would not actually disable
            // discovered models.
            continue;
        }
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
            // A gateway report corrects a built-in seed. An explicit user
            // value remains final because it may describe a deployment limit
            // the provider's generic catalog cannot express.
            let user_overrode_window =
                context_overrides.contains(&(provider.clone(), id.to_string()));
            match models
                .iter_mut()
                .find(|m| m.provider == provider && m.id == id)
            {
                Some(existing) => {
                    if !user_overrode_window {
                        if let Some(w) = item["context_window"].as_u64() {
                            existing.context_window = w;
                        }
                    }
                }
                None => models.push(Model {
                    provider: provider.clone(),
                    id: id.to_string(),
                    base_url: base.clone(),
                    api,
                    catalog: catalog_strategy,
                    responses_mount,
                    provider_supports_tools: supports_tools,
                    provider_image_input: image_input,
                    effort: Vec::new(),
                    thinking: Thinking::Manual,
                    context_window: item["context_window"].as_u64().unwrap_or(200_000),
                    max_output: None,
                    // The endpoint reports only id/window, so inherit the
                    // deployment-wide defaults retained independently from
                    // any declared sibling model's override.
                    supports_tools,
                    image_input,
                    pricing: None,
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
    // One representative model per signed-in provider gives base + auth
    // kind; catalog entries first so models.json base_url overrides win.
    // Registry providers follow so a keyless local with an empty seed list
    // (its models come only from this refresh) still gets polled.
    let mut providers: Vec<(
        String,
        String,
        super::Api,
        crate::core::providers::registry::CatalogStrategy,
        crate::core::providers::registry::ResponsesMount,
    )> = Vec::new();
    for m in catalog() {
        if crate::core::auth::signed_in(&auth, &m.provider)
            && !providers.iter().any(|(p, _, _, _, _)| *p == m.provider)
        {
            providers.push((
                m.provider.clone(),
                m.base_url.clone(),
                m.api,
                m.catalog,
                m.responses_mount,
            ));
        }
    }
    for p in crate::core::providers::registry::all() {
        if crate::core::auth::signed_in(&auth, &p.name)
            && !providers.iter().any(|(name, _, _, _, _)| *name == p.name)
        {
            providers.push((
                p.name.clone(),
                p.base_url.clone(),
                p.api(),
                p.catalog,
                p.responses_mount,
            ));
        }
    }
    for (provider, base, api, catalog_strategy, responses_mount) in providers {
        if catalog_strategy == crate::core::providers::registry::CatalogStrategy::None {
            continue;
        }
        let fresh = stored
            .get(&provider)
            .and_then(|e| e.get("checked_at"))
            .and_then(|v| v.as_u64())
            .map(|at| now.saturating_sub(at) < max_age_ms)
            .unwrap_or(false);
        if fresh {
            continue;
        }
        if let Some(models) =
            fetch_models(&provider, &base, api, catalog_strategy, responses_mount).await
        {
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
/// listed model; None on any failure. Google lists at the same path but with
/// its own auth header and payload shape (`models[].name`, not `data[].id`).
async fn fetch_models(
    provider: &str,
    base: &str,
    api: super::Api,
    catalog_strategy: crate::core::providers::registry::CatalogStrategy,
    responses_mount: crate::core::providers::registry::ResponsesMount,
) -> Option<Vec<(String, Option<u64>)>> {
    let authorization =
        crate::core::providers::runtime::authorize_provider(provider, api, responses_mount)
            .await
            .ok()?;
    // Anthropic declares the bare host as its base (the dialect appends
    // /v1 for /v1/messages); the list endpoint lives under /v1 too, so
    // fetching `{base}/models` would 404 silently on every refresh.
    let url = if catalog_strategy == crate::core::providers::registry::CatalogStrategy::Anthropic {
        format!("{base}/v1/models")
    } else {
        format!("{base}/models")
    };
    let mut request = crate::core::providers::http()
        .ok()?
        .get(url)
        .timeout(std::time::Duration::from_secs(15));
    request = match (authorization.credentialed, catalog_strategy) {
        (false, _) => request,
        (true, crate::core::providers::registry::CatalogStrategy::Anthropic) => request
            .header("x-api-key", &authorization.bearer)
            .header("anthropic-version", "2023-06-01"),
        (true, crate::core::providers::registry::CatalogStrategy::Google) => {
            request.header("x-goog-api-key", &authorization.bearer)
        }
        (true, _) => {
            let request = request.bearer_auth(&authorization.bearer);
            match authorization.account_id {
                Some(account) => request.header("chatgpt-account-id", account),
                None => request,
            }
        }
    };
    let body: serde_json::Value = request.send().await.ok()?.json().await.ok()?;
    let google = catalog_strategy == crate::core::providers::registry::CatalogStrategy::Google;
    let entries = if google {
        body["models"].as_array()
    } else {
        body["data"].as_array()
    }?;
    // The wire id: Gemini reports `models/gemini-…` and wants the bare id back.
    let id_of = |entry: &serde_json::Value| -> Option<String> {
        if google {
            entry["name"]
                .as_str()
                .map(|n| n.strip_prefix("models/").unwrap_or(n).to_string())
        } else {
            entry["id"].as_str().map(String::from)
        }
    };
    let all_ids: Vec<String> = entries.iter().filter_map(id_of).collect();
    let mut out = Vec::new();
    for entry in entries {
        let Some(id) = id_of(entry) else {
            continue;
        };
        // Providers that report a type or capability list embeddings, images,
        // video, and speech beside chat models. Keep the picker for language
        // models: Gemini says so via supportedGenerationMethods, OpenAI-style
        // gateways via a `type` field, falling back to the id heuristic when
        // the provider doesn't say.
        if google {
            let serves_chat = entry["supportedGenerationMethods"]
                .as_array()
                .is_some_and(|ms| ms.iter().any(|m| m.as_str() == Some("generateContent")));
            if !serves_chat {
                continue;
            }
        } else if let Some(kind) = entry["type"].as_str() {
            if kind != "language" {
                continue;
            }
        }
        if !looks_like_chat_model(&id) {
            continue;
        }
        if let Some(base_id) = dated_alias_of(&id) {
            if all_ids.iter().any(|a| a == base_id) {
                continue;
            }
        }
        // Some gateways report the window; keep it when they do. Gemini's
        // inputTokenLimit is its context window as far as the picker cares.
        let window = entry["context_length"]
            .as_u64()
            .or(entry["context_window"].as_u64())
            .or(entry["max_context_length"].as_u64())
            .or(entry["inputTokenLimit"].as_u64());
        out.push((id, window));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::providers::catalog::{Api, Model};
    use crate::core::providers::registry::{CatalogStrategy, ResponsesMount};

    // E_HOME is process-global; serialize tests that set it.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn seeded_model(provider: &str, catalog: CatalogStrategy) -> Model {
        Model {
            provider: provider.into(),
            id: "seed".into(),
            base_url: "https://example.invalid".into(),
            api: Api::Completions,
            catalog,
            responses_mount: ResponsesMount::Platform,
            provider_supports_tools: true,
            provider_image_input: false,
            effort: Vec::new(),
            thinking: Thinking::Manual,
            context_window: 200_000,
            max_output: None,
            supports_tools: true,
            image_input: false,
            pricing: None,
        }
    }

    fn with_temp_home(name: &str, body: impl FnOnce(&std::path::Path)) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "e-remote-overlay-{name}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("E_HOME", &dir);
        body(&dir);
        std::env::remove_var("E_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn catalog_none_ignores_a_cached_model_the_overlay_would_otherwise_merge() {
        with_temp_home("none", |dir| {
            std::fs::write(
                dir.join("models-store.json"),
                r#"{"acme":{"models":[{"id":"stale-model","context_window":128000}]}}"#,
            )
            .unwrap();

            let mut models = vec![seeded_model("acme", CatalogStrategy::None)];
            remote_overlay(&mut models, &Default::default());
            assert_eq!(
                models.len(),
                1,
                "catalog: none must not resurrect a cached model"
            );
        });
    }

    #[test]
    fn a_normal_catalog_strategy_still_merges_cached_models() {
        with_temp_home("openai", |dir| {
            std::fs::write(
                dir.join("models-store.json"),
                r#"{"acme":{"models":[{"id":"discovered-model","context_window":128000}]}}"#,
            )
            .unwrap();

            let mut models = vec![seeded_model("acme", CatalogStrategy::Openai)];
            remote_overlay(&mut models, &Default::default());
            assert!(
                models.iter().any(|m| m.id == "discovered-model"),
                "a provider without catalog: none should still pick up cached models"
            );
        });
    }
}
