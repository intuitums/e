//! Cross-component contracts for independent agents and lifecycle ownership.
mod common;

use common::{env_lock, serve_sse, test_model, Home};
use e::core::agent::{Agent, AgentOptions, SessionEvent};
use e::core::providers::catalog::Api;
use e::core::tools::{ToolOutcome, ToolRuntime};
use std::sync::atomic::AtomicBool;

/// Consume one complete run without mutating the agent's lifecycle state.
async fn finish(events: &mut tokio::sync::mpsc::Receiver<SessionEvent>) -> String {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut output = String::new();
        loop {
            match events.recv().await.expect("terminal event") {
                SessionEvent::TextDelta(text) => output.push_str(&text),
                SessionEvent::Error(error) => panic!("unexpected error: {error}"),
                SessionEvent::TurnEnd { aborted } => {
                    assert!(!aborted);
                    return output;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("run must complete")
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_agents_keep_credentials_and_logs_in_their_own_homes() {
    let _lock = env_lock();
    let reply = "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\ndata: [DONE]\n\n";
    let (port, server) = serve_sse(&[reply, reply]);
    let first = Home::new("runtime-first");
    first.auth(r#"{"mock":{"key":"first-test-key"}}"#);
    let second = Home::new("runtime-second");
    second.auth(r#"{"mock":{"key":"second-test-key"}}"#);
    let make = |home: &Home| {
        Agent::with_options(
            test_model("mock", port, Api::Completions),
            AgentOptions {
                home: Some(home.dir.clone()),
                cwd: Some(home.dir.clone()),
                ..AgentOptions::default()
            },
        )
    };
    let (mut a, mut a_events) = make(&first);
    let (mut b, mut b_events) = make(&second);
    a.submit("first".into(), "system".into());
    b.submit("second".into(), "system".into());
    let (a_text, b_text) = tokio::join!(finish(&mut a_events), finish(&mut b_events));
    assert_eq!((a_text.as_str(), b_text.as_str()), ("done", "done"));
    assert!(!a.is_streaming() && !b.is_streaming());
    assert!(a.session_path().unwrap().starts_with(&first.dir));
    assert!(b.session_path().unwrap().starts_with(&second.dir));
    let requests = server.join().unwrap();
    assert!(requests.iter().any(|r| r.contains("Bearer first-test-key")));
    assert!(requests
        .iter()
        .any(|r| r.contains("Bearer second-test-key")));
}

#[test]
fn another_agents_read_cannot_refresh_a_stale_edit() {
    let _lock = env_lock();
    let home = Home::new("runtime-observations");
    let file = home.dir.join("file");
    std::fs::write(&file, "old").unwrap();
    let a = ToolRuntime::default();
    let b = ToolRuntime::default();
    let run = |state: &ToolRuntime, name, args: serde_json::Value| {
        state.run_streaming(
            name,
            &args.to_string(),
            &home.dir,
            &AtomicBool::new(false),
            |_, _| {},
        )
    };
    assert!(!run(&a, "read", serde_json::json!({"path":"file"})).is_error());
    std::fs::write(&file, "new content").unwrap();
    assert!(!run(&b, "read", serde_json::json!({"path":"file"})).is_error());
    let edit = run(
        &a,
        "edit",
        serde_json::json!({"path":"file","old_string":"new content","new_string":"lost"}),
    );
    assert_eq!(edit.summary, "stale");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "new content");
}

#[test]
fn background_handles_are_private_to_the_owning_agent() {
    let a = ToolRuntime::default();
    let b = ToolRuntime::default();
    let run = |state: &ToolRuntime, args: serde_json::Value| {
        state.run_streaming(
            "bash",
            &args.to_string(),
            std::path::Path::new("."),
            &AtomicBool::new(false),
            |_, _| {},
        )
    };
    let started = run(
        &a,
        serde_json::json!({"command":"sleep 30","background":true}),
    );
    assert_eq!(started.outcome, ToolOutcome::Completed);
    let handle = started.content.split_whitespace().nth(3).unwrap();
    assert!(run(&b, serde_json::json!({"handle":handle})).is_error());
    assert!(!run(&a, serde_json::json!({"handle":handle})).is_error());
    run(&a, serde_json::json!({"handle":handle,"signal":"kill"}));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn starting_a_new_turn_does_not_revive_cancelled_work() {
    let _lock = env_lock();
    let home = Home::new("cancel-next-turn");
    home.auth(r#"{"mock":{"key":"k"}}"#);
    let first = serde_json::json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"slow","function":{"name":"bash","arguments":"{\"command\":\"sleep 0.5; printf escaped > escaped\"}"}}]}}]});
    let first = format!("data: {first}\n\ndata: [DONE]\n\n");
    let reply = "data: {\"choices\":[{\"delta\":{\"content\":\"new turn\"}}]}\n\ndata: [DONE]\n\n";
    let (port, server) = serve_sse(&[&first, reply]);
    let (mut agent, mut events) = Agent::with_options(
        test_model("mock", port, Api::Completions),
        AgentOptions {
            save_session: false,
            cwd: Some(home.dir.clone()),
            home: Some(home.dir.clone()),
            ..AgentOptions::default()
        },
    );
    agent.submit("start slow work".into(), "system".into());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            match event {
                SessionEvent::ToolStart { .. } => agent.interrupt(),
                SessionEvent::TurnEnd { aborted } => {
                    assert!(aborted);
                    break;
                }
                SessionEvent::Error(error) => panic!("{error}"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    agent.submit("new work".into(), "system".into());
    assert_eq!(finish(&mut events).await, "new turn");
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert!(!home.dir.join("escaped").exists());
    server.join().unwrap();
}
