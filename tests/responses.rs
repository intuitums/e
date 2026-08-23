//! The Responses dialect against a mock server. Pins the wire shape the
//! first live codex turn broke on: tools must be FLAT ({type, name, …}),
//! not chat-completions-nested — plus the item shapes and event parsing.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;

// E_HOME is process-global; serialize the tests that set it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

use e::core::provider::catalog::{Api, Model};
use e::core::provider::{self, ChatMessage, Event, Request};

fn sse(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

// The env lock is deliberately held across awaits: E_HOME must stay ours and
// each #[tokio::test] runs on its own runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn responses_tools_are_flat_on_the_wire() {
    let _lock = ENV_LOCK.lock().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-responses-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"openai":{"key":"sk-test"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":0}}}}\n\n",
        );
        a.write_all(sse(body).as_bytes()).unwrap();
        sent
    });

    let request = Request {
        model: Model {
            provider: "openai".into(),
            id: "gpt-test".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            api: Api::Responses,
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            context_window: 400_000,
        },
        system: "sys".into(),
        messages: vec![ChatMessage::user("hello")],
        effort: Some("medium".into()),
        tools: vec![serde_json::json!({
            "type": "function",
            "function": {"name": "read", "description": "read a file",
                          "parameters": {"type": "object", "properties": {}}}
        })],
    };

    let (mut rx, _handle) = provider::stream(request);
    let mut text = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta(d) => text.push_str(&d),
            Event::Error(err) => panic!("stream errored: {}", err.message),
            Event::Done => break,
            _ => {}
        }
    }
    assert_eq!(text, "hi");

    let sent = server.join().unwrap();
    let body_json: serde_json::Value =
        serde_json::from_str(sent.split("\r\n\r\n").nth(1).expect("request has a body"))
            .expect("request body is JSON");

    // The regression that 400'd the first live turn: tools[0].name at top level.
    assert_eq!(body_json["tools"][0]["type"], "function");
    assert_eq!(body_json["tools"][0]["name"], "read");
    assert!(
        body_json["tools"][0].get("function").is_none(),
        "nested shape leaked"
    );
    assert!(body_json["tools"][0]["parameters"].is_object());
    // Batches run concurrently, matching the advertised capability.
    assert_eq!(body_json["parallel_tool_calls"], true);

    // API-key deployment: the standard mount, no codex account header.
    let request_line = sent.lines().next().unwrap();
    assert!(
        request_line.starts_with("POST /responses "),
        "wrong mount: {request_line}"
    );
    assert!(!sent.contains("chatgpt-account-id"));
    assert!(
        sent.contains("authorization: Bearer sk-test")
            || sent.contains("Authorization: Bearer sk-test")
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn reasoning_items_replay_ahead_of_their_calls() {
    use e::core::agent::{Agent, SessionEvent};

    let _lock = ENV_LOCK.lock().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-reasoning-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"openai":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);
    let ws = std::env::temp_dir().join(format!("e-reasoning-ws-{port}"));
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("f.txt"), "data\n").unwrap();
    std::env::set_current_dir(ws.canonicalize().unwrap()).unwrap();

    let server = std::thread::spawn(move || {
        // First request: a reasoning item precedes a function call.
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let _ = a.read(&mut buf).unwrap();
        let first = concat!(
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"encrypted_content\":\"SECRETBLOB\",\"summary\":[]}}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"f.txt\\\"}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        );
        a.write_all(sse(first).as_bytes()).unwrap();

        // Second request must replay the reasoning item verbatim, before the call.
        let (mut b, _) = listener.accept().unwrap();
        let mut buf2 = vec![0u8; 262144];
        let n = b.read(&mut buf2).unwrap();
        let sent = String::from_utf8_lossy(&buf2[..n]).to_string();
        let second = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        );
        b.write_all(sse(second).as_bytes()).unwrap();
        sent
    });

    let model = Model {
        provider: "openai".into(),
        id: "gpt-test".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Responses,
        efforts: vec!["low".into(), "medium".into(), "high".into()],
        context_window: 400_000,
    };
    let (mut agent, mut rx) = Agent::new(model);
    agent.submit("read f.txt".into(), "sys".into());
    let mut ended = false;
    while let Some(event) = rx.recv().await {
        if let SessionEvent::TurnEnd { .. } = event {
            ended = true;
            break;
        }
    }
    assert!(ended);

    let sent = server.join().unwrap();
    let body: serde_json::Value =
        serde_json::from_str(sent.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let input = body["input"].as_array().unwrap();
    let kinds: Vec<&str> = input.iter().filter_map(|i| i["type"].as_str()).collect();
    let reasoning_pos = kinds.iter().position(|k| *k == "reasoning");
    let call_pos = kinds.iter().position(|k| *k == "function_call");
    assert!(
        reasoning_pos.is_some(),
        "reasoning item not replayed: {kinds:?}"
    );
    assert!(
        reasoning_pos.unwrap() < call_pos.expect("function_call replayed"),
        "reasoning must precede its call: {kinds:?}"
    );
    let item = &input[reasoning_pos.unwrap()];
    assert_eq!(item["encrypted_content"], "SECRETBLOB", "item not verbatim");
    assert_eq!(item["id"], "rs_1");

    let _ = std::fs::remove_dir_all(&home);
}
