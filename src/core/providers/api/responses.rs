//! The Responses-API dialect.
//!
//! One dialect, more than one deployment: the ChatGPT backend mounts it at
//! `{base}/codex/responses` behind a subscription OAuth (bearer + account-id
//! header, lazy refresh); other providers serve the same event grammar at
//! `{base}/responses` behind a plain key. The provider id — not this module —
//! names the account type. OAuth refresh lives in `auth::login`.

use serde_json::json;
use tokio::sync::mpsc;

use crate::core::auth::{self, login, Credential};
use crate::core::providers::{
    http, require_success, send_request, Event, FailureCause, FinishReason, ProviderError, Request,
    SseStream, StreamEnd, ToolCall,
};

pub async fn run(request: &Request, tx: &mpsc::Sender<Event>) -> Result<StreamEnd, ProviderError> {
    // A plain API key means the standard platform mount (`{base}/responses`,
    // bearer only); OAuth means the ChatGPT backend (`{base}/codex/responses`
    // plus the account header), with lazy refresh.
    let (access, account) = match auth::load().get(request.model.provider.as_str()) {
        Some(Credential::ApiKey { key }) => (key.clone(), None),
        _ => {
            let (access, account) = login::codex_access(&request.model.provider)
                .await
                .map_err(ProviderError::auth)?;
            (access, Some(account))
        }
    };
    let session_id = uuid::Uuid::new_v4().to_string();

    // Responses-API items: messages, function calls, and their outputs.
    let mut input: Vec<serde_json::Value> = Vec::new();
    for m in &request.messages {
        match m.role.as_str() {
            "assistant" => {
                if !m.content.is_empty() {
                    input.push(json!({
                        "type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": m.content}],
                    }));
                }
                for call in &m.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.arguments,
                    }));
                }
            }
            "reasoning" => {
                if let Ok(item) = serde_json::from_str::<serde_json::Value>(&m.content) {
                    input.push(item);
                }
            }
            "tool" => input.push(json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id.clone().unwrap_or_default(),
                "output": m.content,
            })),
            role => input.push(json!({
                "type": "message", "role": role,
                "content": [{"type": "input_text", "text": m.content}],
            })),
        }
    }

    let mut body = json!({
        "model": request.model.id,
        "store": false,
        "stream": true,
        "instructions": request.system,
        "input": input,
        "text": {"verbosity": "low"},
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": session_id,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
    });
    if let Some(effort) = &request.effort {
        body["reasoning"] = json!({"effort": effort, "summary": "auto"});
    }
    if !request.tools.is_empty() {
        // The Responses dialect wants flat tools ({type, name, …}) — the
        // chat-completions nesting 400s with "Missing required parameter:
        // 'tools[0].name'". Caught by the first live codex turn.
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t["function"]["name"],
                    "description": t["function"]["description"],
                    "parameters": t["function"]["parameters"],
                    "strict": false,
                })
            })
            .collect();
        body["tools"] = json!(tools);
    }

    let mut builder = match &account {
        Some(account) => http()
            .post(format!("{}/codex/responses", request.model.base_url))
            .header("chatgpt-account-id", account)
            .header("originator", "e")
            .header("OpenAI-Beta", "responses=experimental")
            .header("session-id", &session_id)
            .header("x-client-request-id", &session_id),
        None => http().post(format!("{}/responses", request.model.base_url)),
    };
    builder = builder
        .bearer_auth(&access)
        .header("accept", "text/event-stream");
    let response = require_success(send_request(builder.json(&body)).await?).await?;

    let mut sse = SseStream::new(response.bytes_stream());
    // function_call items accumulate argument deltas keyed by item id.
    let mut pending: std::collections::BTreeMap<String, ToolCall> = Default::default();
    loop {
        let payload = sse.next().await?;
        {
            if payload == "[DONE]" {
                return Ok(sse.end(FinishReason::Normal));
            }
            let value: serde_json::Value = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(_) => {
                    sse.malformed();
                    continue;
                }
            };
            match value["type"].as_str().unwrap_or("") {
                "response.output_text.delta" => {
                    if let Some(text) = value["delta"].as_str() {
                        let _ = tx.send(Event::TextDelta(text.into())).await;
                    }
                }
                "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                    if let Some(text) = value["delta"].as_str() {
                        let _ = tx.send(Event::ReasoningDelta(text.into())).await;
                    }
                }
                "response.output_item.added" => {
                    let item = &value["item"];
                    if item["type"].as_str() == Some("function_call") {
                        let key = item["id"]
                            .as_str()
                            .or(item["call_id"].as_str())
                            .unwrap_or("")
                            .to_string();
                        pending.insert(
                            key,
                            ToolCall {
                                id: item["call_id"].as_str().unwrap_or("").into(),
                                name: item["name"].as_str().unwrap_or("").into(),
                                arguments: item["arguments"].as_str().unwrap_or("").into(),
                            },
                        );
                    }
                }
                "response.function_call_arguments.delta" => {
                    let key = value["item_id"].as_str().unwrap_or("").to_string();
                    if let Some(call) = pending.get_mut(&key) {
                        call.arguments
                            .push_str(value["delta"].as_str().unwrap_or(""));
                    }
                }
                "response.output_item.done" => {
                    let item = &value["item"];
                    if item["type"].as_str() == Some("reasoning") {
                        // Must be replayed verbatim on the next request, ahead
                        // of the calls it produced — the API 400s otherwise.
                        let _ = tx.send(Event::ReasoningItem(item.to_string())).await;
                    }
                    if item["type"].as_str() == Some("function_call") {
                        let key = item["id"]
                            .as_str()
                            .or(item["call_id"].as_str())
                            .unwrap_or("")
                            .to_string();
                        let mut call = pending.remove(&key).unwrap_or(ToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        });
                        // The done item carries the authoritative fields.
                        if let Some(id) = item["call_id"].as_str() {
                            call.id = id.into();
                        }
                        if let Some(name) = item["name"].as_str() {
                            call.name = name.into();
                        }
                        if let Some(args) = item["arguments"].as_str() {
                            if !args.is_empty() {
                                call.arguments = args.into();
                            }
                        }
                        if !call.name.is_empty() {
                            let _ = tx.send(Event::ToolCall(call)).await;
                        }
                    }
                }
                kind @ ("response.completed" | "response.done" | "response.incomplete") => {
                    let usage = &value["response"]["usage"];
                    if usage.is_object() {
                        let cached = usage["input_tokens_details"]["cached_tokens"]
                            .as_u64()
                            .unwrap_or(0);
                        let _ = tx
                            .send(Event::Usage {
                                input: usage["input_tokens"].as_u64().unwrap_or(0),
                                output: usage["output_tokens"].as_u64().unwrap_or(0),
                                cache_read: cached,
                            })
                            .await;
                    }
                    // `response.incomplete` is a truncated reply the API still
                    // delivers with a 200 — name why instead of passing it off
                    // as a finished turn.
                    let finish = if kind == "response.incomplete" {
                        match value["response"]["incomplete_details"]["reason"]
                            .as_str()
                            .unwrap_or("")
                        {
                            "max_output_tokens" | "max_tokens" => FinishReason::Length,
                            "content_filter" => FinishReason::ContentFilter,
                            other => FinishReason::Other(format!("incomplete: {other}")),
                        }
                    } else {
                        FinishReason::Normal
                    };
                    return Ok(sse.end(finish));
                }
                "response.failed" => {
                    let message = value["response"]["error"]["message"]
                        .as_str()
                        .unwrap_or("response failed")
                        .to_string();
                    let cause = match value["response"]["error"]["code"].as_str().unwrap_or("") {
                        "rate_limit_exceeded" => FailureCause::RateLimited,
                        "server_error" | "internal_error" => FailureCause::ProviderUnavailable,
                        _ => FailureCause::Rejected,
                    };
                    return Err(ProviderError::frame(message, cause));
                }
                _ => {}
            }
        }
    }
}
