//! The Gemini dialect: `POST {base}/models/{model}:streamGenerateContent`
//! with `alt=sse`, `x-goog-api-key` auth. Candidate chunks stream text,
//! thought summaries, and function calls; thought signatures on function
//! calls are captured and replayed verbatim — the API requires them back on
//! the next request of a tool loop. A signature verifies against the thought
//! text that preceded it, so that text is committed as a "reasoning" history
//! item too and replayed ahead of the function call it signs (mirroring how
//! the Anthropic dialect replays signed thinking blocks). Effort maps to
//! `thinkingConfig.thinkingLevel`. The stream has no `[DONE]` sentinel: the
//! chunk carrying `finishReason` is terminal.

use serde_json::json;
use tokio::sync::mpsc;

use crate::core::auth::login;
use crate::core::providers::{
    http, require_success, send_request, Event, FinishReason, ProviderError, Request, SseStream,
    StreamEnd, ToolCall,
};

/// Older Gemini responses carried no wire id. A UUID fallback stays unique
/// across resumed processes and later model switches; Gemini 3's own id wins
/// whenever present and is returned verbatim with the function result.
fn synthesize_call_id(name: &str) -> String {
    format!("{name}-{}", uuid::Uuid::new_v4())
}

pub async fn run(request: &Request, tx: &mpsc::Sender<Event>) -> Result<StreamEnd, ProviderError> {
    let key = login::access_token(request.model.provider.as_str())
        .await
        .map_err(ProviderError::auth)?;

    // History → contents. Assistant turns replay their function calls with
    // thought signatures; tool results ride user turns as functionResponse
    // parts, consecutive results joining one turn to mirror the batch.
    // functionResponse.name comes from the assistant call the id refers to —
    // ids may be another dialect's (a mid-session model switch), so the name
    // is never derived from the id's spelling. Signed thought text committed
    // as "reasoning" messages replays verbatim at the head of the assistant
    // turn it preceded — the API validates a function call's signature
    // against that thought content.
    let mut contents: Vec<serde_json::Value> = Vec::new();
    let mut call_names: std::collections::HashMap<String, String> = Default::default();
    let mut pending_thoughts: Vec<serde_json::Value> = Vec::new();
    for m in &request.messages {
        match m.role.as_str() {
            "assistant" => {
                let mut parts = std::mem::take(&mut pending_thoughts);
                if !m.content.is_empty() {
                    parts.push(json!({"text": m.content}));
                }
                for call in &m.tool_calls {
                    call_names.insert(call.id.clone(), call.name.clone());
                    let args: serde_json::Value =
                        serde_json::from_str(&call.arguments).unwrap_or(json!({}));
                    let mut part = json!({"functionCall": {
                        "id": call.id, "name": call.name, "args": args
                    }});
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
                let name = m
                    .tool_call_id
                    .as_deref()
                    .and_then(|id| call_names.get(id))
                    .cloned()
                    .unwrap_or_default();
                let part = json!({"functionResponse": {
                    "id": m.tool_call_id.clone().unwrap_or_default(),
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
            "reasoning" => {
                // Only this dialect's own items; items from other dialects
                // (Anthropic thinking blocks, Responses reasoning JSON) mean
                // nothing here.
                if let Ok(item) = serde_json::from_str::<serde_json::Value>(&m.content) {
                    if item["type"].as_str() == Some("gemini_thought") {
                        if let Some(text) = item["text"].as_str() {
                            pending_thoughts.push(json!({"text": text, "thought": true}));
                        }
                    }
                }
            }
            _ => {
                // A turn boundary without an assistant message orphans any
                // buffered thought text; replaying it elsewhere would fail
                // the signature check.
                pending_thoughts.clear();
                contents.push(json!({"role": "user", "parts": [{"text": m.content}]}));
            }
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
                        if !text.is_empty() {
                            let item = json!({"type": "gemini_thought", "text": text});
                            let _ = tx.send(Event::ReasoningItem(item.to_string())).await;
                        }
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
                        id: part["functionCall"]["id"]
                            .as_str()
                            .filter(|id| !id.is_empty())
                            .map(String::from)
                            .unwrap_or_else(|| synthesize_call_id(&name)),
                        name,
                        arguments: part["functionCall"]["args"].to_string(),
                        signature: part["thoughtSignature"].as_str().map(String::from),
                    };
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
