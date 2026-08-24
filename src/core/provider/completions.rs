//! The chat-completions dialect: `POST {base}/chat/completions`, Bearer key,
//! SSE deltas at `choices[0].delta`, streamed tool-call argument fragments
//! accumulated by index, a final usage frame, `[DONE]` sentinel.

use serde_json::json;
use std::collections::BTreeMap;
use tokio::sync::mpsc;

use crate::core::auth::login;
use crate::core::provider::{
    http, next_sse_chunk, retry_after_seconds, send_request, Event, ProviderError, Request,
    SseSplitter, ToolCall,
};

type RunError = ProviderError;

pub async fn run(request: &Request, tx: &mpsc::Sender<Event>) -> Result<(), RunError> {
    let key = login::access_token(request.model.provider.as_str())
        .await
        .map_err(ProviderError::auth)?;

    let mut messages = vec![json!({"role": "system", "content": request.system})];
    for m in &request.messages {
        match m.role.as_str() {
            "assistant" if !m.tool_calls.is_empty() => {
                let calls: Vec<_> = m
                    .tool_calls
                    .iter()
                    .map(|c| {
                        json!({"id": c.id, "type": "function",
                               "function": {"name": c.name, "arguments": c.arguments}})
                    })
                    .collect();
                let mut msg = json!({"role": "assistant", "tool_calls": calls});
                if !m.content.is_empty() {
                    msg["content"] = json!(m.content);
                }
                messages.push(msg);
            }
            "tool" => messages.push(json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id.clone().unwrap_or_default(),
                "content": m.content,
            })),
            // Responses-dialect reasoning items mean nothing here.
            "reasoning" => {}
            role => messages.push(json!({"role": role, "content": m.content})),
        }
    }

    let mut body = json!({
        "model": request.model.id,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools);
    }

    let response = send_request(
        http()
            .post(format!("{}/chat/completions", request.model.base_url))
            .bearer_auth(&key)
            .header("accept", "text/event-stream")
            .json(&body),
    )
    .await?;

    if !response.status().is_success() {
        let status = response.status();
        let retry_after = retry_after_seconds(&response);
        let text = response.text().await.unwrap_or_default();
        return Err(ProviderError::from_status(status, &text).with_retry_after(retry_after));
    }

    // Tool-call fragments accumulate per stream index until [DONE].
    let mut pending: BTreeMap<u64, ToolCall> = BTreeMap::new();
    let flush = |pending: &mut BTreeMap<u64, ToolCall>, tx: &mpsc::Sender<Event>| {
        let calls: Vec<ToolCall> = std::mem::take(pending).into_values().collect();
        let tx = tx.clone();
        async move {
            for call in calls {
                if !call.name.is_empty() {
                    let _ = tx.send(Event::ToolCall(call)).await;
                }
            }
        }
    };

    let mut splitter = SseSplitter::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = next_sse_chunk(&mut stream).await? {
        for payload in splitter.feed_bytes(&chunk) {
            if payload == "[DONE]" {
                flush(&mut pending, tx).await;
                return Ok(());
            }
            let value: serde_json::Value = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(delta) = value["choices"][0]["delta"].as_object() {
                if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        let _ = tx.send(Event::TextDelta(text.into())).await;
                    }
                }
                for key in ["reasoning_content", "reasoning"] {
                    if let Some(text) = delta.get(key).and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            let _ = tx.send(Event::ReasoningDelta(text.into())).await;
                        }
                    }
                }
                if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                    for fragment in calls {
                        let index = fragment["index"].as_u64().unwrap_or(0);
                        let entry = pending.entry(index).or_insert_with(|| ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        });
                        if let Some(id) = fragment["id"].as_str() {
                            entry.id = id.into();
                        }
                        if let Some(name) = fragment["function"]["name"].as_str() {
                            entry.name.push_str(name);
                        }
                        if let Some(args) = fragment["function"]["arguments"].as_str() {
                            entry.arguments.push_str(args);
                        }
                    }
                }
            }
            // A finish_reason of tool_calls closes the accumulation early.
            if value["choices"][0]["finish_reason"].as_str() == Some("tool_calls") {
                flush(&mut pending, tx).await;
            }
            if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
                let cached = usage["prompt_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0);
                let _ = tx
                    .send(Event::Usage {
                        input: usage["prompt_tokens"].as_u64().unwrap_or(0),
                        output: usage["completion_tokens"].as_u64().unwrap_or(0),
                        cache_read: cached,
                    })
                    .await;
            }
        }
    }
    // EOF without [DONE] is a broken stream, not a successful empty reply.
    Err(ProviderError::stalled("stream ended unexpectedly"))
}
