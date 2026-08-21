//! /compact against a mock provider. Pins: the history reaches the model as a
//! flattened transcript with tool results trimmed, the streamed reply becomes
//! the summary, and load_compacted seeds a fresh, resumable session file.

use std::io::{Read, Write};
use std::net::TcpListener;

use e::core::agent::Agent;
use e::core::model::{Api, Model};
use e::core::provider::{ChatMessage, ToolCall};

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
        efforts: &[],
        context_window: 200_000,
    };

    let long_tool_output = "x".repeat(5000);
    let history = vec![
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

    let summary = e::core::compact::summarize(model.clone(), &history)
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
    agent.load_history(history);
    agent.load_compacted(&summary);
    let seeded = agent.history_snapshot();
    assert_eq!(seeded.len(), 1);
    assert!(seeded[0].content.contains("Goal: fix the parser."));
    let latest = e::core::session::list(&ws).into_iter().next().unwrap();
    let logged = std::fs::read_to_string(&latest.path).unwrap();
    assert!(
        logged.contains("Goal: fix the parser."),
        "seed not persisted to the new session"
    );

    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&ws);
}
