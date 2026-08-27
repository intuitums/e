//! Opt-in paid provider canary. The normal suite is deterministic and uses
//! local mock servers; this ignored test verifies one configured deployment
//! all the way through auth, streaming, a tool call, replay, and final text.
//!
//! Run deliberately:
//!   E_LIVE_MODEL=provider/model cargo test --test live_provider -- --ignored

use e::core::providers::catalog;
use e::core::providers::{self, ChatMessage, Event, Request, ToolCall};

async fn response(request: Request) -> (String, Vec<ToolCall>, usize, usize, usize) {
    let (mut events, _handle) = providers::stream(request);
    let mut text = String::new();
    let mut calls = Vec::new();
    let mut starts = 0usize;
    let mut deltas = 0usize;
    let mut ends = 0usize;
    while let Some(event) = events.recv().await {
        match event {
            Event::TextDelta(delta) => text.push_str(&delta),
            Event::ToolCallStart { .. } => starts += 1,
            Event::ToolArgumentsDelta { .. } => deltas += 1,
            Event::ToolCallEnd { .. } => ends += 1,
            Event::ToolCall(call) => calls.push(call),
            Event::Error(error) => panic!("live provider failed: {}", error.message),
            Event::Done(_) => break,
            _ => {}
        }
    }
    (text, calls, starts, deltas, ends)
}

#[tokio::test]
#[ignore = "uses configured credentials and incurs a real provider request"]
async fn configured_provider_streams_a_complete_tool_loop() {
    let slug = std::env::var("E_LIVE_MODEL")
        .expect("set E_LIVE_MODEL=provider/model before running the paid canary");
    let model = catalog::catalog()
        .into_iter()
        .find(|model| catalog::slug(model) == slug)
        .unwrap_or_else(|| panic!("E_LIVE_MODEL `{slug}` is not in the catalog"));
    let tool = serde_json::json!({
        "type": "function",
        "function": {
            "name": "live_echo",
            "description": "Return a supplied marker. You must call this when the user asks for a live canary.",
            "parameters": {
                "type": "object",
                "properties": {"marker": {"type": "string"}},
                "required": ["marker"],
                "additionalProperties": false
            }
        }
    });
    let first = Request {
        model: model.clone(),
        system: "This is a provider conformance canary. Follow the tool instruction exactly.".into(),
        messages: vec![ChatMessage::user(
            "Run the live canary by calling live_echo once with marker E_LIVE_OK. Do not answer before using the tool.",
        )],
        effort: None,
        tools: vec![tool.clone()],
    };
    let (_prefix, calls, starts, deltas, ends) =
        tokio::time::timeout(std::time::Duration::from_secs(120), response(first))
            .await
            .expect("live provider timed out");
    assert!(
        !calls.is_empty(),
        "provider did not issue the required tool call"
    );
    assert_eq!(starts, calls.len(), "every call needs one semantic start");
    assert_eq!(ends, calls.len(), "every call needs one semantic end");
    assert!(deltas > 0, "provider emitted no semantic argument progress");

    let mut history = vec![ChatMessage::user("Run the live canary.")];
    history.push(ChatMessage::assistant("", calls.clone()));
    for call in &calls {
        history.push(ChatMessage::tool_result(&call.id, "E_LIVE_OK"));
    }
    let second = Request {
        model,
        system: "After the tool result, answer with the marker it returned.".into(),
        messages: history,
        effort: None,
        tools: vec![tool],
    };
    let (text, _, _, _, _) =
        tokio::time::timeout(std::time::Duration::from_secs(120), response(second))
            .await
            .expect("live follow-up timed out");
    assert!(
        text.contains("E_LIVE_OK"),
        "unexpected final response: {text}"
    );
}
