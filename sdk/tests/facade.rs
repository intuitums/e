//! The SDK facade's own surface: model resolution and clean construction.
//! The turn loop the facade drives is pinned by the library's own integration
//! tests (`tests/api.rs`, `tests/stream.rs`); this pins the wrapper's logic —
//! that a bad slug is a typed error and a built session starts empty on the
//! resolved model. Both tests route through `.home()` so nothing here touches
//! the process-global `E_HOME`, and they can run in parallel.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use e_sdk::{Error, Session};

/// A unique empty temporary home, removed even when an assertion panics.
struct IsolatedHome(PathBuf);

impl IsolatedHome {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "e-sdk-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        IsolatedHome(dir)
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// An empty home with no credentials: an unknown slug is a typed error naming
/// the slug asked for, and no model requested falls back to the catalog's
/// shipped default — build succeeds, names a model, and starts empty.
#[test]
fn builder_resolves_models_and_reports_a_bad_slug() {
    let home = IsolatedHome::new("facade");

    match Session::builder()
        .home(&home.0)
        .model("nope/not-a-model")
        .save_session(false)
        .build()
    {
        Err(Error::ModelNotFound(slug)) => assert_eq!(slug, "nope/not-a-model"),
        Err(other) => panic!("wrong error: {other}"),
        Ok(_) => panic!("an unknown slug must not resolve"),
    }

    let session = Session::builder()
        .home(&home.0)
        .save_session(false)
        .build()
        .unwrap();
    assert!(!session.model_slug().is_empty());
    assert!(session.history().is_empty());
    assert!(session.model().context_window > 0);
}

/// The builder's `.home()` scopes construction to that home — the agent's
/// system prompt is assembled from the isolated home's AGENTS.md, not from
/// `E_HOME` or the default `~/.e`.
#[test]
fn builder_home_isolates_construction() {
    let home = IsolatedHome::new("home-isolate");
    let marker = "SDK_HOME_ISOLATION_MARKER_9f3a2c";
    std::fs::write(home.0.join("AGENTS.md"), marker).unwrap();

    let mut session = Session::builder()
        .home(&home.0)
        .save_session(false)
        .build()
        .unwrap();

    // The agent assembles from its stored home, so the prompt must carry the
    // isolated AGENTS.md — proof that `.home()` reached both the catalog read
    // at build time and the prompt assembly at turn time.
    let prompt = session.agent_mut().system_prompt();
    assert!(
        prompt.contains(marker),
        "system prompt should include the isolated home's AGENTS.md content, but got:\n{}",
        &prompt[..prompt.len().min(500)]
    );
}
