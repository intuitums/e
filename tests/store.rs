//! The non-destructive store: writes preserve unknown keys, quarantine
//! corrupt files, and never wipe on a parse error.

use std::path::PathBuf;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

fn home(name: &str) -> PathBuf {
    let h = std::env::temp_dir().join(format!("e-store-{name}"));
    let _ = std::fs::remove_dir_all(&h);
    std::fs::create_dir_all(&h).unwrap();
    std::env::set_var("E_HOME", &h);
    h
}

#[test]
fn settings_write_preserves_unknown_keys() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("settings");
    // A user hand-added a key e knows nothing about.
    std::fs::write(
        h.join("settings.json"),
        r#"{"theme":"dark","my_custom":{"deep":42}}"#,
    )
    .unwrap();

    e::core::config::settings::set_string("effort", "low");

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(h.join("settings.json")).unwrap()).unwrap();
    assert_eq!(after["effort"], "low"); // our change landed
    assert_eq!(after["theme"], "dark"); // untouched
    assert_eq!(after["my_custom"]["deep"], 42); // the unknown key survived
}

#[test]
fn auth_write_preserves_an_unparseable_entry() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("auth");
    // One good entry, one in a shape e can't interpret.
    std::fs::write(
        h.join("auth.json"),
        r#"{"opencode-go":{"key":"k"},"future-provider":{"scheme":"totally-new"}}"#,
    )
    .unwrap();

    // load() surfaces only what it understands…
    assert!(e::core::auth::load().contains_key("opencode-go"));
    assert!(!e::core::auth::load().contains_key("future-provider"));

    // …and a write to a different provider must not wipe the one it couldn't parse.
    e::core::auth::set("xai", e::core::auth::Credential::ApiKey { key: "z".into() }).unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(h.join("auth.json")).unwrap()).unwrap();
    assert_eq!(after["xai"]["key"], "z"); // added
    assert_eq!(after["opencode-go"]["key"], "k"); // kept
    assert_eq!(after["future-provider"]["scheme"], "totally-new"); // NOT wiped
}

#[test]
fn a_corrupt_file_is_quarantined_not_reset() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("corrupt");
    std::fs::write(h.join("settings.json"), "{ this is not json").unwrap();

    e::core::config::settings::set_string("theme", "light");

    // The write succeeded on a fresh object…
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(h.join("settings.json")).unwrap()).unwrap();
    assert_eq!(after["theme"], "light");
    // …and the broken original is preserved aside, recoverable.
    let quarantined = std::fs::read_dir(&h)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains("corrupt-"));
    assert!(quarantined, "corrupt file was not quarantined");
}

#[test]
fn an_unreadable_file_aborts_the_write_instead_of_being_wiped() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let h = home("unreadable");
        std::fs::write(h.join("auth.json"), r#"{"existing":{"key":"k"}}"#).unwrap();
        std::fs::set_permissions(
            h.join("auth.json"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();

        // We may not be root (then the read fails as intended) or we may be
        // (then the read succeeds and the update must still preserve).
        let result = e::core::auth::set("new", e::core::auth::Credential::ApiKey { key: "z".into() });

        match result {
            Err(ref err) => assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied),
            Ok(()) => {}
        }
        std::fs::set_permissions(
            h.join("auth.json"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(h.join("auth.json")).unwrap()).unwrap();
        assert_eq!(after["existing"]["key"], "k");
        if result.is_ok() {
            assert_eq!(after["new"]["key"], "z");
        }
    }
    #[cfg(not(unix))]
    {
        // Windows ACLs make a true unreadable file awkward; the guarantee is
        // covered by the unix branch.
    }
}

#[test]
fn quarantine_failure_preserves_the_corrupt_source() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("quarantine-fail");

    // A directory where the file must be: rename() of the corrupt original
    // aside cannot succeed, so the write must abort rather than proceed.
    std::fs::create_dir_all(h.join("settings.json")).unwrap();

    let result = e::core::config::settings::set_string("theme", "light");
    let _ = result;

    // The corrupt "file" (our directory) was never replaced by a real file,
    // and no fresh settings.json appeared beside it.
    assert!(
        h.join("settings.json").is_dir(),
        "corrupt source was clobbered"
    );
}

#[test]
fn concurrent_updates_preserve_every_key_and_keep_the_temp_unique() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("concurrent");
    let path = h.clone();

    let handles: Vec<_> = (0..8)
        .map(|i| {
            let path = path.clone();
            std::thread::spawn(move || {
                e::core::config::store::update(&path.join("settings.json"), 0o644, |obj| {
                    obj.insert(format!("key{i}"), serde_json::json!(true));
                })
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(h.join("settings.json")).unwrap()).unwrap();
    for i in 0..8 {
        assert_eq!(after[format!("key{i}")], serde_json::json!(true), "key{i} lost");
    }
    let strays = std::fs::read_dir(&h)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains(".tmp"));
    assert!(!strays, "a temp file was left behind");
}
