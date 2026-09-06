//! Regressions for the lifecycle and persistence review of the core refactor.
mod common;

use common::{env_lock, serve_sse, test_model, Home};
use e::core::agent::{Agent, AgentOptions, SessionEvent};
use e::core::providers::catalog::Api;
use e::core::providers::ChatMessage;
use e::core::session::SessionLog;
use serde_json::json;

/// Encode a batch using the provider's ordinary streaming wire format.
fn batch(calls: Vec<serde_json::Value>) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{"delta": {"tool_calls": calls}}]
        })
    )
}

/// Give every call a stable source position and provider id.
fn call(index: usize, name: &str, args: serde_json::Value) -> serde_json::Value {
    json!({"index":index, "id":format!("c{index}"),
        "function":{"name":name,"arguments":args.to_string()}})
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn tool_execution_obeys_the_configured_concurrency_limit() {
    let _lock = env_lock();
    let home = Home::new("review-concurrency");
    home.auth(r#"{"mock":{"key":"test"}}"#);
    home.write("settings.json", r#"{"tool_concurrency":2}"#);
    let first = batch(
        (0..5)
            .map(|i| call(i, "bash", json!({"command":"sleep 0.05"})))
            .collect(),
    );
    let reply = "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\ndata: [DONE]\n\n";
    let (port, server) = serve_sse(&[&first, reply]);
    let (mut agent, mut events) = Agent::with_options(
        test_model("mock", port, Api::Completions),
        AgentOptions {
            home: Some(home.dir.clone()),
            cwd: Some(home.dir.clone()),
            save_session: false,
            ..AgentOptions::default()
        },
    );
    agent.submit("run".into(), "system".into());
    let mut active = 0;
    let mut completed = 0;
    let mut maximum = 0;
    while let Some(event) = events.recv().await {
        match event {
            SessionEvent::ToolStart { .. } => {
                active += 1;
                maximum = maximum.max(active);
            }
            SessionEvent::ToolEnd { .. } => {
                active -= 1;
                completed += 1;
            }
            SessionEvent::Error(error) => panic!("{error}"),
            SessionEvent::TurnEnd { aborted } => {
                assert!(!aborted);
                break;
            }
            _ => {}
        }
    }
    assert_eq!((maximum, active, completed), (2, 0, 5));
    server.join().unwrap();
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn same_file_calls_execute_in_provider_order() {
    let _lock = env_lock();
    let home = Home::new("review-file-order");
    home.auth(r#"{"mock":{"key":"test"}}"#);
    let first = batch(vec![
        call(0, "write", json!({"path":"file", "content":"first"})),
        call(1, "write", json!({"path":"./file", "content":"second"})),
        call(2, "read", json!({"path":"file"})),
    ]);
    let reply = "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\ndata: [DONE]\n\n";
    let (port, server) = serve_sse(&[&first, reply]);
    let (mut agent, mut events) = Agent::with_options(
        test_model("mock", port, Api::Completions),
        AgentOptions {
            home: Some(home.dir.clone()),
            cwd: Some(home.dir.clone()),
            save_session: false,
            ..AgentOptions::default()
        },
    );
    agent.submit("run".into(), "system".into());
    let mut lifecycle = Vec::new();
    while let Some(event) = events.recv().await {
        match event {
            SessionEvent::ToolStart { id } => lifecycle.push((id, true)),
            SessionEvent::ToolEnd { id, .. } => lifecycle.push((id, false)),
            SessionEvent::Error(error) => panic!("{error}"),
            SessionEvent::TurnEnd { aborted } => {
                assert!(!aborted);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        lifecycle,
        [
            (1, true),
            (1, false),
            (2, true),
            (2, false),
            (3, true),
            (3, false)
        ]
    );
    assert_eq!(
        std::fs::read_to_string(home.dir.join("file")).unwrap(),
        "second"
    );
    server.join().unwrap();
}

#[test]
fn nodes_reject_corruption_on_an_inactive_branch() {
    let _lock = env_lock();
    let home = Home::new("review-tree");
    for parent in ["missing", "bad"] {
        let path = home.dir.join("session.jsonl");
        let entries = [
            json!({"type":"session","id":"s","cwd":"/tmp","created":1,"model":"mock"}),
            json!({"type":"message","id":"bad","parent":parent,"message":ChatMessage::user("bad branch")}),
            json!({"type":"message","id":"good","parent":null,"message":ChatMessage::user("active branch")}),
        ];
        home.write(
            "session.jsonl",
            entries
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert!(
            SessionLog::nodes(&path).is_err(),
            "accepted parent {parent}"
        );
        assert!(SessionLog::load(&path).is_err(), "accepted parent {parent}");
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn oversized_usage_reaches_the_context_guard_without_overflowing() {
    let _lock = env_lock();
    let home = Home::new("review-usage");
    home.auth(r#"{"mock":{"key":"test"}}"#);
    home.write("file", "small result");
    let first = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices":[{"delta":{"tool_calls":[call(0, "read", json!({"path":"file"}))]}}],
            "usage":{"prompt_tokens":u64::MAX,"completion_tokens":1}
        })
    );
    let (port, server) = serve_sse(&[&first]);
    let (mut agent, mut events) = Agent::with_options(
        test_model("mock", port, Api::Completions),
        AgentOptions {
            home: Some(home.dir.clone()),
            cwd: Some(home.dir.clone()),
            save_session: false,
            ..AgentOptions::default()
        },
    );
    agent.submit("read".into(), "system".into());
    let mut guarded = false;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            match event {
                SessionEvent::Error(error) => {
                    assert!(error.contains("context is full"), "{error}");
                    guarded = true;
                }
                SessionEvent::TurnEnd { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(guarded);
    server.join().unwrap();
}

#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn background_configuration_tasks_inherit_the_selected_home() {
    let _lock = env_lock();
    let selected = Home::new("review-selected-home");
    selected.write("settings.json", r#"{"theme":"light"}"#);
    let global = Home::new("review-global-home");
    global.write("settings.json", r#"{"theme":"dark"}"#);
    use e::core::config::{home, settings};
    home::scope(selected.dir.clone(), async {
        home::spawn(async {
            assert_eq!(settings::get_string("theme").as_deref(), Some("light"));
            settings::set_string("theme", "custom").unwrap();
        })
        .await
        .unwrap();
    })
    .await;
    assert_eq!(settings::get_string("theme").as_deref(), Some("dark"));
    assert_eq!(
        home::with_home(selected.dir.clone(), || settings::get_string("theme")).as_deref(),
        Some("custom")
    );
}
