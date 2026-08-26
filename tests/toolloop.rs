//! The agent tool loop against a mock provider that asks for a tool once,
//! then replies. Proves: tool_call parsed, tool executed for real, result fed
//! back, a second request made, plain reply ends the turn — all on the one
//! session stream, TurnStart first, TurnEnd last.

mod common;

use common::{env_lock, serve_sse, test_model, Home};
use e::core::agent::{Agent, SessionEvent};
use e::core::providers::catalog::Api;

// The env lock is deliberately held across awaits: E_HOME and cwd must stay
// ours for the whole test, and each #[tokio::test] runs on its own runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn agent_runs_a_tool_then_replies() {
    let _lock = env_lock();
    // First request → ask to read hello.txt; second → a plain reply.
    let first = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
        "\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"hello.txt\\\"}\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let second = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"the file has two lines\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, server) = serve_sse(&[first, second]);
    let home = Home::new("toolloop");
    home.auth(r#"{"mock":{"key":"k"}}"#);

    // A workspace with one file for the tool to read.
    let ws = std::env::temp_dir().join(format!("e-ws-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("hello.txt"), "line one\nline two\n").unwrap();
    std::env::set_current_dir(&ws).unwrap();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("how many lines in hello.txt?".into(), "sys".into());

    let mut order = Vec::new();
    let mut reply = String::new();
    let mut tool_ok = false;
    let mut assembly: Vec<u64> = Vec::new();
    let mut assembly_before_batch = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::TurnStart => order.push("start"),
            SessionEvent::ToolCallAssembly { bytes } => {
                assembly_before_batch |= !order.contains(&"batch");
                assembly.push(bytes);
            }
            SessionEvent::ToolBatchStart { calls } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].running, "Reading");
                assert_eq!(calls[0].completed, "Read");
                assert_eq!(calls[0].target, "hello.txt");
                order.push("batch");
            }
            SessionEvent::ToolStart { .. } => order.push("tool"),
            SessionEvent::ToolEnd { outcome, .. } => {
                tool_ok = !outcome.is_error();
            }
            SessionEvent::TextDelta(d) => reply.push_str(&d),
            SessionEvent::TurnEnd { .. } => {
                order.push("end");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(order, vec!["start", "batch", "tool", "end"]);
    assert!(tool_ok, "the read tool errored");
    assert_eq!(reply, "the file has two lines");
    // Argument streaming is visible while it happens — cumulative byte
    // counts, arriving before the batch opens, so a long tool call never
    // looks like a stalled turn.
    assert!(!assembly.is_empty(), "no ToolCallAssembly liveness events");
    assert!(assembly.windows(2).all(|w| w[0] < w[1]) || assembly.len() == 1);
    assert!(assembly_before_batch, "liveness must precede the batch");
    // The second request carried the tool result back to the model.
    let captured = server.join().unwrap();
    assert!(
        captured[1].contains("line one"),
        "tool result not sent back to model"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn tool_batches_run_concurrently_and_commit_in_source_order() {
    let _lock = env_lock();
    // First request → two calls: a slow command and a fast read; second → a
    // plain reply.
    let first = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"sleep 0.4\\\"}\"}},",
        "{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"quick.txt\\\"}\"}}",
        "]}}]}\n\n",
        "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let second = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"both done\"}}]}\n\n",
        "data: [DONE]\n\n"
    );
    let (port, server) = serve_sse(&[first, second]);
    let home = Home::new("concurrent");
    home.auth(r#"{"mock":{"key":"k"}}"#);

    let ws = std::env::temp_dir().join(format!("e-ws-c-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("quick.txt"), "quick body\n").unwrap();
    std::env::set_current_dir(&ws).unwrap();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("run both".into(), "sys".into());

    let mut started = Vec::new();
    let mut ended_before_last_start = false;
    let mut ends = 0usize;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::ToolStart { id } => {
                if ends > 0 {
                    ended_before_last_start = true;
                }
                started.push(id);
            }
            SessionEvent::ToolEnd { .. } => ends += 1,
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    // Concurrency: the second call starts before the first one finishes.
    assert_eq!(started.len(), 2);
    assert!(!ended_before_last_start, "calls ran serially");
    // The sent history carries both results in assistant source order.
    let captured = server.join().unwrap();
    let sent = &captured[1];
    let c1 = sent.find("call_id\":\"c1\"").expect("c1 result sent");
    let c2 = sent.find("call_id\":\"c2\"").expect("c2 result sent");
    assert!(c1 < c2, "results must commit in source order");
    let _ = std::fs::remove_dir_all(&ws);
}
