//! The chat-completions dialect: `POST {base}/chat/completions`, Bearer key,
//! SSE deltas at `choices[0].delta`, streamed tool-call argument fragments
//! accumulated by index, a final usage frame, `[DONE]` sentinel.

use futures::StreamExt;
use serde_json::json;
use std::collections::BTreeMap;
use tokio::sync::mpsc;

use crate::core::auth::{self, Credential};
use crate::core::provider::{http, Event, Request, SseSplitter, ToolCall};

type RunError = (String, bool); // (message, delivered)

pub async fn run(request: &Request, tx: &mpsc::Sender<Event>) -> Result<(), RunError> {
    let auth = auth::load();
    let key = match auth.get(request.model.provider.as_str()) {
        Some(Credential::ApiKey { key }) => key.clone(),
        Some(Credential::OAuth {
            access,
            refresh,
            expires,
            ..
        }) => {
            // The only OAuth provider on this dialect is xAI; refresh lazily
            // when the access token is within a minute of expiry.
            if auth::now_ms() + 60_000 < *expires {
                access.clone()
            } else {
                let fresh = crate::core::auth::login::xai_refresh(refresh)
                    .await
                    .map_err(|e| (e, false))?;
                let _ = auth::set(&request.model.provider, fresh.clone());
                match fresh {
                    Credential::OAuth { access, .. } => access,
                    Credential::ApiKey { key } => key,
                }
            }
        }
        None => {
            return Err((
                format!("no credentials for {} — run /login", request.model.provider),
                false,
            ))
        }
    };

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

    let response = http()
        .post(format!("{}/chat/completions", request.model.base_url))
        .bearer_auth(&key)
        .header("accept", "text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|e| (format!("request failed: {e}"), false))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err((
            format!("{status}: {}", text.chars().take(300).collect::<String>()),
            true,
        ));
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
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| (format!("stream error: {e}"), true))?;
        for payload in splitter.feed(&String::from_utf8_lossy(&chunk)) {
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
    flush(&mut pending, tx).await;
    Ok(())
}
