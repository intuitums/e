//! The non-destructive store: writes preserve unknown keys, quarantine
//! corrupt files, and never wipe on a parse error.

use std::path::PathBuf;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

#[test]
fn unversioned_configuration_fixtures_remain_json_objects() {
    for name in ["settings-v0.json", "auth-v0.json", "trust-v0.json"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/config")
            .join(name);
        let object = e::core::config::store::read_object(&path)
            .unwrap_or_else(|error| panic!("compatibility fixture {name} failed: {error}"));
        assert!(!object.is_empty(), "compatibility fixture {name} was empty");
        assert!(!object.contains_key("format_version"));
    }
}

fn home(name: &str) -> PathBuf {
    // Unique per process so concurrent CI users on a shared box can't collide
    // on a fixed path (a stale other-user dir would make writes EPERM).
    let h = std::env::temp_dir().join(format!("e-store-{name}-{}", std::process::id()));
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

    e::core::config::settings::set_string("effort", "low").unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(h.join("settings.json")).unwrap()).unwrap();
    assert_eq!(after["effort"], "low"); // our change landed
    assert_eq!(after["format_version"], 1); // writes declare their format
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
    assert_eq!(after["format_version"], 1); // writes declare their format
    assert_eq!(after["opencode-go"]["key"], "k"); // kept
    assert_eq!(after["future-provider"]["scheme"], "totally-new"); // NOT wiped
}

#[test]
fn trust_write_versions_the_file_and_preserves_unknown_keys() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("trust-format");
    std::fs::write(h.join("trust.json"), r#"{"future":{"value":42}}"#).unwrap();
    let workspace = h.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    e::core::config::trust::set(&workspace, true).unwrap();

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(h.join("trust.json")).unwrap()).unwrap();
    assert_eq!(after["format_version"], 1);
    assert_eq!(after["future"]["value"], 42);
}

#[test]
fn future_configuration_formats_are_never_downgraded() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("future-formats");
    let future = r#"{"format_version":999,"future":{"must":"survive"}}"#;

    let settings_path = h.join("settings.json");
    std::fs::write(&settings_path, future).unwrap();
    let settings_error = e::core::config::settings::set_string("theme", "light").unwrap_err();
    assert_eq!(settings_error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read_to_string(&settings_path).unwrap(), future);

    let auth_path = h.join("auth.json");
    std::fs::write(&auth_path, future).unwrap();
    let auth_error =
        e::core::auth::set("xai", e::core::auth::Credential::ApiKey { key: "z".into() })
            .unwrap_err();
    assert_eq!(auth_error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read_to_string(&auth_path).unwrap(), future);

    let trust_path = h.join("trust.json");
    std::fs::write(&trust_path, future).unwrap();
    let workspace = h.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let trust_error = e::core::config::trust::set(&workspace, true).unwrap_err();
    assert_eq!(trust_error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(std::fs::read_to_string(&trust_path).unwrap(), future);
}

#[test]
fn a_corrupt_file_is_quarantined_not_reset() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("corrupt");
    std::fs::write(h.join("settings.json"), "{ this is not json").unwrap();

    e::core::config::settings::set_string("theme", "light").unwrap();

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
        std::fs::set_permissions(h.join("auth.json"), std::fs::Permissions::from_mode(0o000))
            .unwrap();

        // We may not be root (then the read fails as intended) or we may be
        // (then the read succeeds and the update must still preserve).
        let result =
            e::core::auth::set("new", e::core::auth::Credential::ApiKey { key: "z".into() });

        if let Err(err) = &result {
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        }
        std::fs::set_permissions(h.join("auth.json"), std::fs::Permissions::from_mode(0o644))
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

    e::core::config::settings::set_string("theme", "light").unwrap_err();

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
        assert_eq!(
            after[format!("key{i}")],
            serde_json::json!(true),
            "key{i} lost"
        );
    }
    let strays = std::fs::read_dir(&h)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains(".tmp"));
    assert!(!strays, "a temp file was left behind");
}

#[test]
fn subprocess_store_writer() {
    let Ok(path) = std::env::var("E_STORE_CHILD_PATH") else {
        return;
    };
    let key = std::env::var("E_STORE_CHILD_KEY").unwrap();
    let marker = std::env::var("E_STORE_CHILD_MARKER").ok();
    let delay_ms = std::env::var("E_STORE_CHILD_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);

    e::core::config::store::update(std::path::Path::new(&path), 0o644, |object| {
        if let Some(marker) = marker {
            std::fs::write(marker, b"read").unwrap();
        }
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        object.insert(key, serde_json::json!(true));
    })
    .unwrap();
}

#[test]
fn concurrent_process_updates_preserve_both_snapshots() {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let h = home("concurrent-processes");
    let path = h.join("settings.json");
    let marker = h.join("first-has-read");

    let mut first = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "subprocess_store_writer", "--nocapture"])
        .env("E_HOME", &h)
        .env("E_STORE_CHILD_PATH", &path)
        .env("E_STORE_CHILD_KEY", "alpha")
        .env("E_STORE_CHILD_MARKER", &marker)
        .env("E_STORE_CHILD_DELAY_MS", "300")
        .spawn()
        .unwrap();

    let started = std::time::Instant::now();
    while !marker.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "first writer never reached its read-modify-write section"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let second = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "subprocess_store_writer", "--nocapture"])
        .env("E_HOME", &h)
        .env("E_STORE_CHILD_PATH", &path)
        .env("E_STORE_CHILD_KEY", "beta")
        .status()
        .unwrap();
    assert!(second.success(), "second config writer failed");
    assert!(
        first.wait().unwrap().success(),
        "first config writer failed"
    );

    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(after["alpha"], true);
    assert_eq!(after["beta"], true);
}
