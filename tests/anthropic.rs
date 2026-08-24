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
                    signature: None,
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
    // Anthropic's prompt fields are disjoint: input reports the inclusive
    // total (100 uncached + 40 cache-read), cache_read the cached subset.
    assert_eq!(usage, Some((140, 25, 40)));

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

/// Signed thinking blocks round-trip a tool loop: the stream's thinking +
/// signature becomes a replayable reasoning item, and the follow-up request
/// carries it verbatim at the head of the assistant turn, before tool_use.
#[tokio::test(flavor = "multi_thread")]
async fn signed_thinking_blocks_are_captured_and_replayed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-anthropic-think-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("auth.json"),
        r#"{"anthropic":{"key":"sk-ant-test"}}"#,
    )
    .unwrap();
    std::env::set_var("E_HOME", &home);

    // Capture: a stream with a signed thinking block and a tool call.
    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let _ = a.read(&mut buf).unwrap();
        let body = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"let me look\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-abc\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
            "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        a.write_all(sse(body).as_bytes()).unwrap();

        // Replay: the follow-up request after the tool result.
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = vec![0u8; 65536];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        );
        a.write_all(sse(body).as_bytes()).unwrap();
        sent
    });

    let model = Model {
        provider: "anthropic".into(),
        id: "claude-test".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Anthropic,
        efforts: vec!["high".into()],
        thinking: e::core::providers::catalog::Thinking::Adaptive,
        context_window: 200_000,
    };

    // First request: collect the reasoning item and the call.
    let request = Request {
        model: model.clone(),
        system: "sys".into(),
        messages: vec![ChatMessage::user("read a.txt")],
        effort: Some("high".into()),
        tools: Vec::new(),
    };
    let (mut rx, _handle) = providers::stream(request);
    let mut items = Vec::new();
    let mut calls = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            Event::ReasoningItem(item) => items.push(item),
            Event::ToolCall(c) => calls.push(c),
            Event::Error(err) => panic!("stream errored: {}", err.message),
            Event::Done(_) => break,
            _ => {}
        }
    }
    assert_eq!(items.len(), 1, "one signed thinking block captured");
    let block: serde_json::Value = serde_json::from_str(&items[0]).unwrap();
    assert_eq!(block["type"], "thinking");
    assert_eq!(block["thinking"], "let me look");
    assert_eq!(block["signature"], "sig-abc");
    assert_eq!(calls.len(), 1);

    // Second request: history as the agent commits it — reasoning item,
    // then the assistant turn with the call, then the tool result.
    let request = Request {
        model,
        system: "sys".into(),
        messages: vec![
            ChatMessage::user("read a.txt"),
            ChatMessage {
                role: "reasoning".into(),
                content: items.remove(0),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_meta: None,
            },
            ChatMessage::assistant("", calls.clone()),
            ChatMessage::tool_result("tu_1", "contents"),
        ],
        effort: Some("high".into()),
        tools: Vec::new(),
    };
    let (mut rx, _handle) = providers::stream(request);
    while let Some(event) = rx.recv().await {
        match event {
            Event::Error(err) => panic!("replay errored: {}", err.message),
            Event::Done(_) => break,
            _ => {}
        }
    }

    let sent = server.join().unwrap();
    let body_json = sent.split("\r\n\r\n").nth(1).unwrap();
    let value: serde_json::Value = serde_json::from_str(body_json).unwrap();
    let assistant = &value["messages"][1];
    assert_eq!(assistant["role"], "assistant");
    let content = assistant["content"].as_array().unwrap();
    assert_eq!(
        content[0]["type"], "thinking",
        "thinking must lead the assistant turn"
    );
    assert_eq!(content[0]["signature"], "sig-abc");
    assert_eq!(content[0]["thinking"], "let me look");
    assert_eq!(content[1]["type"], "tool_use");

    let _ = std::fs::remove_dir_all(&home);
}
