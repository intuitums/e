//! Compaction. Pins: the keep-recent cut never lands on a tool result and
//! spares small histories, the auto threshold is window minus reserve, the
//! summarized part reaches the model flattened and trimmed, and the fresh
//! session file carries the seed plus the kept messages.

use std::io::{Read, Write};
use std::net::TcpListener;

use e::core::agent::Agent;
use e::core::providers::catalog::{Api, Model};
use e::core::providers::{ChatMessage, ToolCall};

fn sse(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn compact_summarizes_and_seeds_a_fresh_session() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-compact-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);
    let ws = std::env::temp_dir().join(format!("e-compact-ws-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    // macOS: /var is a symlink to /private/var; the agent sees the canonical
    // cwd, so the session lookup must use it too.
    let ws = ws.canonicalize().unwrap();
    std::env::set_current_dir(&ws).unwrap();

    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 262144];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let reply = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Goal: fix the parser. Next: run tests.\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        a.write_all(sse(reply).as_bytes()).unwrap();
        sent
    });

    let model = Model {
        provider: "mock".into(),
        id: "m".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Completions,
        efforts: Vec::new(),
        thinking: e::core::providers::catalog::Thinking::Manual,
        context_window: 200_000,
    };

    let long_tool_output = "x".repeat(5000);
    let history = [
        ChatMessage::user("fix the parser"),
        ChatMessage::assistant(
            "looking",
            vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: "{\"path\":\"parser.rs\"}".into(),
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
    let sent = server.join().unwrap();
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

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn threshold_is_window_minus_reserve() {
    use e::core::agent::compact::{should_compact, RESERVE_TOKENS};
    let window = 200_000u64;
    assert!(!should_compact(window - RESERVE_TOKENS, window));
    assert!(should_compact(window - RESERVE_TOKENS + 1, window));
    // A tiny window never underflows.
    assert!(should_compact(1, RESERVE_TOKENS / 2));
}

#[test]
fn split_spares_small_histories() {
    let history = vec![
        ChatMessage::user("hi"),
        ChatMessage::assistant("hello", Vec::new()),
    ];
    let (to_summarize, kept) = e::core::agent::compact::split(&history);
    assert!(to_summarize.is_empty());
    assert_eq!(kept.len(), 2);
}

#[test]
fn split_never_cuts_at_a_tool_result() {
    // Big old turn, then a recent turn whose tool result sits right where a
    // naive cut would land: the cut must move past it to a non-tool message.
    let big = "y".repeat(85_000); // ~21k estimated tokens
    let history = vec![
        ChatMessage::user("old work"),
        ChatMessage::assistant(&big, Vec::new()),
        ChatMessage::user("new task"),
        ChatMessage::assistant(
            "",
            vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: "{}".into(),
            }],
        ),
        ChatMessage::tool_result("c1", "contents"),
        ChatMessage::assistant("done", Vec::new()),
    ];
    let (to_summarize, kept) = e::core::agent::compact::split(&history);
    assert!(!to_summarize.is_empty(), "the big turn must be summarized");
    assert_ne!(kept[0].role, "tool", "cut landed on a tool result");
    // The kept tail is intact and in order.
    let roles: Vec<&str> = kept.iter().map(|m| m.role.as_str()).collect();
    assert!(roles
        .windows(2)
        .all(|w| !(w[1] == "tool" && w[0] == "user")));
    assert_eq!(kept.last().unwrap().content, "done");
}
