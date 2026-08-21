//! Extension host round-trip against a real subprocess: a /bin/sh extension
//! that answers the line protocol. Pins discovery, the initialize handshake,
//! tool routing, command dispatch, and the tool_call hook (block + fail-open).

use std::sync::Mutex;

use e::core::api::ExtensionHost;

// E_HOME is process-global; serialize the tests that set it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// hook.tool_call contains "tool_call", so the hook case must match first.
const FAKE: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*) printf '{"id":%s,"result":{"name":"fake","version":"1","tools":[{"name":"greet","description":"say hi","parameters":{"type":"object","properties":{}}},{"name":"bash","description":"my bash","parameters":{"type":"object","properties":{}}}],"commands":[{"name":"ping","description":"pong"}],"hooks":["tool_call"]}}\n' "$id" ;;
    *'"hook.tool_call"'*)
      case "$line" in
        *danger*) printf '{"id":%s,"result":{"block":true,"reason":"nope"}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{"block":false}}\n' "$id" ;;
      esac ;;
    *'"tool_call"'*) printf '{"id":%s,"result":{"content":"hello from fake"}}\n' "$id" ;;
    *'"command"'*) printf '{"id":%s,"result":{"notice":"pong"}}\n' "$id" ;;
    *'"shutdown"'*) exit 0 ;;
  esac
done
"#;

fn fake_home() -> tempdir::TempHome {
    tempdir::TempHome::with_extension("fake.sh", FAKE)
}

/// Minimal temp E_HOME helper; restores nothing (each test sets its own).
mod tempdir {
    pub struct TempHome {
        pub dir: std::path::PathBuf,
    }
    impl TempHome {
        pub fn with_extension(name: &str, body: &str) -> TempHome {
            let dir = std::env::temp_dir().join(format!("e-api-test-{}", std::process::id()));
            let ext = dir.join("extensions");
            std::fs::create_dir_all(&ext).unwrap();
            let path = ext.join(name);
            std::fs::write(&path, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            std::env::set_var("E_HOME", &dir);
            TempHome { dir }
        }
    }
    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

// The env lock is deliberately held across the awaits: E_HOME must stay ours
// for the whole test, and each #[tokio::test] runs on its own runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn extension_round_trip() {
    let _lock = ENV_LOCK.lock().unwrap();
    let _home = fake_home();
    let (notices, _rx) = tokio::sync::mpsc::channel(16);
    let host = ExtensionHost::start(notices).await;

    // Discovery + manifest.
    assert!(
        !host.is_empty(),
        "fake.sh should be discovered and initialized"
    );
    assert!(host.owns_tool("greet"));
    assert_eq!(
        host.commands(),
        vec![("ping".to_string(), "pong".to_string())]
    );

    // The extension's tools join the schema set, and "bash" overrides the built-in.
    let schemas = host.merged_tool_schemas();
    let names: Vec<&str> = schemas
        .iter()
        .filter_map(|s| s["function"]["name"].as_str())
        .collect();
    assert!(names.contains(&"greet"));
    assert_eq!(names.iter().filter(|n| **n == "bash").count(), 1);
    let bash = schemas
        .iter()
        .find(|s| s["function"]["name"] == "bash")
        .unwrap();
    assert_eq!(bash["function"]["description"], "my bash");

    // Tool call round-trip.
    let result = host.call_tool("greet", "{}").await;
    assert!(!result.is_error);
    assert_eq!(result.content, "hello from fake");

    // Command dispatch.
    let out = host.run_command("ping", "").await;
    assert_eq!(out.notice.as_deref(), Some("pong"));
    assert_eq!(out.prompt, None);

    // Hook: explicit block wins, everything else is allowed.
    assert_eq!(
        host.hook_tool_call("bash", r#"{"cmd":"danger"}"#)
            .await
            .as_deref(),
        Some("nope")
    );
    assert_eq!(host.hook_tool_call("bash", r#"{"cmd":"ls"}"#).await, None);

    host.shutdown().await;
}

#[tokio::test]
async fn empty_host_serves_builtins() {
    let host = ExtensionHost::empty();
    assert!(host.is_empty());
    assert!(!host.owns_tool("bash"));
    let names: Vec<String> = host
        .merged_tool_schemas()
        .iter()
        .filter_map(|s| s["function"]["name"].as_str().map(String::from))
        .collect();
    assert!(names.contains(&"bash".to_string()));
    let result = host.call_tool("greet", "{}").await;
    assert!(result.is_error);
}
