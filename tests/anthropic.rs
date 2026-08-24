//! The Anthropic Messages dialect against a mock server. Pins: request shape
//! (x-api-key, anthropic-version, system block, tool schema conversion,
//! thinking budget), and event parsing — text deltas, thinking deltas,
//! streamed tool_use input JSON, usage totals.

use std::io::{Read, Write};
use std::net::TcpListener;

use e::core::providers::catalog::{Api, Model};
use e::core::providers::{self, ChatMessage, Event, Request};

fn sse(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn anthropic_stream_round_trip() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-anthropic-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("auth.json"),
        r#"{"anthropic":{"key":"sk-ant-test"}}"#,
    )
    .unwrap();
    std::env::set_var("E_HOME", &home);

    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = concat!(
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":40}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.txt\\\"}\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":25}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        a.write_all(sse(body).as_bytes()).unwrap();
        sent
    });

    let request = Request {
        model: Model {
            provider: "anthropic".into(),
            id: "claude-test".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            api: Api::Anthropic,
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            thinking: e::core::providers::catalog::Thinking::Manual,
            context_window: 200_000,
        },
        system: "be helpful".into(),
        messages: vec![
            ChatMessage::user("read a.txt"),
            ChatMessage::assistant(
                "",
                vec![providers::ToolCall {
                    id: "tu_0".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"old.txt\"}".into(),
                }],
            ),
            ChatMessage::tool_result("tu_0", "old contents"),
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
            Event::Done(_) => break,
            Event::ReasoningItem(_) => {}
        }
    }

    assert_eq!(text, "hello world");
    assert_eq!(reasoning, "hmm");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "tu_1");
    assert_eq!(calls[0].arguments, "{\"path\":\"a.txt\"}");
    assert_eq!(usage, Some((100, 25, 40)));

    // The request wire shape.
    let sent = server.join().unwrap();
    assert!(sent.contains("x-api-key: sk-ant-test"));
    assert!(sent.contains("anthropic-version: 2023-06-01"));
    assert!(
        sent.contains("\"input_schema\""),
        "tool schema not converted"
    );
    assert!(
        sent.contains("\"tool_use_id\":\"tu_0\""),
        "tool result not in anthropic shape"
    );
    assert!(
        sent.contains("\"budget_tokens\":24000"),
        "high effort budget missing"
    );
    assert!(sent.contains("\"max_tokens\""));

    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test(flavor = "multi_thread")]
async fn adaptive_models_take_effort_through_output_config_not_budget_tokens() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-anthropic-adaptive-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("auth.json"),
        r#"{"anthropic":{"key":"sk-ant-test"}}"#,
    )
    .unwrap();
    std::env::set_var("E_HOME", &home);

    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":1}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        a.write_all(response.as_bytes()).unwrap();
        sent
    });

    let request = Request {
        model: Model {
            provider: "anthropic".into(),
            id: "claude-test".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            api: Api::Anthropic,
            efforts: vec!["low".into(), "medium".into(), "high".into()],
            thinking: e::core::providers::catalog::Thinking::Adaptive,
            context_window: 200_000,
        },
        system: "be helpful".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: Some("high".into()),
        tools: Vec::new(),
    };

    let (mut rx, _handle) = providers::stream(request);
    while let Some(event) = rx.recv().await {
        match event {
            Event::Error(err) => panic!("stream errored: {}", err.message),
            Event::Done(_) => break,
            _ => {}
        }
    }

    let sent = server.join().unwrap();
    assert!(
        sent.contains("\"thinking\":{\"type\":\"adaptive\"}"),
        "adaptive shape missing"
    );
    assert!(
        sent.contains("\"output_config\":{\"effort\":\"high\"}"),
        "effort not carried through output_config"
    );
    assert!(
        !sent.contains("budget_tokens"),
        "legacy budget_tokens must not reach an adaptive model"
    );

    let _ = std::fs::remove_dir_all(&home);
}
