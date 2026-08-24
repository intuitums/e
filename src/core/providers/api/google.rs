//! The Gemini dialect: `POST {base}/models/{model}:streamGenerateContent`
//! with `alt=sse`, `x-goog-api-key` auth. Candidate chunks stream text,
//! thought summaries, and function calls; thought signatures on function
//! calls are captured and replayed verbatim — the API requires them back on
//! the next request of a tool loop. Effort maps to
//! `thinkingConfig.thinkingLevel`. The stream has no `[DONE]` sentinel: the
//! chunk carrying `finishReason` is terminal.

use serde_json::json;
use tokio::sync::mpsc;

use crate::core::auth::login;
use crate::core::providers::{
    http, require_success, send_request, Event, FinishReason, ProviderError, Request, SseStream,
    StreamEnd, ToolCall,
};

/// Gemini function calls carry no ids; e synthesizes `{index}:{name}` so the
/// tool loop can address them, and this recovers the name a
/// `functionResponse` must be filed under.
fn call_name(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, name)| name)
}

pub async fn run(request: &Request, tx: &mpsc::Sender<Event>) -> Result<StreamEnd, ProviderError> {
    let key = login::access_token(request.model.provider.as_str())
        .await
        .map_err(ProviderError::auth)?;

    // History → contents. Assistant turns replay their function calls with
    // thought signatures; tool results ride user turns as functionResponse
    // parts, consecutive results joining one turn to mirror the batch.
    let mut contents: Vec<serde_json::Value> = Vec::new();
    for m in &request.messages {
        match m.role.as_str() {
            "assistant" => {
                let mut parts = Vec::new();
                if !m.content.is_empty() {
                    parts.push(json!({"text": m.content}));
                }
                for call in &m.tool_calls {
                    let args: serde_json::Value =
                        serde_json::from_str(&call.arguments).unwrap_or(json!({}));
                    let mut part = json!({"functionCall": {"name": call.name, "args": args}});
                    if let Some(signature) = &call.signature {
                        part["thoughtSignature"] = json!(signature);
                    }
                    parts.push(part);
                }
                if !parts.is_empty() {
                    contents.push(json!({"role": "model", "parts": parts}));
                }
            }
            "tool" => {
                let name = m.tool_call_id.as_deref().map(call_name).unwrap_or_default();
                let part = json!({"functionResponse": {
                    "name": name,
                    "response": {"output": m.content},
                }});
                match contents.last_mut() {
                    Some(last)
                        if last["role"] == "user"
                            && last["parts"][0]["functionResponse"].is_object() =>
                    {
                        last["parts"].as_array_mut().unwrap().push(part);
                    }
                    _ => contents.push(json!({"role": "user", "parts": [part]})),
                }
            }
            // Responses-dialect reasoning items mean nothing here.
            "reasoning" => {}
            _ => contents.push(json!({"role": "user", "parts": [{"text": m.content}]})),
        }
    }

    let mut body = json!({
        "systemInstruction": {"parts": [{"text": request.system}]},
        "contents": contents,
    });
    if !request.tools.is_empty() {
        // OpenAI-shaped schemas → Gemini function declarations.
        let declarations: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t["function"]["name"],
                    "description": t["function"]["description"],
                    "parameters": t["function"]["parameters"],
                })
            })
            .collect();
        body["tools"] = json!([{"functionDeclarations": declarations}]);
    }
    if let Some(effort) = &request.effort {
        body["generationConfig"] = json!({
            "thinkingConfig": {"thinkingLevel": effort, "includeThoughts": true},
        });
    }

    let response = require_success(
        send_request(
            http()
                .post(format!(
                    "{}/models/{}:streamGenerateContent?alt=sse",
                    request.model.base_url, request.model.id
                ))
                .header("x-goog-api-key", &key)
                .header("accept", "text/event-stream")
                .json(&body),
        )
        .await?,
    )
    .await?;

    let mut sse = SseStream::new(response.bytes_stream());
    let mut usage: Option<(u64, u64, u64)> = None;
    let mut call_index = 0usize;
    loop {
        let payload = sse.next().await?;
        let value: serde_json::Value = match serde_json::from_str(&payload) {
            Ok(v) => v,
            Err(_) => {
                sse.malformed();
                continue;
            }
        };
        if let Some(meta) = value.get("usageMetadata").filter(|u| u.is_object()) {
            // Cumulative — the latest frame wins. Thought tokens are output.
            usage = Some((
                meta["promptTokenCount"].as_u64().unwrap_or(0),
                meta["candidatesTokenCount"].as_u64().unwrap_or(0)
                    + meta["thoughtsTokenCount"].as_u64().unwrap_or(0),
                meta["cachedContentTokenCount"].as_u64().unwrap_or(0),
            ));
        }
        if let Some(reason) = value["promptFeedback"]["blockReason"].as_str() {
            return Err(ProviderError::rejected(format!("prompt blocked: {reason}")));
        }
        let candidate = &value["candidates"][0];
        if let Some(parts) = candidate["content"]["parts"].as_array() {
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    if part["thought"].as_bool().unwrap_or(false) {
                        let _ = tx.send(Event::ReasoningDelta(text.into())).await;
                    } else if !text.is_empty() {
                        let _ = tx.send(Event::TextDelta(text.into())).await;
                    }
                }
                if part["functionCall"].is_object() {
                    let name = part["functionCall"]["name"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    let call = ToolCall {
                        id: format!("{call_index}:{name}"),
                        name,
                        arguments: part["functionCall"]["args"].to_string(),
                        signature: part["thoughtSignature"].as_str().map(String::from),
                    };
                    call_index += 1;
                    if !call.name.is_empty() {
                        let _ = tx.send(Event::ToolCall(call)).await;
                    }
                }
            }
        }
        if let Some(reason) = candidate["finishReason"].as_str() {
            if let Some((input, output, cache_read)) = usage {
                let _ = tx
                    .send(Event::Usage {
                        input,
                        output,
                        cache_read,
                    })
                    .await;
            }
            let finish = match reason {
                "STOP" => FinishReason::Normal,
                "MAX_TOKENS" => FinishReason::Length,
                "SAFETY" | "PROHIBITED_CONTENT" | "BLOCKLIST" | "SPII" | "IMAGE_SAFETY" => {
                    FinishReason::ContentFilter
                }
                other => FinishReason::Other(other.to_string()),
            };
            return Ok(sse.end(finish));
        }
    }
}
