//! The SDK facade's own surface: model resolution and clean construction.
//! The turn loop the facade drives is pinned by the library's own integration
//! tests (`tests/api.rs`, `tests/stream.rs`); this pins the wrapper's logic —
//! that a bad slug is a typed error and a built session starts empty on the
//! resolved model.

use e_sdk::{Error, Session};

/// One test, so the process-global `E_HOME` is set once with no cross-test
/// race: a fresh isolated home keeps resolution off the developer's config.
#[test]
fn builder_resolves_models_and_reports_a_bad_slug() {
    let home = std::env::temp_dir().join(format!("e-sdk-facade-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    // An unknown slug is a typed error naming the slug asked for.
    match Session::builder()
        .model("nope/not-a-model")
        .save_session(false)
        .build()
    {
        Err(Error::ModelNotFound(slug)) => assert_eq!(slug, "nope/not-a-model"),
        Err(other) => panic!("wrong error: {other}"),
        Ok(_) => panic!("an unknown slug must not resolve"),
    }

    // No model requested falls back to the catalog's shipped default: build
    // succeeds, names a model, and the conversation starts empty.
    let session = Session::builder().save_session(false).build().unwrap();
    assert!(!session.model_slug().is_empty());
    assert!(session.history().is_empty());
    assert!(session.model().context_window > 0);

    std::fs::remove_dir_all(&home).ok();
}

/// The builder's `.home()` method scopes model resolution and system prompt
/// generation to the specified home — credentials, settings, and AGENTS.md are
/// read from there, not from E_HOME or the default ~/.e.
#[test]
fn builder_home_isolates_construction() {
    // Create an isolated home with a distinctive AGENTS.md.
    let isolated = std::env::temp_dir().join(format!(
        "e-sdk-home-isolate-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&isolated).unwrap();
    let marker = "SDK_HOME_ISOLATION_MARKER_9f3a2c";
    std::fs::write(isolated.join("AGENTS.md"), marker).unwrap();

    // Build a session pointing at the isolated home.
    let mut session = Session::builder()
        .home(&isolated)
        .save_session(false)
        .build()
        .unwrap();

    // The agent's system_prompt() assembles from its stored home, so it should
    // include the isolated AGENTS.md content. This verifies that with_home
    // scoped the construction correctly and the agent received the home.
    let prompt = session.agent_mut().system_prompt();
    assert!(
        prompt.contains(marker),
        "system prompt should include the isolated home's AGENTS.md content, but got:\n{}",
        &prompt[..prompt.len().min(500)]
    );

    std::fs::remove_dir_all(&isolated).ok();
}
