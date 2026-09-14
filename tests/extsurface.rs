//! The extension surface beyond version 1 (decision 0005), against a real
//! subprocess: event subscriptions, the before_turn / tool_result /
//! compact_summary hooks, tool labels, shortcuts, and the extension's own
//! `ui.*` / `session.*` requests — answered by whoever holds the request
//! channel, or "no ui" at once when nobody does.

mod common;

use common::{env_lock, Home};
use e::core::extensions::{ExtensionHost, HostRequest};

/// Put one shell extension into an isolated home.
fn with_extension(label: &str, body: &str) -> Home {
    let home = Home::new(&format!("surface-{label}"));
    let ext = home.dir.join("extensions");
    std::fs::create_dir_all(&ext).unwrap();
    let path = ext.join("surface.sh");
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    home
}

/// What the extension appended to its log file so far.
fn log_of(home: &Home) -> String {
    std::fs::read_to_string(home.dir.join("ext.log")).unwrap_or_default()
}

/// A version-2 extension: subscribes to two events, declares every new
/// hook, labels its tool, owns a shortcut, and on initialize immediately
/// asks two things of e (`session.info`, then `ui.select`), logging the
/// answers. Every request e sends is logged by method.
const SURFACE: &str = r#"#!/bin/sh
log="$E_HOME/ext.log"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\),"method".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' "$line" | grep -o '"ui":[a-z]*' >> "$log"
      printf '{"id":%s,"result":{"name":"surface","version":"2","events":["turn_start","tool_end"],"hooks":["before_turn","tool_result","compact_summary","render"],"renders":["tool:bash","assistant"],"tools":[{"name":"diff","description":"d","parameters":{"type":"object"},"label":{"category":"diff","running":"Diffing","completed":"Diffed","target":"path"}}],"commands":[{"name":"deploy","description":"ship it","arguments":"<env>","completions":true}],"shortcuts":[{"key":"Ctrl+Shift+G","description":"go"},{"key":"enter","description":"steal"}]}}\n' "$id"
      printf '{"id":"q1","method":"session.info","params":{}}\n'
      printf '{"id":"q2","method":"ui.select","params":{"title":"pick","options":["a","b"]}}\n' ;;
    *'"method":"hook.before_turn"'*)
      printf '{"id":%s,"result":{"system_suffix":"Be brief.","message":{"content":"ctx","internal":true}}}\n' "$id" ;;
    *'"method":"hook.tool_result"'*)
      case "$line" in
        *secret*) printf '{"id":%s,"result":{"content":"[redacted]"}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{}}\n' "$id" ;;
      esac ;;
    *'"method":"hook.compact_summary"'*)
      printf '{"id":%s,"result":{"summary":"edited summary"}}\n' "$id" ;;
    *'"method":"hook.render"'*)
      case "$line" in
        *'"kind":"tool"'*) printf '{"id":%s,"result":{"body":"rendered: bash","format":"markdown"}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{}}\n' "$id" ;;
      esac ;;
    *'"method":"command.complete"'*)
      case "$line" in
        *'"prefix":"st"'*) printf '{"id":%s,"result":{"items":[{"value":"staging","description":"pre-prod"}]}}\n' "$id" ;;
        *) printf '{"id":%s,"result":{"items":[{"value":"dev"},{"value":"staging"},{"value":"prod"}]}}\n' "$id" ;;
      esac ;;
    *'"method":"shortcut"'*)
      printf '{"id":%s,"result":{"notice":"shortcut ran","show":{"title":"t","body":"b","format":"text"}}}\n' "$id" ;;
    *'"method":"event"'*)
      printf '%s\n' "$line" | sed -n 's/.*"name":"\([a-z_]*\)".*/event \1/p' >> "$log" ;;
    *'"id":"q1"'*|*'"id":"q2"'*)
      printf 'reply %s\n' "$line" >> "$log" ;;
    *'"method":"shutdown"'*) exit 0 ;;
  esac
done
"#;

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn headless_hosts_answer_extension_requests_with_no_ui() {
    let _lock = env_lock();
    let home = with_extension("h", SURFACE);
    let (notices, _rx) = tokio::sync::mpsc::channel(16);
    let host = ExtensionHost::start(notices, None).await;
    settle().await;
    let log = log_of(&home);
    assert!(log.contains("\"ui\":false"), "{log}");
    assert!(
        log.contains(r#"reply {"error":"no ui","id":"q1"}"#),
        "session requests need a surface owner too: {log}"
    );
    assert!(
        log.contains(r#"reply {"error":"no ui","id":"q2"}"#),
        "{log}"
    );
    host.shutdown().await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn requests_reach_the_surface_owner_and_replies_return_verbatim() {
    let _lock = env_lock();
    let home = with_extension("h", SURFACE);
    let (notices, _rx) = tokio::sync::mpsc::channel(16);
    let (requests, mut inbox) = tokio::sync::mpsc::channel::<HostRequest>(8);
    let host = ExtensionHost::start(notices, Some(requests)).await;
    let first = inbox.recv().await.expect("session.info arrives");
    assert_eq!(
        (first.extension.as_str(), first.method.as_str()),
        ("surface", "session.info")
    );
    first.respond(Ok(serde_json::json!({"cwd": "/w"})));
    let second = inbox.recv().await.expect("ui.select arrives");
    assert_eq!(second.method, "ui.select");
    assert_eq!(second.params["title"], "pick");
    // Dropping a request unanswered still answers the extension.
    drop(second);
    settle().await;
    let log = log_of(&home);
    assert!(log.contains("\"ui\":true"), "{log}");
    assert!(
        log.contains(r#"reply {"id":"q1","result":{"cwd":"/w"}}"#),
        "{log}"
    );
    assert!(
        log.contains(r#"reply {"error":"request dropped","id":"q2"}"#),
        "{log}"
    );
    host.shutdown().await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn events_go_only_to_subscribers_and_hooks_chain_their_answers() {
    let _lock = env_lock();
    let home = with_extension("h", SURFACE);
    let (notices, _rx) = tokio::sync::mpsc::channel(16);
    let host = ExtensionHost::start(notices, None).await;

    host.event("turn_start", serde_json::json!({"prompt": "hi"}))
        .await;
    host.event("turn_end", serde_json::json!({"aborted": false}))
        .await;
    host.event("tool_end", serde_json::json!({"id": 1})).await;
    settle().await;
    let log = log_of(&home);
    assert!(
        log.contains("event turn_start") && log.contains("event tool_end"),
        "{log}"
    );
    assert!(
        !log.contains("event turn_end"),
        "a manifest that lists events gets exactly those: {log}"
    );

    assert!(host.has_hook("before_turn") && !host.has_hook("input"));
    let added = host.hook_before_turn("hi").await;
    assert_eq!(added.system_suffixes, vec!["Be brief.".to_string()]);
    assert_eq!(added.messages.len(), 1);
    assert!(added.messages[0].internal);
    assert_eq!(added.messages[0].content, "ctx");

    assert_eq!(
        host.hook_tool_result("bash", "the secret word", false)
            .await,
        Some("[redacted]".into())
    );
    assert_eq!(host.hook_tool_result("bash", "plain", false).await, None);
    assert_eq!(
        host.hook_compact_summary("draft").await,
        Some("edited summary".into())
    );

    // The render hook is asked only about what the manifest lists, and an
    // empty answer leaves the entry as e paints it.
    assert!(host.renders("tool:bash") && host.renders("assistant"));
    assert!(!host.renders("tool:read"));
    let rendered = host.hook_render("tool:bash", "bash", "raw").await.unwrap();
    assert_eq!(rendered.body, "rendered: bash");
    assert_eq!(rendered.format, e::core::extensions::Format::Markdown);
    assert!(host.hook_render("assistant", "", "reply").await.is_none());
    assert!(host.hook_render("tool:read", "read", "x").await.is_none());
    host.shutdown().await;
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn labels_and_shortcuts_are_declared_once_and_normalized() {
    let _lock = env_lock();
    let _home = with_extension("h", SURFACE);
    let (notices, mut rx) = tokio::sync::mpsc::channel(16);
    let host = ExtensionHost::start(notices, None).await;

    let label = host.tool_label("diff").expect("declared label");
    assert_eq!(
        (label.running.as_str(), label.target.as_str()),
        ("Diffing", "path")
    );
    assert!(host.tool_label("bash").is_none());

    // `Ctrl+Shift+G` normalizes; a bare `enter` is refused at the manifest
    // because it would steal the composer.
    assert_eq!(
        host.shortcuts(),
        vec![("ctrl+shift+g".to_string(), "go".to_string())]
    );
    assert!(host.has_shortcut("shift+ctrl+G"));
    assert!(!host.has_shortcut("enter"));
    let mut refused = false;
    while let Ok(notice) = rx.try_recv() {
        refused |= notice.contains("shortcut enter");
    }
    assert!(refused, "the refusal is visible");

    let result = host.run_shortcut("ctrl+shift+g").await;
    assert_eq!(result.notice.as_deref(), Some("shortcut ran"));
    assert_eq!(result.show.unwrap().title, "t");

    // Argument hints and completions ride the command declaration.
    assert_eq!(host.command_hint("deploy").as_deref(), Some("<env>"));
    assert!(host.has_completions("deploy") && !host.has_completions("other"));
    let all = host.complete_command("deploy", "").await;
    assert_eq!(
        all.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(),
        ["dev", "staging", "prod"]
    );
    let some = host.complete_command("deploy", "st").await;
    assert_eq!(some.len(), 1);
    assert_eq!(some[0].description.as_deref(), Some("pre-prod"));
    assert!(host.complete_command("other", "x").await.is_empty());
    host.shutdown().await;
}
