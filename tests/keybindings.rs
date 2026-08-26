//! `~/.e/keybindings.json`: overrides load, unrecognized entries fail open
//! rather than breaking the composer, and a missing/malformed file behaves
//! exactly like an empty one.

use std::sync::Mutex;

use e::core::config::keybindings::{self, Keymap};
use e::tui::content::composer::Key;

// E_HOME is process-global; serialize the tests that set it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_home<F: FnOnce()>(name: &str, f: F) {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!("e-keybindings-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);
    f();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn load_is_empty_with_no_file_on_disk() {
    with_home("missing", || {
        let map = keybindings::load();
        assert!(map.lookup("ctrl+w").is_none());
    });
}

#[test]
fn load_fails_open_on_malformed_json() {
    with_home("malformed", || {
        let home = std::env::var("E_HOME").unwrap();
        std::fs::write(format!("{home}/keybindings.json"), "not json").unwrap();
        let map = keybindings::load();
        assert!(map.lookup("ctrl+w").is_none());
    });
}

#[test]
fn load_parses_overrides_none_and_skips_unknown_actions() {
    with_home("parse", || {
        let home = std::env::var("E_HOME").unwrap();
        std::fs::write(
            format!("{home}/keybindings.json"),
            r#"{
                "ctrl+j": "none",
                "alt+d": "kill_word",
                "Ctrl+W": "home",
                "ctrl+q": "not_a_real_action"
            }"#,
        )
        .unwrap();

        let map = keybindings::load();
        assert!(
            matches!(map.lookup("ctrl+j"), Some(None)),
            "explicitly unbound"
        );
        assert!(matches!(map.lookup("alt+d"), Some(Some(Key::KillWord))));
        // Case/order-insensitive: "Ctrl+W" normalizes to "ctrl+w".
        assert!(matches!(map.lookup("ctrl+w"), Some(Some(Key::Home))));
        assert!(
            map.lookup("ctrl+q").is_none(),
            "an unrecognized action name is dropped, not stored as a broken binding"
        );
        assert!(
            map.lookup("ctrl+z").is_none(),
            "unmentioned chords fall through to the built-in binding"
        );
    });
}

#[test]
fn empty_keymap_never_overrides_anything() {
    let map = Keymap::empty();
    assert!(map.lookup("ctrl+w").is_none());
    assert!(map.lookup("enter").is_none());
}
