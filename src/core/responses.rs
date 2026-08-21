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
use crate::core::provider::{http, Event, Request, SseSplitter};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTH_BASE: &str = "https://auth.openai.com";

/// Refresh when within a minute of expiry; persist the rotated pair.
async fn fresh_access(provider: &str) -> Result<(String, String), String> {
    let mut file = auth::load();
    let Some(Credential::OAuth { access, refresh, expires, account_id }) = file.get(provider).cloned() else {
        return Err(format!("no OAuth credentials for {provider} — run `e auth {provider}`"));
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
        return Err(format!("token refresh rejected ({status}) — run `e auth {provider}` again"));
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
    file.insert(
        provider.to_string(),
        Credential::OAuth {
            access: access.to_string(),
            refresh: refresh.to_string(),
            expires: auth::now_ms() + expires_in * 1000,
            account_id: Some(account.clone()),
        },
    );
    auth::save(&file).map_err(|e| e.to_string())?;
    Ok((access.to_string(), account))
}

pub async fn run(request: &Request, tx: &mpsc::Sender<Event>) -> Result<(), String> {
    let (access, account) = fresh_access(&request.model.provider).await?;
    let session_id = uuid::Uuid::new_v4().to_string();

    // Responses-API message items.
    let input: Vec<serde_json::Value> = request
        .messages
        .iter()
        .map(|m| {
            let part = if m.role == "assistant" { "output_text" } else { "input_text" };
            json!({
                "type": "message",
                "role": m.role,
                "content": [{"type": part, "text": m.content}],
            })
        })
        .collect();

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

    let response = http()
        .post(format!("{}/codex/responses", request.model.base_url))
        .bearer_auth(&access)
        .header("chatgpt-account-id", &account)
        .header("originator", "e")
        .header("OpenAI-Beta", "responses=experimental")
        .header("accept", "text/event-stream")
        .header("session-id", &session_id)
        .header("x-client-request-id", &session_id)
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
                "response.completed" | "response.done" | "response.incomplete" => {
                    let usage = &value["response"]["usage"];
                    if usage.is_object() {
                        let cached = usage["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
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
                    return Err(message);
                }
                _ => {}
            }
        }
    }
    Ok(())
}
