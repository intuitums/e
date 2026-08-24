//! The Gemini dialect against a mock server. Pins: request shape
//! (x-goog-api-key, systemInstruction, function declarations, thinking
//! level, thought-signature replay on tool loops), and event parsing —
//! text, thought summaries, function calls with captured signatures, usage,
//! and the terminal finishReason with no [DONE] sentinel.

use std::io::{Read, Write};
use std::net::TcpListener;

use e::core::providers::catalog::{Api, Model};
use e::core::providers::{self, ChatMessage, Event, FinishReason, Request};

// E_HOME is process-global; concurrent tests would race each other's homes.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn sse(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn model(port: u16) -> Model {
    Model {
        provider: "google".into(),
        id: "gemini-test".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Google,
        efforts: vec!["low".into(), "high".into()],
        thinking: e::core::providers::catalog::Thinking::Manual,
        context_window: 1_048_576,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn google_stream_round_trip() {
    let _lock = ENV_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-google-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"google":{"key":"g-test"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"planning\",\"thought\":true}]}}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello \"}]}}]}\n\n",
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"world\"},{\"functionCall\":{\"name\":\"read\",\"args\":{\"path\":\"a.txt\"}},\"thoughtSignature\":\"sig-1\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":90,\"candidatesTokenCount\":20,\"thoughtsTokenCount\":5,\"cachedContentTokenCount\":30}}\n\n",
        );
        a.write_all(sse(body).as_bytes()).unwrap();
        sent
    });

    let request = Request {
        model: model(port),
        system: "be helpful".into(),
        messages: vec![
            ChatMessage::user("read a.txt"),
            // A foreign-dialect id (an OpenAI-style call id from a
            // mid-session model switch): the functionResponse name must come
            // from the call it refers to, never from the id's spelling.
            ChatMessage::assistant(
                "",
                vec![providers::ToolCall {
                    id: "call_abc123".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"old.txt\"}".into(),
                    signature: Some("sig-0".into()),
                }],
            ),
            ChatMessage::tool_result("call_abc123", "old contents"),
        ],
        effort: Some("high".into()),
        tools: vec![serde_json::json!({
            "type": "function",
            "function": {"name": "read", "description": "read a file",
                          "parameters": {"type": "object", "properties": {}}}
        })],
    };

    let (mut rx, _handle) = providers::stream(request);
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    let mut usage = None;
    let mut finish = None;
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta(d) => text.push_str(&d),
            Event::ReasoningDelta(d) => reasoning.push_str(&d),
            Event::ToolCall(c) => calls.push(c),
            Event::Usage {
                input,
                output,
                cache_read,
            } => usage = Some((input, output, cache_read)),
            Event::Error(err) => panic!("stream errored: {}", err.message),
            Event::Done(end) => {
                finish = Some(end.finish);
                break;
            }
            Event::ReasoningItem(_) => {}
        }
    }

    assert_eq!(text, "hello world");
    assert_eq!(reasoning, "planning");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].name, "read");
    // Synthesized ids are session-unique (a process-wide counter), so a
    // multi-step tool loop never replays duplicate ids into history.
    assert!(calls[0].id.starts_with("read-"), "id: {}", calls[0].id);
    assert_eq!(calls[0].signature.as_deref(), Some("sig-1"));
    let args: serde_json::Value = serde_json::from_str(&calls[0].arguments).unwrap();
    assert_eq!(args["path"], "a.txt");
    // Thought tokens count as output.
    assert_eq!(usage, Some((90, 25, 30)));
    assert_eq!(finish, Some(FinishReason::Normal));

    // The request wire shape.
    let sent = server.join().unwrap();
    assert!(sent.contains("POST /models/gemini-test:streamGenerateContent?alt=sse"));
    assert!(sent.contains("x-goog-api-key: g-test"));
    let body_json = sent.split("\r\n\r\n").nth(1).unwrap();
    let value: serde_json::Value = serde_json::from_str(body_json).unwrap();
    assert_eq!(value["systemInstruction"]["parts"][0]["text"], "be helpful");
    assert_eq!(
        value["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "high"
    );
    assert_eq!(value["tools"][0]["functionDeclarations"][0]["name"], "read");
    // The prior turn's function call replays with its thought signature, and
    // the tool result files under the function's name.
    let contents = value["contents"].as_array().unwrap();
    assert_eq!(contents[1]["role"], "model");
    assert_eq!(contents[1]["parts"][0]["functionCall"]["name"], "read");
    assert_eq!(contents[1]["parts"][0]["thoughtSignature"], "sig-0");
    assert_eq!(contents[2]["parts"][0]["functionResponse"]["name"], "read");

    let _ = std::fs::remove_dir_all(&home);
}

/// MAX_TOKENS from Gemini is a truncated reply delivered as HTTP success;
/// the dialect must name it instead of finishing normally.
#[tokio::test(flavor = "multi_thread")]
async fn google_max_tokens_maps_to_length() {
    let _lock = ENV_LOCK.lock().await;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-google-len-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"google":{"key":"g-test"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let _ = a.read(&mut buf);
        let body = concat!(
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"cut\"}]}}]}\n\n",
            "data: {\"candidates\":[{\"finishReason\":\"MAX_TOKENS\"}]}\n\n",
        );
        a.write_all(sse(body).as_bytes()).unwrap();
    });

    let request = Request {
        model: model(port),
        system: "sys".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: None,
        tools: Vec::new(),
    };
    let (mut rx, _handle) = providers::stream(request);
    let mut finish = None;
    while let Some(event) = rx.recv().await {
        match event {
            Event::Done(end) => {
                finish = Some(end.finish);
                break;
            }
            Event::Error(err) => panic!("stream errored: {}", err.message),
            _ => {}
        }
    }
    assert_eq!(finish, Some(FinishReason::Length));
    let _ = std::fs::remove_dir_all(&home);
}
