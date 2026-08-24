//! The Anthropic Messages dialect: `{base}/v1/messages`, `x-api-key` auth.
//!
//! Same event grammar as the reference client: SSE data payloads carry a
//! `type` — content_block_start/delta/stop stream text, thinking, and
//! tool_use input JSON; message_start/message_delta carry usage. Effort maps
//! to an extended-thinking token budget (manual) or `output_config.effort`
//! (adaptive), per the model's declared thinking mode.

use serde_json::json;
use tokio::sync::mpsc;

use crate::core::auth::login;
use crate::core::providers::catalog::Thinking;
use crate::core::providers::{
    http, next_sse_chunk, retry_after_seconds, send_request, Event, FailureCause, ProviderError,
    Request, SseSplitter, ToolCall,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Output ceiling per reply; every cataloged model allows at least this.
const MAX_TOKENS: u64 = 32_000;

/// Extended-thinking budgets per effort; each stays under MAX_TOKENS.
fn thinking_budget(effort: &str) -> u64 {
    match effort {
        "low" => 4_000,
        "high" => 24_000,
        _ => 12_000,
    }
}

type RunError = ProviderError;

pub async fn run(request: &Request, tx: &mpsc::Sender<Event>) -> Result<(), RunError> {
    let key = login::access_token(request.model.provider.as_str())
        .await
        .map_err(ProviderError::auth)?;

    // History → content blocks. Tool results ride user turns.
    let mut messages: Vec<serde_json::Value> = Vec::new();
    for m in &request.messages {
        match m.role.as_str() {
            "assistant" => {
                let mut content = Vec::new();
                if !m.content.is_empty() {
                    content.push(json!({"type": "text", "text": m.content}));
                }
                for call in &m.tool_calls {
                    let input: serde_json::Value =
                        serde_json::from_str(&call.arguments).unwrap_or(json!({}));
                    content.push(json!({
                        "type": "tool_use", "id": call.id, "name": call.name, "input": input,
                    }));
                }
                if !content.is_empty() {
                    messages.push(json!({"role": "assistant", "content": content}));
                }
            }
            "tool" => messages.push(json!({
                "role": "user",
                "content": [{"type": "tool_result",
                             "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                             "content": m.content}],
            })),
            // Responses-dialect reasoning items mean nothing here.
            "reasoning" => {}
            _ => messages.push(json!({
                "role": "user",
                "content": [{"type": "text", "text": m.content}],
            })),
        }
    }

    let mut body = json!({
        "model": request.model.id,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": [{"type": "text", "text": request.system,
                    "cache_control": {"type": "ephemeral"}}],
        "messages": messages,
    });
    if !request.tools.is_empty() {
        // OpenAI-shaped schemas → Anthropic tool declarations.
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t["function"]["name"],
                    "description": t["function"]["description"],
                    "input_schema": t["function"]["parameters"],
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }
    if let Some(effort) = &request.effort {
        // Adaptive-thinking models (Claude 4.7+) reject the legacy manual
        // shape with a 400 before generation; they take the effort through
        // output_config instead. Manual models keep the token budget.
        match request.model.thinking {
            Thinking::Adaptive => {
                body["thinking"] = json!({"type": "adaptive"});
                body["output_config"] = json!({"effort": effort});
            }
            Thinking::Manual => {
                body["thinking"] = json!({
                    "type": "enabled",
                    "budget_tokens": thinking_budget(effort),
                });
            }
        }
    }

    let response = send_request(
        http()
            .post(format!("{}/v1/messages", request.model.base_url))
            .header("x-api-key", &key)
            .header("anthropic-version", ANTHROPIC_VERSION)
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

    let mut splitter = SseSplitter::new();
    let mut stream = response.bytes_stream();
    // Tool input JSON streams in fragments per content block index.
    let mut open_tool: Option<(ToolCall, usize)> = None;
    let mut input_tokens = 0u64;
    let mut cache_read = 0u64;
    let mut output_tokens = 0u64;

    while let Some(chunk) = next_sse_chunk(&mut stream).await? {
        for payload in splitter.feed_bytes(&chunk) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                continue;
            };
            match value["type"].as_str().unwrap_or("") {
                "message_start" => {
                    let usage = &value["message"]["usage"];
                    input_tokens = usage["input_tokens"].as_u64().unwrap_or(0);
                    cache_read = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                }
                "content_block_start" => {
                    let index = value["index"].as_u64().unwrap_or(0) as usize;
                    let block = &value["content_block"];
                    if block["type"] == "tool_use" {
                        open_tool = Some((
                            ToolCall {
                                id: block["id"].as_str().unwrap_or("").to_string(),
                                name: block["name"].as_str().unwrap_or("").to_string(),
                                arguments: String::new(),
                            },
                            index,
                        ));
                    }
                }
                "content_block_delta" => match value["delta"]["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        if let Some(text) = value["delta"]["text"].as_str() {
                            let _ = tx.send(Event::TextDelta(text.to_string())).await;
                        }
                    }
                    "thinking_delta" => {
                        if let Some(text) = value["delta"]["thinking"].as_str() {
                            let _ = tx.send(Event::ReasoningDelta(text.to_string())).await;
                        }
                    }
                    "input_json_delta" => {
                        if let Some((call, _)) = &mut open_tool {
                            call.arguments
                                .push_str(value["delta"]["partial_json"].as_str().unwrap_or(""));
                        }
                    }
                    _ => {}
                },
                "content_block_stop" => {
                    if let Some((mut call, _)) = open_tool.take() {
                        if call.arguments.is_empty() {
                            call.arguments = "{}".into();
                        }
                        let _ = tx.send(Event::ToolCall(call)).await;
                    }
                }
                "message_delta" => {
                    if let Some(out) = value["usage"]["output_tokens"].as_u64() {
                        output_tokens = out;
                    }
                }
                "message_stop" => {
                    let _ = tx
                        .send(Event::Usage {
                            input: input_tokens,
                            output: output_tokens,
                            cache_read,
                        })
                        .await;
                    return Ok(());
                }
                "error" => {
                    let message = value["error"]["message"]
                        .as_str()
                        .unwrap_or("unknown provider error")
                        .to_string();
                    // A mid-stream error frame carries its own type — the
                    // API's way of saying "overloaded" or "rate limited"
                    // once a connection is already open, distinct from an
                    // HTTP status.
                    let cause = match value["error"]["type"].as_str().unwrap_or("") {
                        "overloaded_error" | "api_error" => FailureCause::ProviderUnavailable,
                        "rate_limit_error" => FailureCause::RateLimited,
                        "authentication_error" | "permission_error" => FailureCause::Auth,
                        _ => FailureCause::Rejected,
                    };
                    return Err(ProviderError::frame(message, cause));
                }
                _ => {}
            }
        }
    }
    Err(ProviderError::stalled("stream ended unexpectedly"))
}
