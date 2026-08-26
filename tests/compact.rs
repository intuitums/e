//! Compaction. Pins: the keep-recent cut never lands on a tool result and
//! spares small histories, the auto threshold is window minus reserve, the
//! summarized part reaches the model flattened and trimmed, and the fresh
//! session file carries the seed plus the kept messages.

mod common;

use common::{env_lock, serve_sse, test_model, Home};
use e::core::agent::Agent;
use e::core::providers::catalog::Api;
use e::core::providers::{ChatMessage, ToolCall};

// The env lock is deliberately held across the summarize await: E_HOME and
// the cwd must stay ours for the whole test, and each tokio test gets its
// own runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn compact_summarizes_and_seeds_a_fresh_session() {
    let _env = env_lock();
    let reply = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Goal: fix the parser. Next: run tests.\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, server) = serve_sse(&[reply]);
    let home = Home::new("compact");
    home.auth(r#"{"mock":{"key":"k"}}"#);
    let ws = std::env::temp_dir().join(format!("e-compact-ws-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    // macOS: /var is a symlink to /private/var; the agent sees the canonical
    // cwd, so the session lookup must use it too.
    let ws = ws.canonicalize().unwrap();
    std::env::set_current_dir(&ws).unwrap();

    let model = test_model("mock", port, Api::Completions);

    let long_tool_output = "x".repeat(5000);
    let history = [
        ChatMessage::user("fix the parser"),
        ChatMessage::assistant(
            "looking",
            vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: "{\"path\":\"parser.rs\"}".into(),
                signature: None,
            }],
        ),
        ChatMessage::tool_result("c1", &long_tool_output),
        ChatMessage::assistant("the bug is in line 3", Vec::new()),
    ];

    let summary = e::core::agent::compact::summarize(model.clone(), &history[..3])
        .await
        .unwrap();
    assert_eq!(summary, "Goal: fix the parser. Next: run tests.");

    // The request carried the flattened history, tool output trimmed.
    let sent = server.join().unwrap().remove(0);
    assert!(sent.contains("fix the parser"));
    assert!(sent.contains("[called read"));
    assert!(sent.contains("[trimmed]"), "long tool output not trimmed");
    assert!(
        !sent.contains(&long_tool_output),
        "full tool output leaked into the request"
    );

    // The seed lands as the first message of a fresh session file.
    let (agent, _rx) = Agent::new(model);
    agent.load_compacted(&summary, history[3..].to_vec());
    let seeded = agent.history_snapshot();
    assert_eq!(seeded.len(), 2, "seed plus the kept tail");
    assert!(seeded[0].content.contains("Goal: fix the parser."));
    assert_eq!(seeded[1].content, "the bug is in line 3");
    let latest = e::core::session::list(&ws).into_iter().next().unwrap();
    let logged = std::fs::read_to_string(&latest.path).unwrap();
    assert!(
        logged.contains("Goal: fix the parser."),
        "seed not persisted to the new session"
    );
    assert!(
        logged.contains("the bug is in line 3"),
        "kept tail not persisted to the new session"
    );

    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn threshold_is_window_minus_reserve() {
    use e::core::agent::compact::{
        keep_recent_tokens, reserve_tokens, should_compact, RESERVE_TOKENS,
    };
    // Large windows keep the reference reserve.
    let window = 200_000u64;
    assert_eq!(reserve_tokens(window), RESERVE_TOKENS);
    assert!(!should_compact(window - RESERVE_TOKENS, window));
    assert!(should_compact(window - RESERVE_TOKENS + 1, window));
    // Small windows scale: a 32k local model must not spend half its context
    // on a reserve tuned for 200k, and the keep budget must stay well under
    // the window or compaction becomes a no-op exactly where it matters.
    assert_eq!(reserve_tokens(32_000), 4_000);
    assert!(keep_recent_tokens(32_000) < 32_000 / 2);
    assert!(should_compact(29_000, 32_000));
    assert!(!should_compact(20_000, 32_000));
    // A tiny window never underflows.
    assert!(should_compact(1_900, 2_000));
}

#[test]
fn split_spares_small_histories() {
    let history = vec![
        ChatMessage::user("hi"),
        ChatMessage::assistant("hello", Vec::new()),
    ];
    let (to_summarize, kept) = e::core::agent::compact::split(&history, 200_000);
    assert!(to_summarize.is_empty());
    assert_eq!(kept.len(), 2);
}

#[test]
fn split_never_cuts_at_a_tool_result() {
    // The keep-budget overrun lands exactly on a recent tool result: the cut
    // must move past it — a result always follows its call — to the next
    // non-tool message, leaving the result summarized away with its turn.
    let big = "y".repeat(85_000); // ~21k estimated tokens
    let history = vec![
        ChatMessage::user("old work"),
        ChatMessage::assistant("earlier reply", Vec::new()),
        ChatMessage::user("new task"),
        ChatMessage::assistant(
            "",
            vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: "{}".into(),
                signature: None,
            }],
        ),
        ChatMessage::tool_result("c1", &big),
        ChatMessage::assistant("done", Vec::new()),
    ];
    let (to_summarize, kept) = e::core::agent::compact::split(&history, 200_000);
    assert_eq!(
        kept.len(),
        1,
        "the cut skipped the tool result to the next non-tool message"
    );
    assert_eq!(kept[0].content, "done");
    assert_eq!(to_summarize.len(), 5);
}

#[test]
fn split_never_separates_signed_thinking_from_its_assistant_turn() {
    // The keep-budget overrun lands exactly on the assistant turn that follows
    // a signed thinking block. Cutting there replays the block's absence as an
    // unsigned history — Anthropic rejects the request. The cut must move past
    // the pair instead, leaving both sides together.
    let big_args = "y".repeat(85_000); // ~21k estimated tokens
    let history = vec![
        ChatMessage::user("old work"),
        ChatMessage::assistant("earlier reply", Vec::new()),
        ChatMessage::user("new task"),
        ChatMessage::reasoning(r#"{"type":"thinking","thinking":"hmm","signature":"sig"}"#),
        ChatMessage::assistant(
            "",
            vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: big_args,
                signature: None,
            }],
        ),
        ChatMessage::tool_result("c1", "ok"),
        ChatMessage::assistant("done", Vec::new()),
    ];
    let (to_summarize, kept) = e::core::agent::compact::split(&history, 200_000);
    // A cut happened, and the signed block stayed with its turn: either both
    // were summarized away or both remain in the kept tail, adjacent.
    let reasoning_kept = kept.iter().any(|m| m.role == "reasoning");
    let turn_kept = kept
        .iter()
        .any(|m| m.tool_calls.iter().any(|c| c.id == "c1"));
    assert_eq!(
        reasoning_kept, turn_kept,
        "the cut separated the signed thinking block from its assistant turn"
    );
    if reasoning_kept {
        let r = kept.iter().position(|m| m.role == "reasoning").unwrap();
        let t = kept
            .iter()
            .position(|m| m.tool_calls.iter().any(|c| c.id == "c1"))
            .unwrap();
        assert_eq!(t, r + 1, "reasoning and its turn must stay adjacent");
    }
    assert!(to_summarize.len() >= 3, "the old turns were summarized");
}

#[test]
fn failed_fresh_log_keeps_the_old_session_attached() {
    // When the compaction seed's fresh log cannot be created (read-only
    // sessions dir), the old log must stay attached: later turns append to
    // the complete pre-compaction file instead of a new file holding only an
    // unanchored tail, and memory still moves to the compacted state.
    use std::os::unix::fs::PermissionsExt;
    let _env = env_lock();
    let home = Home::new("loadcompact");
    let ws = std::env::temp_dir().join(format!("e-loadcompact-ws-{}", std::process::id()));
    std::fs::create_dir_all(&ws).unwrap();
    let ws = ws.canonicalize().unwrap();
    std::env::set_current_dir(&ws).unwrap();

    let model = test_model("mock", 1, Api::Completions);

    // An old session with history, attached the way a resumed session is.
    let mut old = e::core::session::Session::create(&ws, "m").unwrap();
    let old_path = old.path().to_path_buf();
    old.append(&ChatMessage::user("original work")).unwrap();
    let (agent, _rx) = Agent::new(model);
    agent.set_session(Some(old));

    // Make the workspace's session directory unwritable so the fresh log's
    // create fails (the parent stays readable so create_dir_all still passes).
    let sessions = home.dir.join("sessions");
    let slug_dir = old_path.parent().unwrap();
    let before = std::fs::metadata(slug_dir).unwrap().permissions();
    std::fs::set_permissions(slug_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Probe that the fault actually took: root (CI containers, sandboxes)
    // ignores directory permissions, and then the failure this test pins
    // cannot be injected at all — skip rather than fail on a fault that
    // didn't happen.
    let probe = slug_dir.join(".probe");
    if std::fs::write(&probe, b"x").is_ok() {
        let _ = std::fs::remove_file(&probe);
        std::fs::set_permissions(slug_dir, before).unwrap();
        let _ = std::fs::remove_dir_all(&ws);
        eprintln!("skipped: permission-based fault injection is inert for this user");
        return;
    }

    agent.load_compacted("Goal: continue.", vec![ChatMessage::user("recent turn")]);

    // Memory moved to the compacted state regardless.
    let h = agent.history_snapshot();
    assert_eq!(h.len(), 2);
    assert!(h[0].content.contains("Goal: continue."));
    assert_eq!(h[1].content, "recent turn");

    // No second session log appeared.
    let logs: Vec<_> = walk(&sessions)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    assert_eq!(logs.len(), 1, "a fresh log was created despite the failure");

    // And later turns land in the old, complete log — not a headless tail.
    agent.record_user("after compact".into());
    let logged = std::fs::read_to_string(&old_path).unwrap();
    assert!(logged.contains("original work"));
    assert!(
        logged.contains("after compact"),
        "post-compaction turns must join the pre-compaction file"
    );

    std::fs::set_permissions(slug_dir, before).unwrap();
    let _ = std::fs::remove_dir_all(&ws);
}

/// Every file under `dir`, any depth — small trees only.
fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
