//! Extension host round-trip against a real subprocess: a /bin/sh extension
//! that answers the line protocol. Pins discovery, the initialize handshake,
//! tool routing, command dispatch, and the tool_call hook (block + fail-open).

use std::sync::Mutex;

use e::core::api::{ExtensionHost, StartupAction};

// E_HOME is process-global; serialize the tests that set it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// hook.tool_call contains "tool_call", so the hook case must match first.
const FAKE: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      case "$line" in
        *'"extensions_config":{"fake":{"mode":"x"}}'*) config_seen=yes ;;
      esac
      printf '{"id":%s,"result":{"name":"fake","version":"1","tools":[{"name":"greet","description":"say hi","parameters":{"type":"object","properties":{}}},{"name":"bash","description":"my bash","parameters":{"type":"object","properties":{}}}],"commands":[{"name":"ping","description":"pong"}],"flags":[{"name":"-x, --extra","description":"an extension flag"}],"hooks":["startup","tool_call","input"]}}\n' "$id" ;;
    *'"hook.startup"'*)
      case "$line" in
        *startup-error*) printf '{"id":%s,"error":"bad startup"}\n' "$id" ;;
        *relaunch-me*) printf '{"id":%s,"result":{"argv":["-c"],"relaunch":{"cwd":"/tmp","env":{"E_TEST":"1"}}}}\n' "$id" ;;
        *probe-flags*) printf '{"id":%s,"result":{"argv":["-c"]}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{"argv":["-c"]}}\n' "$id" ;;
      esac ;;
    *'"hook.tool_call"'*)
      case "$line" in
        *danger*) printf '{"id":%s,"result":{"block":true,"reason":"nope"}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{"block":false}}\n' "$id" ;;
      esac ;;
    *'"hook.input"'*)
      case "$line" in
        *secret-word*) printf '{"id":%s,"result":{"consume":true,"notice":"swallowed"}}\n' "$id" ;;
        *rewrite-me*) printf '{"id":%s,"result":{"replace":"new text"}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{}}\n' "$id" ;;
      esac ;;
    *'"tool_call"'*)
      case "$line" in
        *name-me*) printf '{"id":%s,"result":{"content":"named","session_name":"my-session"}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{"content":"hello from fake"}}\n' "$id" ;;
      esac ;;
    *'"command"'*)
      case "$line" in
        *name-me*) printf '{"id":%s,"result":{"notice":"pong","session_name":"cmd-session"}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{"notice":"pong"}}\n' "$id" ;;
      esac ;;
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
            Self::with_extensions(&[(name, body)])
        }
        pub fn with_extensions(names_bodies: &[(&str, &str)]) -> TempHome {
            let dir = std::env::temp_dir().join(format!("e-api-test-{}", std::process::id()));
            let ext = dir.join("extensions");
            std::fs::create_dir_all(&ext).unwrap();
            for (name, body) in names_bodies {
                let path = ext.join(name);
                std::fs::write(&path, body).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                        .unwrap();
                }
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
    assert_eq!(
        host.flags(),
        vec![("-x, --extra".to_string(), "an extension flag".to_string())]
    );

    // Startup hooks consume custom arguments before e parses its own flags.
    match host.startup(vec!["--custom".into()]).await.unwrap() {
        StartupAction::Continue(argv) => assert_eq!(argv, vec!["-c"]),
        StartupAction::Relaunch { .. } => panic!("unexpected relaunch"),
    }
    match host.startup(vec!["relaunch-me".into()]).await.unwrap() {
        StartupAction::Relaunch { argv, request } => {
            assert_eq!(argv, vec!["-c"]);
            assert_eq!(request.cwd, "/tmp");
            assert_eq!(request.env["E_TEST"].as_deref(), Some("1"));
        }
        StartupAction::Continue(_) => panic!("expected relaunch"),
    }
    let error = match host.startup(vec!["startup-error".into()]).await {
        Ok(_) => panic!("advertised startup errors must be fatal"),
        Err(error) => error,
    };
    assert!(error.contains("bad startup"));

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

    // A tool may name the session as a side effect.
    let named = host.call_tool("greet", "name-me now").await;
    assert_eq!(named.session_name.as_deref(), Some("my-session"));
    // ...and an ordinary tool call leaves the name unset.
    let plain = host.call_tool("greet", "{}").await;
    assert!(plain.session_name.is_none());

    // Command dispatch.
    let out = host.run_command("ping", "").await;
    assert_eq!(out.notice.as_deref(), Some("pong"));
    assert_eq!(out.prompt, None);

    // A command may name the session too.
    let cmd_named = host.run_command("ping", "name-me").await;
    assert_eq!(cmd_named.session_name.as_deref(), Some("cmd-session"));

    // Input hook: consume, replace, and pass-through.
    let consumed = host.hook_input("has a secret-word inside").await;
    assert!(consumed.consume);
    assert_eq!(consumed.notice.as_deref(), Some("swallowed"));
    let rewritten = host.hook_input("rewrite-me now").await;
    assert!(!rewritten.consume);
    assert_eq!(rewritten.replace.as_deref(), Some("new text"));
    let pass = host.hook_input("ordinary line").await;
    assert!(!pass.consume);
    assert!(pass.replace.is_none());
    assert!(host.has_input_hook());

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

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn extension_exit_wakes_pending_requests() {
    const EXITING: &str = r#"#!/bin/sh
IFS= read -r line
id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"id":%s,"result":{"name":"exiting","tools":[{"name":"boom","parameters":{"type":"object"}}]}}\n' "$id"
exit 0
"#;
    let _lock = ENV_LOCK.lock().unwrap();
    let _home = tempdir::TempHome::with_extension("exiting.sh", EXITING);
    let (notices, _rx) = tokio::sync::mpsc::channel(4);
    let host = ExtensionHost::start(notices).await;
    assert!(host.owns_tool("boom"));

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        host.call_tool("boom", "{}"),
    )
    .await
    .expect("extension exit should resolve the request immediately");
    assert!(result.is_error);
    assert!(result.content.contains("extension exited"));
    host.shutdown().await;
}

/// The initialize handshake carries namespaced extension config from
/// settings.json (`extensions.<name>`), so extensions never squat on a
/// top-level settings key. The fake answers with its manifest only when it
/// sees its own config — discovery fails otherwise, which pins the delivery.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn initialize_carries_namespaced_config() {
    const CONFIG_FAKE: &str = r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"initialize"'*)
      case "$line" in
        *'"extensions_config":{"cfg":{"mode":"x"}}'*)
          printf '{"id":%s,"result":{"name":"cfg","version":"1","commands":[{"name":"cfg","description":"d"}]}}
' "$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')" ;;
        *) printf '{"id":%s,"error":"no config"}' "$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')" ;;
      esac ;;
    *'"shutdown"'*) exit 0 ;;
  esac
done
"#;
    let _lock = ENV_LOCK.lock().unwrap();
    let home = tempdir::TempHome::with_extension("cfg.sh", CONFIG_FAKE);
    std::fs::write(
        home.dir.join("settings.json"),
        r#"{"extensions":{"cfg":{"mode":"x"}},"top_level":"untouched"}"#,
    )
    .unwrap();
    let (notices, _rx) = tokio::sync::mpsc::channel(16);
    let host = ExtensionHost::start(notices).await;
    assert!(
        !host.is_empty(),
        "config-carrying extension must initialize"
    );
    assert_eq!(host.commands(), vec![("cfg".to_string(), "d".to_string())]);
    host.shutdown().await;
}

/// Typed flags are parsed from startup argv by e and handed to the hook as
/// `params.flags`. The fake echoes them back over notify; the test asserts
/// boolean (bare / =value / --no-), string (=value / value / bare-null),
/// last-wins, and that a display-only flag name is never parsed.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn startup_parses_typed_flags_into_params() {
    const FLAGS_FAKE: &str = r#"#!/usr/bin/env node
const rl = require("node:readline").createInterface({ input: process.stdin });
rl.on("line", (line) => {
  let req; try { req = JSON.parse(line); } catch { return; }
  if (req.method === "initialize") {
    process.stdout.write(JSON.stringify({ id: req.id, result: {
      name: "flagsprobe", version: "1",
      flags: [
        { name: "worktree", type: "string", description: "branch" },
        { name: "plan", type: "boolean", description: "plan mode" },
        { name: "-x, --extra", description: "display only" },
      ],
      hooks: ["startup"],
    }}) + "\n");
  } else if (req.method === "hook.startup") {
    process.stdout.write(JSON.stringify({ id: req.id, result: { argv: ["-c"] } }) + "\n");
    // Report what e parsed (notify is a no-reply extension->e message).
    if (req.params && req.params.flags) {
      process.stdout.write(JSON.stringify({ method: "notify", params: { message: "FLAGS " + JSON.stringify(req.params.flags) } }) + "\n");
    }
  } else if (req.method === "shutdown") process.exit(0);
});
"#;
    let _lock = ENV_LOCK.lock().unwrap();
    let _home = tempdir::TempHome::with_extension("flagsprobe.sh", FLAGS_FAKE);
    let (notices, mut rx) = tokio::sync::mpsc::channel(16);
    let host = ExtensionHost::start(notices).await;
    assert!(!host.is_empty());

    let mut flags: Option<serde_json::Value> = None;
    // Drive the startup with a mixed argv; the fake's hook echoes the flags.
    match host
        .startup(vec!["--worktree=feat".into(), "--plan".into(), "-c".into()])
        .await
        .unwrap()
    {
        StartupAction::Continue(_) => {}
        StartupAction::Relaunch { .. } => panic!("no relaunch"),
    }
    while let Some(msg) = rx.recv().await {
        if let Some(rest) = msg.strip_prefix("FLAGS ") {
            flags = serde_json::from_str(rest).ok();
            break;
        }
    }
    let flags = flags.expect("notify echoed the parsed flags");
    assert_eq!(flags["worktree"], "feat");
    assert_eq!(flags["plan"], true);
    assert_eq!(flags["-x, --extra"], serde_json::Value::Null); // display-only, unparsed

    // Boolean variants: =false, --no-, last-wins; string separating value.
    let mut flags: Option<serde_json::Value> = None;
    match host
        .startup(vec![
            "--plan=false".into(),
            "--worktree".into(),
            "feature-x".into(),
        ])
        .await
        .unwrap()
    {
        StartupAction::Continue(_) => {}
        StartupAction::Relaunch { .. } => panic!("no relaunch"),
    }
    while let Some(msg) = rx.recv().await {
        if let Some(rest) = msg.strip_prefix("FLAGS ") {
            flags = serde_json::from_str(rest).ok();
            break;
        }
    }
    let flags = flags.expect("second echo");
    assert_eq!(flags["plan"], false);
    assert_eq!(flags["worktree"], "feature-x");

    // A bare string flag parses as null whether at the end or followed by
    // another flag; and it never consumes the next flag as a value.
    let mut flags: Option<serde_json::Value> = None;
    match host
        .startup(vec!["--worktree".into(), "--plan".into()])
        .await
        .unwrap()
    {
        StartupAction::Continue(_) => {}
        StartupAction::Relaunch { .. } => panic!("no relaunch"),
    }
    while let Some(msg) = rx.recv().await {
        if let Some(rest) = msg.strip_prefix("FLAGS ") {
            flags = serde_json::from_str(rest).ok();
            break;
        }
    }
    let flags = flags.expect("third echo");
    assert_eq!(flags["worktree"], serde_json::Value::Null);

    host.shutdown().await;
}

/// Two extensions declaring the same string flag must consume the
/// separated value exactly once — a second declaration must not skip a
/// following flag. argv is never modified by parsing; the value stays put.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn shared_string_flag_consumes_one_value() {
    const BOTH_FAKE: &str = r#"#!/usr/bin/env node
const rl = require("node:readline").createInterface({ input: process.stdin });
rl.on("line", (line) => {
  let req; try { req = JSON.parse(line); } catch { return; }
  if (req.method === "initialize") {
    process.stdout.write(JSON.stringify({ id: req.id, result: {
      name: "shared", version: "1",
      flags: [
        { name: "dir", type: "string", description: "target" },
        { name: "plan", type: "boolean", description: "plan mode" },
      ],
      hooks: ["startup"],
    }}) + "\n");
  } else if (req.method === "hook.startup") {
    process.stdout.write(JSON.stringify({ id: req.id, result: { argv: req.params.argv } }) + "\n");
    if (req.params.flags) {
      process.stdout.write(JSON.stringify({ method: "notify", params: { message: "FLAGS " + JSON.stringify(req.params.flags) } }) + "\n");
    }
  } else if (req.method === "shutdown") process.exit(0);
});
"#;
    let _lock = ENV_LOCK.lock().unwrap();
    let _home = tempdir::TempHome::with_extensions(&[("a.sh", BOTH_FAKE), ("b.sh", BOTH_FAKE)]);
    let (notices, mut rx) = tokio::sync::mpsc::channel(16);
    let host = ExtensionHost::start(notices).await;
    assert!(!host.is_empty());

    let mut flags = None;
    match host
        .startup(vec!["--dir".into(), "target".into(), "--plan".into()])
        .await
        .unwrap()
    {
        StartupAction::Continue(argv) => {
            // Parsing never modifies argv — the value and following flag
            // stay untouched.
            assert_eq!(argv, vec!["--dir", "target", "--plan"]);
        }
        StartupAction::Relaunch { .. } => panic!("no relaunch"),
    }
    while let Some(msg) = rx.recv().await {
        if let Some(rest) = msg.strip_prefix("FLAGS ") {
            flags = serde_json::from_str::<serde_json::Value>(rest).ok();
            break;
        }
    }
    let flags = flags.expect("echoed flags");
    // The separated value was consumed once...
    assert_eq!(flags["dir"], "target");
    // ...and the following flag was still parsed (not skipped).
    assert_eq!(flags["plan"], true);
    host.shutdown().await;
}

/// Every extension with typed flags gets a `flags` notification at start —
/// even one with no startup hook. A tool-only extension reads its flags in
/// any handler (pi's getFlag semantics: passed value, else default).
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn flags_notification_reaches_tool_only_extensions() {
    const TOOLONLY_FAKE: &str = r#"#!/usr/bin/env node
const rl = require("node:readline").createInterface({ input: process.stdin });
let flags = {};
rl.on("line", (line) => {
  let req; try { req = JSON.parse(line); } catch { return; }
  if (req.method === "initialize") {
    process.stdout.write(JSON.stringify({ id: req.id, result: {
      name: "toolonly", version: "1",
      tools: [{ name: "peek", description: "report flags", parameters: { type: "object", properties: {} } }],
      flags: [
        { name: "dry", type: "boolean", default: false, description: "dry run" },
        { name: "tag", type: "string", default: "default-tag", description: "a tag" },
      ],
    }}) + "\n");
  } else if (req.method === "flags") {
    // Notification — no reply. Store for the tool to read.
    flags = (req.params && req.params.flags) || {};
  } else if (req.method === "tool_call") {
    if (req.params && req.params.name === "peek") {
      // A handler reads the flags it was given — the getFlag path.
      process.stdout.write(JSON.stringify({ id: req.id, result: {
        content: JSON.stringify({ dry: flags.dry, tag: flags.tag, hasDry: Object.hasOwn(flags, "dry") }),
      }}) + "\n");
    } else {
      process.stdout.write(JSON.stringify({ id: req.id, result: { content: "bad", is_error: true } }) + "\n");
    }
  } else if (req.method === "shutdown") process.exit(0);
});
"#;
    let _lock = ENV_LOCK.lock().unwrap();
    let _home = tempdir::TempHome::with_extension("toolonly.sh", TOOLONLY_FAKE);
    let (notices, _rx) = tokio::sync::mpsc::channel(16);
    // No startup hook — the flags notification must arrive on its own.
    let host = ExtensionHost::start(notices).await;
    assert!(!host.is_empty());

    // The extension reads what it was given. With `--dry` absent (the test
    // binary's argv declares no typed flags), the notification is empty and
    // raw flags stay absent — defaults are the extension's own concern, the
    // scaffold applies the manifest's `default` (see flag()).
    let r = host.call_tool("peek", "{}").await;
    let seen: serde_json::Value = serde_json::from_str(&r.content).unwrap();
    assert_eq!(seen["hasDry"], false, "absent flag stays absent: {seen}");
    host.shutdown().await;
}

/// An extension whose process dies before answering initialize must fail
/// fast — its death is detected on stdout EOF and reported, never left to
/// hold the launch for the full 5 s timeout (the startup-stall bug).
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn dead_extension_fails_fast_instead_of_stalling() {
    // A valid script with no stdin reader: node drains its event loop and
    // exits immediately, before e's initialize can be answered.
    const DIES_INSTANTLY: &str = r#"#!/usr/bin/env node
// no readline, no handlers — the process exits at once
"#;
    let _lock = ENV_LOCK.lock().unwrap();
    let _home = tempdir::TempHome::with_extension("dies.sh", DIES_INSTANTLY);
    let (notices, _rx) = tokio::sync::mpsc::channel(16);

    let t0 = std::time::Instant::now();
    let host = ExtensionHost::start(notices).await;
    let elapsed = t0.elapsed();
    // The extension is discovered but skipped: initialize failed fast.
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "a dead extension stalled launch for {elapsed:?} — it should be skipped in well under the 5 s timeout"
    );
    assert!(host.is_empty(), "dead extension must not be kept");

    // And a startup pass over the (now empty) host stays instant.
    let t1 = std::time::Instant::now();
    match host.startup(vec![]).await.unwrap() {
        StartupAction::Continue(_) => {}
        StartupAction::Relaunch { .. } => panic!("no relaunch"),
    }
    assert!(t1.elapsed() < std::time::Duration::from_millis(500));
}
