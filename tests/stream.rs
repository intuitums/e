//! Wire-level test: a canned SSE server proves the completions client parses
//! deltas, reasoning, usage, and [DONE] correctly — no network, no keys.

use std::io::{Read, Write};
use std::net::TcpListener;

use e::core::model::{Api, Model};
use e::core::provider::{stream, ChatMessage, Event, Request, SseSplitter};

#[test]
fn sse_splitter_handles_fragmentation_and_crlf() {
    let mut s = SseSplitter::new();
    assert!(s.feed("data: {\"a\":").is_empty());
    assert_eq!(s.feed("1}\n\n"), vec!["{\"a\":1}"]);
    assert_eq!(s.feed("data: x\r\n\r\ndata: y\n\n"), vec!["x", "y"]);
    // multi-line data field joins with newline
    assert_eq!(s.feed("data: a\ndata: b\n\n"), vec!["a\nb"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn completions_stream_parses_deltas_and_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let home = std::env::temp_dir().join(format!("e-test-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\
             \"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
            "data: [DONE]\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(response.as_bytes()).unwrap();
    });

    let request = Request {
        model: Model {
            provider: "mock".into(),
            id: "m".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            api: Api::Completions,
            efforts: &[],
            context_window: 200_000,
        },
        system: "sys".into(),
        messages: vec![ChatMessage { role: "user".into(), content: "hi".into() }],
        effort: None,
    };
    let (mut rx, _handle) = stream(request);

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut usage = None;
    let mut done = false;
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta(d) => text.push_str(&d),
            Event::ReasoningDelta(d) => reasoning.push_str(&d),
            Event::Usage { input, output, cache_read } => usage = Some((input, output, cache_read)),
            Event::Done => {
                done = true;
                break;
            }
            Event::Error(e) => panic!("stream error: {e}"),
        }
    }
    assert!(done);
    assert_eq!(text, "Hello");
    assert_eq!(reasoning, "hmm");
    assert_eq!(usage, Some((12, 3, 4)));
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_folds_provider_events_into_one_session_stream() {
    use e::core::agent::{Agent, SessionEvent};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-test-agent-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(response.as_bytes()).unwrap();
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
    agent.prompt("hi".into(), "sys".into());

    // The contract: TurnStart first, TurnEnd last, exactly once each.
    let mut kinds = Vec::new();
    while let Some(event) = rx.recv().await {
        let done = matches!(event, SessionEvent::TurnEnd { .. });
        kinds.push(match event {
            SessionEvent::TurnStart => "start",
            SessionEvent::TextDelta(_) => "text",
            SessionEvent::Usage { .. } => "usage",
            SessionEvent::TurnEnd { .. } => "end",
            SessionEvent::ReasoningDelta(_) => "reasoning",
            SessionEvent::Error(_) => "error",
        });
        if done {
            break;
        }
    }
    assert_eq!(kinds.first(), Some(&"start"));
    assert_eq!(kinds.last(), Some(&"end"));
    assert!(kinds.contains(&"text") && kinds.contains(&"usage"));
    assert!(!kinds.contains(&"error"));
}
