//! The Responses dialect against a mock server. Pins the wire shape the
//! first live codex turn broke on: tools must be FLAT ({type, name, …}),
//! not chat-completions-nested — plus the item shapes and event parsing.

use std::io::{Read, Write};
use std::net::TcpListener;

use e::core::provider::catalog::{Api, Model};
use e::core::provider::{self, ChatMessage, Event, Request};

fn sse(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn responses_tools_are_flat_on_the_wire() {
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
            efforts: &["low", "medium", "high"],
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
            Event::Error { message, .. } => panic!("stream errored: {message}"),
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
