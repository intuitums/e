//! Regression tests for the Codex findings on PR #178 (fs tools).
//!
//! Each `tests/*.rs` is its own binary; `common/` supplies the mock server,
//! `E_HOME` lock, and request collector so the pins stay about tool behavior.

mod common;

use common::{env_lock, serve_sse, test_model, Home};
use e::core::agent::{Agent, SessionEvent};
use e::core::providers::catalog::Api;

/// A `read` with `limit` must stop before pulling the next line, so a line
/// past the window that is oversized (or invalid UTF-8) cannot turn a valid
/// window into an error.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn read_limit_does_not_error_on_a_later_oversized_line() {
    let _lock = env_lock();
    let first = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
        "\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"f.txt\\\",\\\"limit\\\":1}\"}}]}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":10}}\n\n",
        "data: [DONE]\n\n",
    );
    let second = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"got it\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, server) = serve_sse(&[first, second]);
    let home = Home::new("read-limit-oversized");
    home.auth(r#"{"mock":{"key":"k"}}"#);
    let ws = std::env::temp_dir().join(format!("e-ws-limit-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    let mut body = String::from("first line\n");
    body.push_str(&"x".repeat(200 * 1024));
    std::fs::write(ws.join("f.txt"), body).unwrap();
    std::env::set_current_dir(&ws).unwrap();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("read the first line".into(), "sys".into());

    let mut tool_ok = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::ToolEnd { outcome, .. } => tool_ok = !outcome.is_error(),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(
        tool_ok,
        "read with limit:1 errored on a later oversized line"
    );
    let captured = server.join().unwrap();
    assert!(
        captured[1].contains("first line"),
        "first line not returned"
    );
    assert!(
        !captured[1].contains(&"x".repeat(50)),
        "oversized line leaked into the requested window"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// `grep` must keep scanning after an oversized line instead of silently
/// dropping every later match.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn grep_continues_past_an_oversized_line() {
    let _lock = env_lock();
    let first = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
        "\"function\":{\"name\":\"grep\",\"arguments\":\"{\\\"pattern\\\":\\\"needle\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":10}}\n\n",
        "data: [DONE]\n\n",
    );
    let second = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"searched\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, server) = serve_sse(&[first, second]);
    let home = Home::new("grep-oversized");
    home.auth(r#"{"mock":{"key":"k"}}"#);
    let ws = std::env::temp_dir().join(format!("e-ws-grep-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    let mut body = String::new();
    body.push_str(&"y".repeat(200 * 1024)); // oversized line 1
    body.push('\n');
    body.push_str("needle in line two\n");
    std::fs::write(ws.join("f.txt"), body).unwrap();
    std::env::set_current_dir(&ws).unwrap();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("grep needle".into(), "sys".into());

    let mut tool_ok = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::ToolEnd { outcome, .. } => tool_ok = !outcome.is_error(),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(tool_ok, "grep errored");
    let captured = server.join().unwrap();
    assert!(
        captured[1].contains("needle"),
        "match after an oversized line was dropped"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

/// `write` must still overwrite an existing valid file whose lines exceed the
/// 64 KiB read cap — counting lines for the summary must not impose that cap.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn write_overwrites_a_file_with_long_lines() {
    let _lock = env_lock();
    let first = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
        "\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"path\\\":\\\"f.txt\\\",\\\"content\\\":\\\"replaced\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":10}}\n\n",
        "data: [DONE]\n\n",
    );
    let second = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"wrote\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, server) = serve_sse(&[first, second]);
    let home = Home::new("write-longline");
    home.auth(r#"{"mock":{"key":"k"}}"#);
    let ws = std::env::temp_dir().join(format!("e-ws-write-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    // A valid UTF-8 file whose single line exceeds the 64 KiB read cap.
    std::fs::write(ws.join("f.txt"), format!("{}\n", "z".repeat(200 * 1024))).unwrap();
    std::env::set_current_dir(&ws).unwrap();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("overwrite f.txt".into(), "sys".into());

    let mut tool_ok = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::ToolEnd { outcome, .. } => tool_ok = !outcome.is_error(),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(
        tool_ok,
        "write rejected an existing file with a long line (count_text_lines cap)"
    );
    let captured = server.join().unwrap();
    assert!(
        captured[1].contains("replaced") || captured[1].contains("wrote"),
        "write did not report success"
    );
    let _ = std::fs::remove_dir_all(&ws);
}
