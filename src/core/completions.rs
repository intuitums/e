//! The chat-completions dialect: `POST {base}/chat/completions`, Bearer key,
//! SSE deltas at `choices[0].delta`, a final usage frame requested via
//! `stream_options`, `[DONE]` sentinel.

use futures::StreamExt;
use serde_json::json;
use tokio::sync::mpsc;

use crate::core::auth::{self, Credential};
use crate::core::provider::{http, Event, Request, SseSplitter};

pub async fn run(request: &Request, tx: &mpsc::Sender<Event>) -> Result<(), String> {
    let auth = auth::load();
    let key = match auth.get(request.model.provider.as_str()) {
        Some(Credential::ApiKey { key }) => key.clone(),
        Some(Credential::OAuth { access, .. }) => access.clone(),
        None => {
            return Err(format!(
                "no credentials for {} — run `e auth {}`",
                request.model.provider, request.model.provider
            ))
        }
    };

    let mut messages = vec![json!({"role": "system", "content": request.system})];
    for m in &request.messages {
        messages.push(json!({"role": m.role, "content": m.content}));
    }
    let body = json!({
        "model": request.model.id,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });

    let response = http()
        .post(format!("{}/chat/completions", request.model.base_url))
        .bearer_auth(&key)
        .header("accept", "text/event-stream")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("{status}: {}", text.chars().take(300).collect::<String>()));
    }

    let mut splitter = SseSplitter::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
        for payload in splitter.feed(&String::from_utf8_lossy(&chunk)) {
            if payload == "[DONE]" {
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
                // Some backends stream reasoning under a sibling field.
                for key in ["reasoning_content", "reasoning"] {
                    if let Some(text) = delta.get(key).and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            let _ = tx.send(Event::ReasoningDelta(text.into())).await;
                        }
                    }
                }
            }
            if let Some(usage) = value.get("usage").filter(|u| !u.is_null()) {
                let cached = usage["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
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
    Ok(())
}
