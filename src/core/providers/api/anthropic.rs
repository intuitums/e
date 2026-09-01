//! The Anthropic Messages dialect: `{base}/v1/messages`, `x-api-key` auth.
//!
//! Same event grammar as the reference client: SSE data payloads carry a
//! `type` — content_block_start/delta/stop stream text, thinking, and
//! tool_use input JSON; message_start/message_delta carry usage. Effort maps
//! to an extended-thinking token budget (manual) or `output_config.effort`
//! (adaptive), per the model's declared thinking mode.

use serde_json::json;
use tokio::sync::mpsc;

use crate::core::providers::catalog::Thinking;
use crate::core::providers::runtime::Authorization;
use crate::core::providers::{
    http, require_success, send_request, Event, FailureCause, FinishReason, ProviderError, Request,
    SseStream, StreamEnd, ToolCall,
};

const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Default output ceiling per reply, for models that don't declare their own
/// (lower) `max_output` — a per-model catalog fact, since it varies by
/// model (e.g. claude-haiku-4-5's ~8k against this 32k default).
const MAX_TOKENS: u64 = 32_000;

/// Extended-thinking budgets per effort; each stays under the default
/// MAX_TOKENS, but a smaller declared `max_output` clamps this further
/// below (see the `max_tokens - 1024` clamp at the call site).
fn thinking_budget(effort: &str) -> u64 {
    match effort {
        "low" => 4_000,
        "high" => 24_000,
        _ => 12_000,
    }
}

pub async fn run(
    request: &Request,
    authorization: &Authorization,
    tx: &mpsc::Sender<Event>,
) -> Result<StreamEnd, ProviderError> {
    // History → content blocks. Tool results ride user turns. Signed
    // thinking blocks committed as "reasoning" messages replay verbatim at
    // the head of the assistant turn they preceded — the API requires them
    // back, complete with signatures, when continuing a tool loop.
    let mut messages: Vec<serde_json::Value> = Vec::new();
    let mut pending_thinking: Vec<serde_json::Value> = Vec::new();
    for m in &request.messages {
        match m.role.as_str() {
            "assistant" => {
                let mut content = std::mem::take(&mut pending_thinking);
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
            "reasoning" => {
                // Only this dialect's own blocks; items from other dialects
                // (Responses reasoning JSON) mean nothing here.
                if let Ok(block) = serde_json::from_str::<serde_json::Value>(&m.content) {
                    if matches!(
                        block["type"].as_str(),
                        Some("thinking") | Some("redacted_thinking")
                    ) {
                        pending_thinking.push(block);
                    }
                }
            }
            _ => {
                // A turn boundary without an assistant message orphans any
                // buffered thinking; replaying it elsewhere would fail the
                // signature check.
                pending_thinking.clear();
                let mut content = Vec::new();
                if !m.content.is_empty() {
                    content.push(json!({"type": "text", "text": m.content}));
                }
                content.extend(m.images.iter().map(|image| {
                    json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": image.media_type,
                            "data": image.data,
                        }
                    })
                }));
                messages.push(json!({"role": "user", "content": content}));
            }
        }
    }

    // Moving cache breakpoint on the last cacheable content block: the
    // system block alone caches only the prefix ahead of the conversation,
    // so every step of a tool loop re-billed the whole history uncached.
    // With the tail marked, each request extends the previous step's cached
    // prefix instead. Thinking blocks can't carry cache_control; skip them.
    if let Some(last) = messages.last_mut() {
        if let Some(blocks) = last["content"].as_array_mut() {
            if let Some(block) = blocks.iter_mut().rev().find(|b| {
                !matches!(
                    b["type"].as_str(),
                    Some("thinking") | Some("redacted_thinking")
                )
            }) {
                block["cache_control"] = json!({"type": "ephemeral"});
            }
        }
    }

    // The output ceiling must fit both the model's own max output (some
    // models allow far less than the 32k default — claude-haiku-4-5 caps
    // at ~8k) and its window: a fixed default against a small declared
    // window would be rejected before generation either way.
    let ceiling = request.model.max_output.unwrap_or(MAX_TOKENS);
    let max_tokens = ceiling.min((request.model.context_window / 2).max(1024));
    let mut body = json!({
        "model": request.model.id,
        "max_tokens": max_tokens,
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
                // The budget must stay strictly under max_tokens with real
                // headroom or the request is rejected. On a small declared
                // window, max_tokens itself can be too small to leave that
                // headroom above a sane minimum budget — there enabling
                // thinking at all would only produce an invalid request, so
                // skip it and let the reply generate without it.
                if max_tokens >= 2048 {
                    body["thinking"] = json!({
                        "type": "enabled",
                        "budget_tokens": thinking_budget(effort).min(max_tokens - 1024),
                    });
                }
            }
        }
    }

    let response = require_success(
        send_request(
            http()?
                .post(format!("{}/v1/messages", request.model.base_url))
                .header("x-api-key", &authorization.bearer)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("accept", "text/event-stream")
                .json(&body),
        )
        .await?,
    )
    .await?;

    let mut sse = SseStream::new(response.bytes_stream());
    // Tool input JSON streams in fragments per content block index.
    let mut open_tools: std::collections::BTreeMap<usize, ToolCall> = Default::default();
    // A thinking block accumulates text and its opaque signature; on stop it
    // becomes a replayable reasoning item.
    let mut open_thinking: Option<(String, String)> = None;
    let mut input_tokens = 0u64;
    let mut cache_read = 0u64;
    let mut output_tokens = 0u64;
    let mut finish = FinishReason::Normal;

    loop {
        let payload = sse.next().await?;
        {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
                sse.malformed();
                continue;
            };
            match value["type"].as_str().unwrap_or("") {
                "message_start" => {
                    // Anthropic's prompt-side fields are disjoint; the Usage
                    // contract wants the inclusive total in `input`.
                    let usage = &value["message"]["usage"];
                    cache_read = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                    input_tokens = usage["input_tokens"].as_u64().unwrap_or(0)
                        + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                        + cache_read;
                }
                "content_block_start" => {
                    let index = value["index"].as_u64().unwrap_or(0) as usize;
                    let block = &value["content_block"];
                    match block["type"].as_str().unwrap_or("") {
                        "tool_use" => {
                            // Anthropic names a tool_use block up front, so we
                            // can refuse a nameless one here rather than let it
                            // dangle: a call we never open needs no ToolCallEnd,
                            // keeping the start/end lifecycle the consumer
                            // relies on balanced. The other dialects can't do
                            // this — their name streams in with the arguments
                            // deltas — so they gate at the close instead.
                            let name = block["name"].as_str().unwrap_or("").to_string();
                            if name.is_empty() {
                                // Skip the block; the stream keeps going.
                                continue;
                            }
                            open_tools.insert(
                                index,
                                ToolCall {
                                    id: block["id"].as_str().unwrap_or("").to_string(),
                                    name,
                                    arguments: String::new(),
                                    signature: None,
                                },
                            );
                            let _ = tx
                                .send(Event::ToolCallStart {
                                    key: index.to_string(),
                                })
                                .await;
                        }
                        "thinking" => open_thinking = Some((String::new(), String::new())),
                        // Arrives complete, no deltas; preserved verbatim.
                        "redacted_thinking" => {
                            let _ = tx.send(Event::ReasoningItem(block.to_string())).await;
                        }
                        _ => {}
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
                            if let Some((thinking, _)) = &mut open_thinking {
                                thinking.push_str(text);
                            }
                            let _ = tx.send(Event::ReasoningDelta(text.to_string())).await;
                        }
                    }
                    "signature_delta" => {
                        if let Some((_, signature)) = &mut open_thinking {
                            signature.push_str(value["delta"]["signature"].as_str().unwrap_or(""));
                        }
                    }
                    "input_json_delta" => {
                        let index = value["index"].as_u64().unwrap_or(0) as usize;
                        if let Some(call) = open_tools.get_mut(&index) {
                            let partial = value["delta"]["partial_json"].as_str().unwrap_or("");
                            call.arguments.push_str(partial);
                            if !partial.is_empty() {
                                let _ = tx
                                    .send(Event::ToolArgumentsDelta {
                                        key: index.to_string(),
                                        delta: partial.to_string(),
                                    })
                                    .await;
                            }
                        }
                    }
                    _ => {}
                },
                "content_block_stop" => {
                    let index = value["index"].as_u64().unwrap_or(0) as usize;
                    if let Some(mut call) = open_tools.remove(&index) {
                        // Only named blocks reach here: nameless tool_use
                        // blocks are refused at content_block_start, so there
                        // is nothing to gate on.
                        if call.arguments.is_empty() {
                            call.arguments = "{}".into();
                        }
                        let _ = tx
                            .send(Event::ToolCallEnd {
                                key: index.to_string(),
                            })
                            .await;
                        let _ = tx.send(Event::ToolCall(call)).await;
                    }
                    if let Some((thinking, signature)) = open_thinking.take() {
                        // Only a signed block is replayable; an unsigned one
                        // has nothing the API demands back.
                        if !signature.is_empty() {
                            let block = json!({
                                "type": "thinking",
                                "thinking": thinking,
                                "signature": signature,
                            });
                            let _ = tx.send(Event::ReasoningItem(block.to_string())).await;
                        }
                    }
                }
                "message_delta" => {
                    if let Some(out) = value["usage"]["output_tokens"].as_u64() {
                        output_tokens = out;
                    }
                    if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                        finish = match reason {
                            "end_turn" | "stop_sequence" => FinishReason::Normal,
                            "tool_use" => FinishReason::ToolCalls,
                            "max_tokens" => FinishReason::Length,
                            "refusal" => FinishReason::Refusal,
                            other => FinishReason::Other(other.to_string()),
                        };
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
                    return Ok(sse.end(finish));
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
                    let text_cause = crate::core::providers::classify_text(&message);
                    let cause = if text_cause == Some(FailureCause::QuotaExhausted) {
                        FailureCause::QuotaExhausted
                    } else {
                        match value["error"]["type"].as_str().unwrap_or("") {
                            "overloaded_error" | "api_error" => FailureCause::ProviderUnavailable,
                            "rate_limit_error" => FailureCause::RateLimited,
                            "authentication_error" | "permission_error" => FailureCause::Auth,
                            // Unknown types still get the message classifier.
                            _ => text_cause.unwrap_or(FailureCause::Rejected),
                        }
                    };
                    return Err(ProviderError::frame(message, cause));
                }
                _ => {}
            }
        }
    }
}
