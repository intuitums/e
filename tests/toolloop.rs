//! The agent tool loop against a mock provider that asks for a tool once,
//! then replies. Proves: tool_call parsed, tool executed for real, result fed
//! back, a second request made, plain reply ends the turn — all on the one
//! session stream, TurnStart first, TurnEnd last.

use std::io::{Read, Write};
use std::net::TcpListener;

use e::core::agent::{Agent, SessionEvent};
use e::core::model::{Api, Model};

fn sse(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_runs_a_tool_then_replies() {
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
        assert!(sent.contains("line one"), "tool result not sent back to model");
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
        efforts: &[],
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
            SessionEvent::ToolStart { verb, .. } => order.push(if verb == "Read" { "tool" } else { "tool?" }),
            SessionEvent::ToolEnd { is_error, .. } => { tool_ok = !is_error; }
            SessionEvent::TextDelta(d) => reply.push_str(&d),
            SessionEvent::TurnEnd { .. } => { order.push("end"); break; }
            _ => {}
        }
    }
    assert_eq!(order, vec!["start", "tool", "end"]);
    assert!(tool_ok, "the read tool errored");
    assert_eq!(reply, "the file has two lines");
}
