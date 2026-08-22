//! The system prompt: the base shape, the settings override, layered context.

use e::core::agent::context::system_prompt;
use std::path::Path;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_home<F: FnOnce()>(name: &str, f: F) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("e-prompt-{name}"));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);
    f();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn default_prompt_has_pi_structure() {
    with_home("default", || {
        let prompt = system_prompt(Path::new("/tmp/proj"));
        assert!(prompt.starts_with(
            "You are an expert coding assistant operating inside e, a coding agent harness."
        ));
        assert!(prompt.contains("Available tools:"));
        assert!(prompt.contains("- read:") && prompt.contains("- bash:"));
        assert!(prompt.contains("Guidelines:"));
        assert!(prompt.contains("- Be concise in your responses"));
        // The self-docs section: the agent learns e's own formats via `e docs`.
        assert!(prompt.contains("e documentation (read only when the user asks about e itself"));
        assert!(prompt.contains("`e docs <topic>`"));
        assert!(prompt
            .trim_end()
            .ends_with("Current working directory: /tmp/proj"));
        // AGENTS.md is bare by default — no project_context.
        assert!(!prompt.contains("<project_context>"));
    });
}

#[test]
fn settings_prompt_replaces_the_base() {
    with_home("override", || {
        let home = std::env::var("E_HOME").unwrap();
        std::fs::write(
            format!("{home}/settings.json"),
            r#"{"system_prompt":"You are Custom. Do custom things."}"#,
        )
        .unwrap();
        let prompt = system_prompt(Path::new("/tmp/proj"));
        assert!(prompt.starts_with("You are Custom. Do custom things."));
        // The base is gone, but the cwd tail still appends.
        assert!(!prompt.contains("Available tools:"));
        assert!(prompt.contains("Current working directory: /tmp/proj"));
    });
}

#[test]
fn agents_md_layers_as_project_instructions() {
    with_home("agents", || {
        let home = std::env::var("E_HOME").unwrap();
        std::fs::write(format!("{home}/AGENTS.md"), "Never touch the database.").unwrap();
        let prompt = system_prompt(Path::new("/tmp/proj"));
        assert!(prompt.contains("<project_context>"));
        assert!(prompt.contains("Never touch the database."));
        assert!(prompt.contains("<project_instructions path="));
    });
}
