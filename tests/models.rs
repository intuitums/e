//! The model catalog. Pins: models.json models carry their declared context
//! window (per-model beats per-provider default beats 200k), and a file entry
//! with a built-in's name replaces the built-in — the file wins.

use std::sync::Mutex;

// E_HOME is process-global; serialize the tests that set it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Registry env keys leak real sign-ins into signed-out assertions when the
/// developer's shell exports them — clear them for this process.
fn clear_env_keys() {
    for provider in e::core::provider::registry::all() {
        if let Some(env) = &provider.auth.key_env {
            std::env::remove_var(env);
        }
    }
}

fn with_models_json(json: &str) -> Vec<e::core::provider::catalog::Model> {
    let dir = std::env::temp_dir().join(format!("e-models-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("models.json"), json).unwrap();
    std::env::set_var("E_HOME", &dir);
    let catalog = e::core::provider::catalog::catalog();
    let _ = std::fs::remove_dir_all(&dir);
    catalog
}

#[test]
fn models_json_windows_are_per_model() {
    let _lock = ENV_LOCK.lock().unwrap();
    let catalog = with_models_json(
        r#"{"providers":{"local":{"base_url":"http://localhost:9999","context_window":64000,
            "models":["small", {"id":"big","context_window":1000000}]}}}"#,
    );
    let find = |id: &str| {
        catalog
            .iter()
            .find(|m| m.provider == "local" && m.id == id)
            .unwrap()
    };
    assert_eq!(
        find("small").context_window,
        64_000,
        "provider default applies"
    );
    assert_eq!(
        find("big").context_window,
        1_000_000,
        "per-model wins over provider default"
    );
}

#[test]
fn models_json_overrides_a_builtin() {
    let _lock = ENV_LOCK.lock().unwrap();
    let catalog = with_models_json(
        r#"{"providers":{"opencode-go":{"models":[{"id":"kimi-k3","context_window":131072}]}}}"#,
    );
    let matches: Vec<_> = catalog
        .iter()
        .filter(|m| m.provider == "opencode-go" && m.id == "kimi-k3")
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "the file entry replaces the built-in, not duplicates it"
    );
    assert_eq!(matches[0].context_window, 131_072);
}

#[test]
fn trust_gates_project_instructions() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("e-trust-{}", std::process::id()));
    let ws = home.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::env::set_var("E_HOME", &home);
    std::fs::write(ws.join("AGENTS.md"), "SECRET-PROJECT-RULE").unwrap();
    let ws = ws.canonicalize().unwrap();

    // Never asked → the project's AGENTS.md stays out of the prompt.
    assert_eq!(e::core::config::trust::status(&ws), None);
    assert!(!e::core::agent::context::system_prompt(&ws).contains("SECRET-PROJECT-RULE"));

    // Declined → still out.
    e::core::config::trust::set(&ws, false).unwrap();
    assert_eq!(e::core::config::trust::status(&ws), Some(false));
    assert!(!e::core::agent::context::system_prompt(&ws).contains("SECRET-PROJECT-RULE"));

    // Trusted → it loads.
    e::core::config::trust::set(&ws, true).unwrap();
    assert!(e::core::agent::context::system_prompt(&ws).contains("SECRET-PROJECT-RULE"));

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn xai_builtins_carry_their_real_windows() {
    let _lock = ENV_LOCK.lock().unwrap();
    std::env::set_var("E_HOME", std::env::temp_dir().join("e-models-none"));
    let catalog = e::core::provider::catalog::catalog();
    let find = |id: &str| {
        catalog
            .iter()
            .find(|m| m.provider == "xai" && m.id == id)
            .unwrap()
    };
    assert_eq!(find("grok-4.3").context_window, 1_000_000);
    assert_eq!(find("grok-4.6").context_window, 500_000);
    assert!(
        !catalog.iter().any(|m| m.id == "grok-build-0.1"),
        "grok-build-0.1 was culled from the built-ins"
    );
    assert_eq!(find("grok-4.6").base_url, "https://api.x.ai/v1");
    assert_eq!(e::core::provider::catalog::display_name("xai"), "xAI");
    assert_eq!(e::core::provider::catalog::display_name("openai"), "OpenAI");
    assert_eq!(
        e::core::provider::catalog::display_name("anthropic"),
        "Anthropic"
    );
    let anthropic = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-fable-5")
        .unwrap();
    assert_eq!(anthropic.context_window, 1_000_000);
    assert!(matches!(
        anthropic.api,
        e::core::provider::catalog::Api::Anthropic
    ));
    let openai = catalog
        .iter()
        .find(|m| m.provider == "openai" && m.id == "gpt-5.5-pro")
        .unwrap();
    assert_eq!(openai.context_window, 1_050_000);
    assert!(matches!(
        openai.api,
        e::core::provider::catalog::Api::Responses
    ));
    assert_eq!(
        e::core::provider::catalog::display_name("opencode-go"),
        "OpenCode Go"
    );
    assert_eq!(
        e::core::provider::catalog::display_name("openai-codex"),
        "OpenAI Codex"
    );
    assert_eq!(
        e::core::provider::catalog::display_name("vercel"),
        "Vercel AI Gateway"
    );
    let nano = catalog
        .iter()
        .find(|m| m.provider == "vercel" && m.id == "openai/gpt-4.1-nano")
        .unwrap();
    assert_eq!(nano.base_url, "https://ai-gateway.vercel.sh/v1");
    assert!(matches!(
        nano.api,
        e::core::provider::catalog::Api::Completions
    ));
    assert_eq!(
        e::core::provider::catalog::slug(nano),
        "vercel/openai/gpt-4.1-nano"
    );
}

#[test]
fn only_signed_in_providers_are_available() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env_keys();
    let dir = std::env::temp_dir().join(format!("e-avail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("E_HOME", &dir);

    // Signed out: nothing is available and nothing resolves.
    assert!(e::core::provider::catalog::available().is_empty());
    assert!(e::core::provider::catalog::resolve("grok-4.6").is_none());

    // Anthropic only: claude models appear, everyone else stays hidden.
    std::fs::write(dir.join("auth.json"), r#"{"anthropic":{"key":"k"}}"#).unwrap();
    let available = e::core::provider::catalog::available();
    assert!(available.iter().all(|m| m.provider == "anthropic"));
    assert!(e::core::provider::catalog::resolve("claude-fable-5").is_some());
    assert!(e::core::provider::catalog::resolve("grok-4.6").is_none());

    // A configured model whose provider is signed out falls back to an
    // available one instead of sticking.
    std::fs::write(
        dir.join("settings.json"),
        r#"{"model":"opencode-go/deepseek-v4-flash"}"#,
    )
    .unwrap();
    assert_eq!(
        e::core::provider::catalog::default_model().provider,
        "anthropic"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cycle_pool_follows_the_scope() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env_keys();
    let dir = std::env::temp_dir().join(format!("e-scope-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("E_HOME", &dir);
    std::fs::write(
        dir.join("auth.json"),
        r#"{"anthropic":{"key":"k"},"xai":{"key":"k"}}"#,
    )
    .unwrap();

    // No scope: the pool is everything available.
    assert_eq!(
        e::core::provider::catalog::cycle_pool().len(),
        e::core::provider::catalog::available().len()
    );

    // A scope narrows the pool — and unavailable entries are ignored.
    e::core::provider::catalog::set_scope(&[
        "anthropic/claude-fable-5".into(),
        "xai/grok-4.6".into(),
        "openai/gpt-5.5".into(), // openai is not signed in
    ]);
    let pool: Vec<String> = e::core::provider::catalog::cycle_pool()
        .iter()
        .map(e::core::provider::catalog::slug)
        .collect();
    assert_eq!(pool, vec!["xai/grok-4.6", "anthropic/claude-fable-5"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn every_docs_topic_has_a_body() {
    for (name, _) in e::core::resources::docs::TOPICS {
        let body = e::core::resources::docs::body(name).unwrap();
        assert!(!body.trim().is_empty(), "empty doc: {name}");
    }
    assert!(e::core::resources::docs::body("extensions")
        .unwrap()
        .contains("initialize"));
    assert!(e::core::resources::docs::body("nope").is_none());
}

#[test]
fn zen_and_go_are_distinct_providers() {
    let _lock = ENV_LOCK.lock().unwrap();
    std::env::set_var("E_HOME", std::env::temp_dir().join("e-models-none2"));
    let catalog = e::core::provider::catalog::catalog();
    let go = catalog
        .iter()
        .find(|m| m.provider == "opencode-go")
        .unwrap();
    let zen = catalog
        .iter()
        .find(|m| m.provider == "opencode-zen")
        .unwrap();
    assert_eq!(go.base_url, "https://opencode.ai/zen/go/v1");
    assert_eq!(zen.base_url, "https://opencode.ai/zen/v1");
    assert_eq!(
        e::core::provider::catalog::display_name("opencode-zen"),
        "OpenCode Zen"
    );
    assert_eq!(
        e::core::provider::catalog::display_name("opencode-go"),
        "OpenCode Go"
    );
}

#[test]
fn legacy_opencode_auth_keys_still_sign_in() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = std::env::temp_dir().join(format!("e-legacy-auth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("E_HOME", &dir);
    for provider in e::core::provider::registry::all() {
        if let Some(env) = &provider.auth.key_env {
            std::env::remove_var(env);
        }
    }

    // An auth.json written before the rename still signs Zen in.
    std::fs::write(dir.join("auth.json"), r#"{"opencode":{"key":"sk-old"}}"#).unwrap();
    let auth = e::core::auth::load();
    assert!(
        matches!(auth.get("opencode-zen"), Some(e::core::auth::Credential::ApiKey { key }) if key == "sk-old"),
        "legacy key not honored"
    );
    assert!(e::core::provider::catalog::available()
        .iter()
        .any(|m| m.provider == "opencode-zen"));
    let _ = std::fs::remove_dir_all(&dir);
}

// The env lock is deliberately held across awaits: E_HOME must stay ours and
// each #[tokio::test] runs on its own runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn provider_reported_models_appear_without_a_release() {
    use std::io::{Read, Write};
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env_keys();
    let dir = std::env::temp_dir().join(format!("e-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("E_HOME", &dir);

    // A mock gateway whose /models lists a model e has never heard of.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = r#"{"data":[
            {"id":"brand-new-model","context_length":64000},
            {"id":"small","context_length":123456},
            {"id":"text-embedding-large"},
            {"id":"brand-new-model-20260101"},
            {"id":"fine-looking-embed","type":"embedding","context_length":8192},
            {"id":"typed-chat","type":"language","context_window":8000}
        ]}"#;
        let _ = a.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        sent
    });

    std::fs::write(dir.join("auth.json"), r#"{"mock":{"key":"sk-live"}}"#).unwrap();
    std::fs::write(
        dir.join("models.json"),
        format!(r#"{{"providers":{{"mock":{{"base_url":"http://127.0.0.1:{port}","api":"anthropic","models":["small"]}}}}}}"#),
    )
    .unwrap();

    e::core::provider::catalog::refresh_remote().await;
    let sent = server.join().unwrap();
    assert!(sent.contains("GET /models"));
    assert!(sent.contains("Bearer sk-live") || sent.contains("bearer sk-live"));

    // The unheard-of model is now in the catalog — and available.
    let catalog = e::core::provider::catalog::catalog();
    let fresh = catalog
        .iter()
        .find(|m| m.provider == "mock" && m.id == "brand-new-model")
        .expect("gateway model appears");
    // The gateway reported the window; the overlay keeps it.
    assert_eq!(fresh.context_window, 64_000);
    // A discovered id speaks the provider's declared wire dialect instead of
    // silently falling back to chat completions.
    assert_eq!(fresh.api, e::core::provider::catalog::Api::Anthropic);
    // And the report wins over what e already lists: "small" was declared
    // without a window (the 200K default) — the gateway's 123456 replaces it.
    let known = catalog
        .iter()
        .find(|m| m.provider == "mock" && m.id == "small")
        .expect("declared model stays listed");
    assert_eq!(known.context_window, 123_456);
    // Non-chat ids, typed non-language models, and dated aliases stay out.
    assert!(!catalog.iter().any(|m| m.id == "text-embedding-large"));
    assert!(!catalog.iter().any(|m| m.id == "brand-new-model-20260101"));
    assert!(
        !catalog.iter().any(|m| m.id == "fine-looking-embed"),
        "a non-language type is dropped even when the id looks like chat"
    );
    let typed = catalog
        .iter()
        .find(|m| m.provider == "mock" && m.id == "typed-chat")
        .expect("language type is kept");
    assert_eq!(typed.context_window, 8_000);
    let available = e::core::provider::catalog::available();
    assert!(available.iter().any(|m| m.id == "brand-new-model"));

    // A fresh cache is not refetched within the window.
    e::core::provider::catalog::refresh_remote().await; // would hang/panic if it re-hit the dead server
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_registry_is_coherent() {
    use e::core::provider::registry;
    let all = registry::all();
    assert!(all.len() >= 6);
    for provider in all {
        assert!(
            !provider.display.is_empty(),
            "{} has no display name",
            provider.name
        );
        assert!(provider.base_url.starts_with("https://"));
        assert!(
            provider.auth.oauth.is_some() || provider.auth.key,
            "{} has no way to sign in",
            provider.name
        );
        if provider.auth.key {
            assert!(
                provider.auth.key_env.is_some(),
                "{} key without env var",
                provider.name
            );
        }
    }
    // The panels' contents, from data: two account flows, six key providers.
    assert_eq!(registry::oauth_providers().len(), 2);
    assert_eq!(registry::key_providers().len(), 6);
    let vercel = registry::find("vercel").expect("vercel is a built-in");
    assert_eq!(vercel.display, "Vercel AI Gateway");
    assert_eq!(vercel.base_url, "https://ai-gateway.vercel.sh/v1");
    assert_eq!(vercel.auth.key_env.as_deref(), Some("AI_GATEWAY_API_KEY"));
    assert!(vercel.auth.oauth.is_none(), "gateway is API-key only");
    assert!(vercel.auth.key);
}

#[test]
fn env_var_keys_sign_a_provider_in() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = std::env::temp_dir().join(format!("e-envkey-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("E_HOME", &dir);
    // No provider may sign in through the ambient environment — clear every
    // key_env the registry declares, or a dev box with OPENAI_API_KEY set
    // fails the emptiness assert below.
    for provider in e::core::provider::registry::all() {
        if let Some(env) = &provider.auth.key_env {
            std::env::remove_var(env);
        }
    }

    assert!(e::core::provider::catalog::available().is_empty());
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-env");
    let available = e::core::provider::catalog::available();
    assert!(available.iter().any(|m| m.provider == "anthropic"));
    // auth.json still wins over the environment.
    std::fs::write(dir.join("auth.json"), r#"{"anthropic":{"key":"sk-file"}}"#).unwrap();
    let auth = e::core::auth::load();
    match auth.get("anthropic").unwrap() {
        e::core::auth::Credential::ApiKey { key } => assert_eq!(key, "sk-file"),
        _ => panic!("wrong credential kind"),
    }
    std::env::remove_var("ANTHROPIC_API_KEY");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vercel_env_key_signs_the_gateway_in() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = std::env::temp_dir().join(format!("e-vercel-env-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("E_HOME", &dir);
    clear_env_keys();

    assert!(e::core::provider::catalog::available().is_empty());
    std::env::set_var("AI_GATEWAY_API_KEY", "vck-test");
    let available = e::core::provider::catalog::available();
    assert!(available.iter().all(|m| m.provider == "vercel"));
    assert!(e::core::provider::catalog::resolve("openai/gpt-4.1-nano").is_some());
    assert!(e::core::provider::catalog::resolve("vercel/openai/gpt-4.1-nano").is_some());
    std::env::remove_var("AI_GATEWAY_API_KEY");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_partial_builtin_override_inherits_transport_not_global_defaults() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env_keys();
    // The issue's reproduction: correct one field of a built-in Anthropic
    // model. Transport must stay Anthropic's — not Chat Completions to an
    // unrelated gateway, which would carry the anthropic credential there.
    let catalog = with_models_json(
        r#"{"providers":{"anthropic":{"models":[{"id":"claude-opus-5","context_window":123456}]}}}"#,
    );
    let model = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-opus-5")
        .unwrap();
    assert_eq!(
        model.base_url, "https://api.anthropic.com",
        "partial override must keep the built-in endpoint"
    );
    assert_eq!(
        model.api,
        e::core::provider::catalog::Api::Anthropic,
        "partial override must keep the built-in dialect"
    );
    assert_eq!(model.context_window, 123_456, "the correction itself lands");
}

#[test]
fn partial_override_keeps_declared_windows_and_efforts() {
    let _lock = ENV_LOCK.lock().unwrap();
    let catalog = with_models_json(r#"{"providers":{"anthropic":{"models":["claude-sonnet-5"]}}}"#);
    let model = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-sonnet-5")
        .unwrap();
    assert_eq!(
        model.context_window, 1_000_000,
        "a bare-id re-declaration keeps the built-in window"
    );
    assert_eq!(
        model.efforts,
        vec!["low".to_string(), "medium".to_string(), "high".to_string()],
        "a bare-id re-declaration keeps the built-in efforts"
    );
}

#[test]
fn current_anthropic_builtins_are_adaptive_haiku_is_manual() {
    let _lock = ENV_LOCK.lock().unwrap();
    clear_env_keys();
    let catalog = e::core::provider::catalog::catalog();
    let thinking = |id: &str| {
        catalog
            .iter()
            .find(|m| m.provider == "anthropic" && m.id == id)
            .unwrap_or_else(|| panic!("{id} missing from the built-in catalog"))
            .thinking
    };
    use e::core::provider::catalog::Thinking::*;
    for id in [
        "claude-fable-5",
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-opus-4-8",
    ] {
        assert_eq!(thinking(id), Adaptive, "{id} must be adaptive-thinking");
    }
    assert_eq!(thinking("claude-haiku-4-5"), Manual);
}

#[test]
fn a_partial_override_inherits_the_builtins_thinking_mode() {
    let _lock = ENV_LOCK.lock().unwrap();
    let catalog = with_models_json(
        r#"{"providers":{"anthropic":{"models":[{"id":"claude-sonnet-5","context_window":12345}]}}}"#,
    );
    let sonnet = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-sonnet-5")
        .unwrap();
    assert_eq!(sonnet.context_window, 12345);
    assert_eq!(
        sonnet.thinking,
        e::core::provider::catalog::Thinking::Adaptive,
        "correcting one field must not swap the thinking wire shape"
    );
}

#[test]
fn a_custom_provider_without_base_url_is_rejected_with_a_warning() {
    let _lock = ENV_LOCK.lock().unwrap();
    let dir = std::env::temp_dir().join(format!("e-model-base-required-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("E_HOME", &dir);
    std::fs::write(
        dir.join("models.json"),
        r#"{"providers":{"custom":{"models":["secret-target"]}}}"#,
    )
    .unwrap();

    let catalog = e::core::provider::catalog::catalog();
    assert!(
        !catalog
            .iter()
            .any(|model| model.provider == "custom" && model.id == "secret-target"),
        "a provider with no endpoint must not inherit an unrelated host"
    );
    assert_eq!(
        e::core::provider::catalog::config_warnings(),
        vec!["models.json: provider custom requires an explicit base_url"]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "linux")]
#[test]
fn trust_keys_distinguish_non_utf8_paths_with_the_same_lossy_form() {
    use std::os::unix::ffi::OsStringExt;

    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("e-trust-bytes-{}", std::process::id()));
    let parent = home.join("workspaces");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&parent).unwrap();
    std::env::set_var("E_HOME", &home);

    let first = parent.join(std::ffi::OsString::from_vec(b"project-\xff".to_vec()));
    let second = parent.join(std::ffi::OsString::from_vec(b"project-\xfe".to_vec()));
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    assert_eq!(first.to_string_lossy(), second.to_string_lossy());

    e::core::config::trust::set(&first, true).unwrap();
    assert_eq!(e::core::config::trust::status(&first), Some(true));
    assert_eq!(
        e::core::config::trust::status(&second),
        None,
        "a distinct raw path must receive its own trust decision"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn legacy_utf8_trust_keys_remain_readable() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("e-trust-legacy-{}", std::process::id()));
    let workspace = home.join("workspace");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&workspace).unwrap();
    std::env::set_var("E_HOME", &home);
    let workspace = workspace.canonicalize().unwrap();
    let legacy = workspace.to_str().unwrap();

    std::fs::write(
        home.join("trust.json"),
        serde_json::to_vec(&serde_json::json!({
            legacy: { "trusted": true }
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        e::core::config::trust::status(&workspace),
        Some(true),
        "valid UTF-8 trust decisions migrate read-only"
    );

    let _ = std::fs::remove_dir_all(&home);
}
