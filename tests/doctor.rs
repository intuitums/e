//! `e doctor` reports useful state without echoing credentials.

use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

// E_HOME is process-global and must remain ours while the report reads it.
#[test]
fn report_is_redacted_and_local_only() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let home = std::env::temp_dir().join(format!(
        "e-doctor-{}-{}-\x1b]0;not-a-terminal-title\x07",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("auth.json"),
        r#"{"format_version":1,"anthropic":{"key":"DO-NOT-PRINT-THIS"}}"#,
    )
    .unwrap();
    std::fs::write(home.join("settings.json"), r#"{"format_version":1}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    let host = e::core::api::ExtensionHost::empty();
    let report = e::core::providers::diagnostics::report(&host);
    let rendered = e::core::providers::diagnostics::render(&report);
    assert!(rendered.contains("auth.json: valid, format 1"));
    assert!(!rendered.contains("DO-NOT-PRINT-THIS"));
    assert!(!rendered.contains('\x1b'));
    assert!(!rendered.contains('\x07'));

    let leftovers = std::fs::read_dir(&home).unwrap().flatten().any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .starts_with(".doctor-write-")
    });
    assert!(!leftovers, "write probe was not removed");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn doctor_cli_emits_the_report_and_rejects_unknown_flags() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let home = std::env::temp_dir().join(format!(
        "e-doctor-cli-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&home).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["doctor", "--no-network"])
        .env("E_HOME", &home)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("e doctor\n"));
    assert!(stdout.contains("configuration:"));
    assert!(!stdout.contains("reachable"));

    let invalid = std::process::Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["doctor", "--unknown"])
        .env("E_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8(invalid.stderr)
        .unwrap()
        .contains("usage: e doctor"));

    let _ = std::fs::remove_dir_all(home);
}

#[cfg(unix)]
#[test]
fn doctor_remains_available_when_an_extension_startup_hook_is_broken() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let home = std::env::temp_dir().join(format!(
        "e-doctor-extension-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let extensions = home.join("extensions");
    std::fs::create_dir_all(&extensions).unwrap();
    let extension = extensions.join("broken-startup");
    std::fs::write(
        &extension,
        r##"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"id":1000000,"result":{"name":"broken-startup","version":"1","hooks":["startup"]}}'
while IFS= read -r line; do
  case "$line" in
    *'"method":"hook.startup"'*) printf '%s\n' '{"id":1,"error":"broken on purpose"}' ;;
    *'"method":"shutdown"'*) exit 0 ;;
  esac
done
"##,
    )
    .unwrap();
    std::fs::set_permissions(&extension, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["doctor", "--no-network"])
        .env("E_HOME", &home)
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("broken-startup: version 1, running"));
    assert!(!stdout.contains("broken on purpose"));

    let _ = std::fs::remove_dir_all(home);
}
