//! Interactive credential acquisition — `e auth <provider>`.
//!
//! API-key providers prompt for a paste. The ChatGPT backend runs the real
//! OAuth authorization-code + PKCE flow: a one-shot listener on the fixed
//! localhost callback port, the browser opened to the authorize URL, the code
//! exchanged and persisted. No other tool's store is ever read (DESIGN.md §2).

use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

use crate::core::auth::{self, Credential};
use crate::core::responses::{AUTH_BASE, CLIENT_ID};

const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn auth_status() {
    let file = auth::load();
    if file.is_empty() {
        println!("no credentials — run `e auth <provider>`");
        return;
    }
    for (provider, credential) in &file {
        match credential {
            Credential::ApiKey { .. } => println!("{provider}: api key"),
            Credential::OAuth { expires, .. } => {
                let state = if auth::now_ms() < *expires {
                    "valid"
                } else {
                    "expired (will refresh)"
                };
                println!("{provider}: oauth, access {state}");
            }
        }
    }
}

/// Store an API key for a provider.
pub fn save_api_key(provider: &str, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("empty key".into());
    }
    auth::set(
        provider,
        Credential::ApiKey {
            key: key.to_string(),
        },
    )
    .map_err(|e| e.to_string())
}

/// The browser PKCE flow, reporting progress through `notify` so the TUI can
/// narrate it. The blocking callback wait runs off the async runtime.
pub async fn codex_login(provider: String, notify: tokio::sync::mpsc::Sender<String>) {
    let message = match codex_login_inner(&provider, &notify).await {
        Ok(()) => format!("signed in to {provider} — saved to ~/.e/auth.json"),
        Err(e) => format!("login failed: {e}"),
    };
    let _ = notify.send(message).await;
}

async fn codex_login_inner(
    provider: &str,
    notify: &tokio::sync::mpsc::Sender<String>,
) -> Result<(), String> {
    let mut verifier_bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut verifier_bytes);
    let verifier = b64url(&verifier_bytes);
    let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
    let mut state_bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut state_bytes);
    let state = b64url(&state_bytes);

    let authorize = format!(
        "{AUTH_BASE}/oauth/authorize?response_type=code&client_id={CLIENT_ID}\
         &redirect_uri={}&scope={}&code_challenge={challenge}&code_challenge_method=S256\
         &state={state}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=e",
        urlencode(REDIRECT_URI),
        urlencode("openid profile email offline_access"),
    );

    // Listener first, so the redirect can never race us.
    let listener = TcpListener::bind("127.0.0.1:1455").map_err(|e| {
        format!("cannot listen on localhost:1455 ({e}) — is another login running?")
    })?;

    let _ = notify.send("opening the browser to sign in…".into()).await;
    let _ = std::process::Command::new("open").arg(&authorize).spawn();

    let expected = state.clone();
    let code = tokio::task::spawn_blocking(move || wait_for_code(&listener, &expected))
        .await
        .map_err(|e| e.to_string())??;

    let response = crate::core::provider::http()
        .post(format!("{AUTH_BASE}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code.as_str()),
            ("code_verifier", verifier.as_str()),
            ("redirect_uri", REDIRECT_URI),
        ])
        .send()
        .await
        .map_err(|e| format!("token exchange failed: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "token exchange rejected ({status}): {}",
            body.chars().take(200).collect::<String>()
        ));
    }
    let tokens: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let (Some(access), Some(refresh), Some(expires_in)) = (
        tokens["access_token"].as_str(),
        tokens["refresh_token"].as_str(),
        tokens["expires_in"].as_u64(),
    ) else {
        return Err("token response missing fields".into());
    };
    let account_id = auth::account_id_from_jwt(access);

    auth::set(
        provider,
        Credential::OAuth {
            access: access.to_string(),
            refresh: refresh.to_string(),
            expires: auth::now_ms() + expires_in * 1000,
            account_id,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Accept exactly one callback request, validate state, answer with a page.
fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String, String> {
    for incoming in listener.incoming() {
        let mut stream = incoming.map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .map_err(|e| e.to_string())?;
        // Drain headers so the browser sees a complete exchange.
        let mut line = String::new();
        while reader.read_line(&mut line).map_err(|e| e.to_string())? > 2 {
            line.clear();
        }

        let path = request_line.split_whitespace().nth(1).unwrap_or("");
        if !path.starts_with("/auth/callback") {
            respond(&mut stream, 404, "not found");
            continue;
        }
        let query: std::collections::HashMap<_, _> = path
            .split_once('?')
            .map(|(_, q)| q)
            .unwrap_or("")
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .collect();
        if query.get("state").copied() != Some(expected_state) {
            respond(&mut stream, 400, "state mismatch — try again");
            return Err("state mismatch".into());
        }
        let Some(code) = query.get("code") else {
            let err = query.get("error").copied().unwrap_or("no code");
            respond(&mut stream, 400, "sign-in failed — return to the terminal");
            return Err(format!("authorization failed: {err}"));
        };
        respond(&mut stream, 200, "signed in — you can close this tab");
        return Ok(urldecode(code));
    }
    Err("listener closed".into())
}

fn respond(stream: &mut std::net::TcpStream, status: u16, message: &str) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let body =
        format!("<html><body style=\"font-family:sans-serif\"><p>{message}</p></body></html>");
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
    // Give the browser a beat to read before the socket drops.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
    let mut sink = [0u8; 256];
    let _ = stream.read(&mut sink);
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
