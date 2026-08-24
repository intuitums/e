//! The agent tool loop against a mock provider that asks for a tool once,
//! then replies. Proves: tool_call parsed, tool executed for real, result fed
//! back, a second request made, plain reply ends the turn — all on the one
//! session stream, TurnStart first, TurnEnd last.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;

// Both tests replace E_HOME and the process cwd; serialize them.
static ENV_LOCK: Mutex<()> = Mutex::new(());

use e::core::agent::{Agent, SessionEvent};
use e::core::providers::catalog::{Api, Model};

fn sse(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

// The env lock is deliberately held across awaits: E_HOME and cwd must stay
// ours for the whole test, and each #[tokio::test] runs on its own runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn agent_runs_a_tool_then_replies() {
    let _lock = ENV_LOCK.lock().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-toolloop-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    // A workspace with one file for the tool to read.
    let ws = std::env::temp_dir().join(format!("e-ws-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("hello.txt"), "line one\nline two\n").unwrap();
    std::env::set_current_dir(&ws).unwrap();

    std::thread::spawn(move || {
        // First request → ask to read hello.txt.
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = [0u8; 16384];
        let _ = a.read(&mut buf);
        let first = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
            "\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"hello.txt\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        a.write_all(sse(first).as_bytes()).unwrap();

        // Second request (now carrying the tool result) → a plain reply.
        let (mut b, _) = listener.accept().unwrap();
        let mut buf2 = vec![0u8; 65536];
        let n = b.read(&mut buf2).unwrap();
        let sent = String::from_utf8_lossy(&buf2[..n]);
        // The tool result must have been fed back.
        assert!(
            sent.contains("line one"),
            "tool result not sent back to model"
        );
        let second = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"the file has two lines\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        b.write_all(sse(second).as_bytes()).unwrap();
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
    let (mut agent, mut rx) = Agent::new(model);
    agent.submit("how many lines in hello.txt?".into(), "sys".into());

    let mut order = Vec::new();
    let mut reply = String::new();
    let mut tool_ok = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::TurnStart => order.push("start"),
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
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn tool_batches_run_concurrently_and_commit_in_source_order() {
    let _lock = ENV_LOCK.lock().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-concurrent-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    let ws = std::env::temp_dir().join(format!("e-ws-c-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("quick.txt"), "quick body\n").unwrap();
    std::env::set_current_dir(&ws).unwrap();

    std::thread::spawn(move || {
        // First request → two calls: a slow command and a fast read.
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = [0u8; 16384];
        let _ = a.read(&mut buf);
        let first = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"command\\\":\\\"sleep 0.4\\\"}\"}},",
            "{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"quick.txt\\\"}\"}}",
            "]}}]}\n\n",
            "data: {\"choices\":[{\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        a.write_all(sse(first).as_bytes()).unwrap();

        // Second request → plain reply; the sent history must carry both
        // results in assistant source order (c1 before c2).
        let (mut b, _) = listener.accept().unwrap();
        let mut buf2 = vec![0u8; 65536];
        let n = b.read(&mut buf2).unwrap();
        let sent = String::from_utf8_lossy(&buf2[..n]);
        let c1 = sent.find("call_id\":\"c1\"").expect("c1 result sent");
        let c2 = sent.find("call_id\":\"c2\"").expect("c2 result sent");
        assert!(c1 < c2, "results must commit in source order");
        let second = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"both done\"}}]}

",
            "data: [DONE]

"
        );
        b.write_all(sse(second).as_bytes()).unwrap();
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
    let (mut agent, mut rx) = Agent::new(model);
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
}
