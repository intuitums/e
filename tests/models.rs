//! The model catalog. Pins: models.json models carry their declared context
//! window (per-model beats per-provider default beats 200k), and a file entry
//! with a built-in's name replaces the built-in — the file wins.

use std::sync::Mutex;

// E_HOME is process-global; serialize the tests that set it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_models_json(json: &str) -> Vec<e::core::model::Model> {
    let dir = std::env::temp_dir().join(format!("e-models-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("models.json"), json).unwrap();
    std::env::set_var("E_HOME", &dir);
    let catalog = e::core::model::catalog();
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
    assert_eq!(e::core::trust::status(&ws), None);
    assert!(!e::core::context::system_prompt(&ws).contains("SECRET-PROJECT-RULE"));

    // Declined → still out.
    e::core::trust::set(&ws, false).unwrap();
    assert_eq!(e::core::trust::status(&ws), Some(false));
    assert!(!e::core::context::system_prompt(&ws).contains("SECRET-PROJECT-RULE"));

    // Trusted → it loads.
    e::core::trust::set(&ws, true).unwrap();
    assert!(e::core::context::system_prompt(&ws).contains("SECRET-PROJECT-RULE"));

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn xai_builtins_carry_their_real_windows() {
    let _lock = ENV_LOCK.lock().unwrap();
    std::env::set_var("E_HOME", std::env::temp_dir().join("e-models-none"));
    let catalog = e::core::model::catalog();
    let find = |id: &str| {
        catalog
            .iter()
            .find(|m| m.provider == "xai" && m.id == id)
            .unwrap()
    };
    assert_eq!(find("grok-4.3").context_window, 1_000_000);
    assert_eq!(find("grok-4.6").context_window, 500_000);
    assert_eq!(find("grok-build-0.1").context_window, 256_000);
    assert_eq!(find("grok-4.6").base_url, "https://api.x.ai/v1");
    assert_eq!(e::core::model::display_name("xai"), "xAI");
    assert_eq!(e::core::model::display_name("openai"), "OpenAI");
    assert_eq!(e::core::model::display_name("anthropic"), "Anthropic");
    let anthropic = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-fable-5")
        .unwrap();
    assert_eq!(anthropic.context_window, 1_000_000);
    assert!(matches!(anthropic.api, e::core::model::Api::Anthropic));
    let openai = catalog
        .iter()
        .find(|m| m.provider == "openai" && m.id == "gpt-5.5-pro")
        .unwrap();
    assert_eq!(openai.context_window, 1_050_000);
    assert!(matches!(openai.api, e::core::model::Api::Responses));
    assert_eq!(e::core::model::display_name("opencode-go"), "OpenCode Go");
    assert_eq!(e::core::model::display_name("openai-codex"), "OpenAI Codex");
}
