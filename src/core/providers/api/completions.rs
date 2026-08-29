//! The chat-completions dialect: `POST {base}/chat/completions`, Bearer key,
//! SSE deltas at `choices[0].delta`, streamed tool-call argument fragments
//! accumulated by index, a final usage frame, `[DONE]` sentinel.

use serde_json::json;
use std::collections::BTreeMap;
use tokio::sync::mpsc;

use crate::core::providers::runtime::Authorization;
use crate::core::providers::{
    http, require_success, retry_after_seconds, send_request, Event, FinishReason, ProviderError,
    Request, SseStream, StreamEnd, ToolCall,
};

pub async fn run(
    request: &Request,
    authorization: &Authorization,
    tx: &mpsc::Sender<Event>,
) -> Result<StreamEnd, ProviderError> {
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
            role => {
                if m.images.is_empty() {
                    messages.push(json!({"role": role, "content": m.content}));
                } else {
                    let mut content = Vec::new();
                    if !m.content.is_empty() {
                        content.push(json!({"type": "text", "text": m.content}));
                    }
                    content.extend(m.images.iter().map(|image| {
                        json!({
                            "type": "image_url",
                            "image_url": {"url": image.data_url()},
                        })
                    }));
                    messages.push(json!({"role": role, "content": content}));
                }
            }
        }
    }

    let mut body = json!({
        "model": request.model.id,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    // Reasoning effort rides the OpenAI-standard field; only sent when the
    // model declares a knob, so gateways that never heard of it never see
    // it. `off` has no wire encoding here — absence is the closest thing.
    if let Some(effort) = request.effort.as_deref().filter(|e| *e != "off") {
        body["reasoning_effort"] = json!(effort);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools);
    }

    let send = |body: &serde_json::Value| {
        send_request(
            http()
                .post(format!("{}/chat/completions", request.model.base_url))
                .bearer_auth(&authorization.bearer)
                .header("accept", "text/event-stream")
                .json(body),
        )
    };
    let first = send(&body).await?;
    let response = if matches!(first.status().as_u16(), 400 | 422) {
        let status = first.status();
        let retry_after = retry_after_seconds(&first);
        let text = first.text().await.unwrap_or_default();
        // Usage is useful but not essential. Strict OpenAI-compatible
        // gateways commonly identify this optional field in their validation
        // error; a rejected request generated no content, so one retry without
        // it is safe and avoids a provider-specific compatibility table.
        if text.to_ascii_lowercase().contains("stream_options") {
            body.as_object_mut().unwrap().remove("stream_options");
            require_success(send(&body).await?).await?
        } else {
            return Err(ProviderError::from_status(status, &text).with_retry_after(retry_after));
        }
    } else {
        require_success(first).await?
    };

    // Tool-call fragments accumulate per stream index until [DONE].
    let mut pending: BTreeMap<u64, ToolCall> = BTreeMap::new();
    let flush = |pending: &mut BTreeMap<u64, ToolCall>, tx: &mpsc::Sender<Event>| {
        let calls: Vec<(u64, ToolCall)> = std::mem::take(pending).into_iter().collect();
        let tx = tx.clone();
        async move {
            for (index, call) in calls {
                if !call.name.is_empty() {
                    let _ = tx
                        .send(Event::ToolCallEnd {
                            key: index.to_string(),
                        })
                        .await;
                    let _ = tx.send(Event::ToolCall(call)).await;
                }
            }
        }
    };

    let mut sse = SseStream::new(response.bytes_stream());
    let mut finish = FinishReason::Normal;
    loop {
        let payload = sse.next().await?;
        {
            if payload == "[DONE]" {
                flush(&mut pending, tx).await;
                return Ok(sse.end(finish));
            }
            let value: serde_json::Value = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(_) => {
                    sse.malformed();
                    continue;
                }
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
                        if !pending.contains_key(&index) {
                            let _ = tx
                                .send(Event::ToolCallStart {
                                    key: index.to_string(),
                                })
                                .await;
                        }
                        let entry = pending.entry(index).or_insert_with(|| ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                            signature: None,
                        });
                        if let Some(id) = fragment["id"].as_str() {
                            entry.id = id.into();
                        }
                        if let Some(name) = fragment["function"]["name"].as_str() {
                            entry.name.push_str(name);
                        }
                        if let Some(args) = fragment["function"]["arguments"].as_str() {
                            entry.arguments.push_str(args);
                            if !args.is_empty() {
                                let _ = tx
                                    .send(Event::ToolArgumentsDelta {
                                        key: index.to_string(),
                                        delta: args.to_string(),
                                    })
                                    .await;
                            }
                        }
                    }
                }
            }
            if let Some(reason) = value["choices"][0]["finish_reason"].as_str() {
                finish = match reason {
                    "stop" => FinishReason::Normal,
                    "tool_calls" => FinishReason::ToolCalls,
                    "length" => FinishReason::Length,
                    "content_filter" => FinishReason::ContentFilter,
                    other => FinishReason::Other(other.to_string()),
                };
                // A finish_reason of tool_calls closes the accumulation early.
                if finish == FinishReason::ToolCalls {
                    flush(&mut pending, tx).await;
                }
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
}
