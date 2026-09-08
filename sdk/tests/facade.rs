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
