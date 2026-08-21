//! The Responses-API dialect.
//!
//! One dialect, more than one deployment: the ChatGPT backend mounts it at
//! `{base}/codex/responses` behind a subscription OAuth (bearer + account-id
//! header, lazy refresh); other providers serve the same event grammar at
//! `{base}/responses` behind a plain key. The provider id — not this module —
//! names the account type.

use futures::StreamExt;
use serde_json::json;
use tokio::sync::mpsc;

use crate::core::auth::{self, Credential};
use crate::core::provider::{http, Event, Request, SseSplitter, ToolCall};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTH_BASE: &str = "https://auth.openai.com";

/// Refresh when within a minute of expiry; persist the rotated pair.
async fn fresh_access(provider: &str) -> Result<(String, String), String> {
    let Some(Credential::OAuth {
        access,
        refresh,
        expires,
        account_id,
    }) = auth::load().get(provider).cloned()
    else {
        return Err(format!(
            "no OAuth credentials for {provider} — run `e auth {provider}`"
        ));
    };
    let account = account_id
        .or_else(|| auth::account_id_from_jwt(&access))
        .ok_or("credentials carry no account id")?;

    if auth::now_ms() + 60_000 < expires {
        return Ok((access, account));
    }

    let response = http()
        .post(format!("{AUTH_BASE}/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|e| format!("token refresh failed: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(format!(
            "token refresh rejected ({status}) — run `e auth {provider}` again"
        ));
    }
    let tokens: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let (Some(access), Some(refresh), Some(expires_in)) = (
        tokens["access_token"].as_str(),
        tokens["refresh_token"].as_str(),
        tokens["expires_in"].as_u64(),
    ) else {
        return Err("token refresh response missing fields".into());
    };
    let account = auth::account_id_from_jwt(access).unwrap_or(account);
    auth::set(
        provider,
        Credential::OAuth {
            access: access.to_string(),
            refresh: refresh.to_string(),
            expires: auth::now_ms() + expires_in * 1000,
            account_id: Some(account.clone()),
        },
    )
    .map_err(|e| e.to_string())?;
    Ok((access.to_string(), account))
}

type RunError = (String, crate::core::provider::ErrorKind);

pub async fn run(request: &Request, tx: &mpsc::Sender<Event>) -> Result<(), RunError> {
    // A plain API key means the standard platform mount (`{base}/responses`,
    // bearer only); OAuth means the ChatGPT backend (`{base}/codex/responses`
    // plus the account header), with lazy refresh.
    let (access, account) = match auth::load().get(request.model.provider.as_str()) {
        Some(Credential::ApiKey { key }) => (key.clone(), None),
        _ => {
            let (access, account) = fresh_access(&request.model.provider)
                .await
                .map_err(|e| (e, crate::core::provider::ErrorKind::Auth))?;
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
    let response = builder.json(&body).send().await.map_err(|e| {
        (
            format!("request failed: {e}"),
            crate::core::provider::ErrorKind::Transient,
        )
    })?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err((
            format!("{status}: {}", text.chars().take(300).collect::<String>()),
            crate::core::provider::ErrorKind::Delivered,
        ));
    }

    let mut splitter = SseSplitter::new();
    let mut stream = response.bytes_stream();
    // function_call items accumulate argument deltas keyed by item id.
    let mut pending: std::collections::BTreeMap<String, ToolCall> = Default::default();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            (
                format!("stream error: {e}"),
                crate::core::provider::ErrorKind::Delivered,
            )
        })?;
        for payload in splitter.feed(&String::from_utf8_lossy(&chunk)) {
            if payload == "[DONE]" {
                return Ok(());
            }
            let value: serde_json::Value = match serde_json::from_str(&payload) {
                Ok(v) => v,
                Err(_) => continue,
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
                "response.completed" | "response.done" | "response.incomplete" => {
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
                    return Ok(());
                }
                "response.failed" => {
                    let message = value["response"]["error"]["message"]
                        .as_str()
                        .unwrap_or("response failed")
                        .to_string();
                    return Err((message, crate::core::provider::ErrorKind::Delivered));
                }
                _ => {}
            }
        }
    }
    Ok(())
}
