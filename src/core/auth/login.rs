//! Interactive credential acquisition — `/login <provider>` in the TUI.
//!
//! API-key providers prompt for a paste. The ChatGPT backend runs the real
//! OAuth authorization-code + PKCE flow: a one-shot listener on the fixed
//! localhost callback port, the browser opened to the authorize URL, the code
//! exchanged and persisted. No other tool's credential store is read.

use base64::Engine;
use rand::Rng;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;

use crate::core::auth::{self, Credential};

pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTH_BASE: &str = "https://auth.openai.com";

const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
const CALLBACK_ADDR: &str = "127.0.0.1:1455";

/// Bound on turn-path token refreshes (whole request — they return small
/// JSON, never a stream). Interactive login flows stay unbounded: the user
/// is present and can abort them.
const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Shared cancellation for an interactive login. The TUI also aborts the
/// async task, while this flag releases the blocking localhost callback wait.
#[derive(Clone, Default)]
pub struct LoginCancellation(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl LoginCancellation {
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Cancellation is synchronous at the UI boundary: wait briefly for the
/// blocking callback worker to observe its flag and drop the fixed port so a
/// second login can start immediately.
pub(crate) fn wait_for_callback_release() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        match TcpListener::bind(CALLBACK_ADDR) {
            Ok(listener) => {
                drop(listener);
                return;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::AddrInUse
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => return,
        }
    }
}
fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(target_os = "macos")]
fn browser_opener() -> &'static str {
    "open"
}

#[cfg(target_os = "linux")]
fn browser_opener() -> &'static str {
    "xdg-open"
}

/// Launch a browser when the platform opener is available, but always show a
/// copyable URL so headless Linux, SSH, and opener failures cannot strand an
/// OAuth flow waiting for a callback the user has no way to initiate.
async fn launch_browser(url: &str, notify: &tokio::sync::mpsc::Sender<String>) {
    let message = match std::process::Command::new(browser_opener())
        .arg(url)
        .spawn()
    {
        Ok(_) => format!("opening the browser to sign in…\nIf it does not open, visit: {url}"),
        Err(error) => format!(
            "could not open the browser with {} ({error})\nVisit: {url}",
            browser_opener()
        ),
    };
    let _ = notify.send(message).await;
}

pub fn auth_status() {
    let file = auth::load();
    if file.is_empty() {
        println!("no credentials — start e and run `/login <provider>`");
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
/// How a login flow ended. The typed signal the TUI acts on — display text
/// goes out separately as notices; control flow never parses prose.
pub enum Outcome {
    SignedIn {
        provider: String,
        flow_id: Option<u64>,
    },
    Failed {
        flow_id: u64,
    },
}

pub async fn codex_login(
    provider: String,
    notify: tokio::sync::mpsc::Sender<String>,
    outcomes: tokio::sync::mpsc::Sender<Outcome>,
    cancellation: LoginCancellation,
    flow_id: u64,
) {
    let (message, outcome) = match codex_login_inner(&provider, &notify, &cancellation).await {
        Ok(()) => (
            format!("signed in to {provider} — saved to ~/.e/auth.json"),
            Outcome::SignedIn {
                provider,
                flow_id: Some(flow_id),
            },
        ),
        Err(e) => (format!("login failed: {e}"), Outcome::Failed { flow_id }),
    };
    let _ = notify.send(message).await;
    let _ = outcomes.send(outcome).await;
}

async fn codex_login_inner(
    provider: &str,
    notify: &tokio::sync::mpsc::Sender<String>,
    cancellation: &LoginCancellation,
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
    let listener = TcpListener::bind(CALLBACK_ADDR).map_err(|e| {
        format!("cannot listen on localhost:1455 ({e}) — is another login running?")
    })?;

    launch_browser(&authorize, notify).await;

    let expected = state.clone();
    let cancellation = cancellation.clone();
    let code =
        tokio::task::spawn_blocking(move || wait_for_code(&listener, &expected, &cancellation))
            .await
            .map_err(|error| error.to_string())??;

    let response = crate::core::providers::http()
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
fn wait_for_code(
    listener: &TcpListener,
    expected_state: &str,
    cancellation: &LoginCancellation,
) -> Result<String, String> {
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    loop {
        if cancellation.is_cancelled() {
            return Err("login cancelled".into());
        }
        let (mut stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        // BSD platforms can inherit O_NONBLOCK from the listener. Callback
        // reads should block only up to the timeout below, not fail before the
        // browser has written its request.
        stream.set_nonblocking(false).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(250)))
            .map_err(|e| e.to_string())?;
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
            respond(&mut stream, 404, "Not found", "");
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
            respond(
                &mut stream,
                400,
                "State mismatch",
                "Try again from the terminal.",
            );
            continue;
        }
        let Some(code) = query.get("code") else {
            let err = query.get("error").copied().unwrap_or("no code");
            respond(
                &mut stream,
                400,
                "Sign-in failed",
                "Return to the terminal.",
            );
            return Err(format!("authorization failed: {err}"));
        };
        respond(&mut stream, 200, "Signed in", "You can close this tab.");
        return Ok(urldecode(code));
    }
}

/// The one e surface a browser renders: the wordmark, a title, a dim line —
/// e's look in page form. UTF-8 is declared in the header AND a meta tag
/// (the em-dash mojibake taught us not to let browsers guess).
fn respond(stream: &mut std::net::TcpStream, status: u16, title: &str, detail: &str) {
    let reason = if status == 200 { "OK" } else { "Error" };
    let body = format!(
        concat!(
            "<!doctype html><html><head><meta charset=\"utf-8\">",
            "<title>e · {title}</title><style>",
            ":root{{--bg:#ffffff;--ink:#262626;--dim:#767676}}",
            "@media(prefers-color-scheme:dark){{:root{{--bg:#0c0c0c;--ink:#e8e8e8;--dim:#8a8a8a}}}}",
            "html,body{{height:100%;margin:0}}",
            "body{{display:flex;align-items:center;justify-content:center;",
            "background:var(--bg);color:var(--ink);",
            "font-family:ui-monospace,SFMono-Regular,Menlo,monospace}}",
            "main{{text-align:center}}",
            ".mark{{font-size:64px;line-height:1}}",
            ".title{{margin-top:24px;font-size:16px}}",
            ".detail{{margin-top:8px;font-size:13px;color:var(--dim)}}",
            "</style></head><body><main>",
            "<div class=\"mark\">𝑒</div>",
            "<div class=\"title\">{title}</div>",
            "<div class=\"detail\">{detail}</div>",
            "</main></body></html>"
        ),
        title = title,
        detail = detail,
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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

/* ---------- xAI (device-code flow) ---------- */

const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const XAI_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const XAI_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
/// Refresh ahead of the reported expiry so a token never dies mid-request.
const XAI_REFRESH_SKEW_MS: u64 = 5 * 60 * 1000;
const XAI_DEFAULT_LIFETIME_SECS: u64 = 3600;

/// RFC 8628: ask for a device code, send the user to the verification page,
/// poll the token endpoint until they approve. The access token then acts as
/// the API key on api.x.ai.
pub async fn xai_login(
    notify: tokio::sync::mpsc::Sender<String>,
    outcomes: tokio::sync::mpsc::Sender<Outcome>,
    cancellation: LoginCancellation,
    flow_id: u64,
) {
    let (message, outcome) = match xai_login_inner(&notify, &cancellation).await {
        Ok(()) => (
            "signed in to xAI — saved to ~/.e/auth.json".to_string(),
            Outcome::SignedIn {
                provider: "xai".into(),
                flow_id: Some(flow_id),
            },
        ),
        Err(e) => (format!("login failed: {e}"), Outcome::Failed { flow_id }),
    };
    let _ = notify.send(message).await;
    let _ = outcomes.send(outcome).await;
}

async fn xai_login_inner(
    notify: &tokio::sync::mpsc::Sender<String>,
    cancellation: &LoginCancellation,
) -> Result<(), String> {
    let device: serde_json::Value = crate::core::providers::http()
        .post(XAI_DEVICE_CODE_URL)
        .form(&[
            ("client_id", XAI_CLIENT_ID),
            ("scope", XAI_SCOPE),
            ("referrer", "e"),
        ])
        .send()
        .await
        .map_err(|e| format!("device authorization failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("device authorization returned invalid JSON: {e}"))?;

    let device_code = required(&device, "device_code")?;
    let user_code = required(&device, "user_code")?;
    let verify_url = device
        .get("verification_uri_complete")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or(required(&device, "verification_uri")?);
    if !verify_url.starts_with("https://") {
        return Err("untrusted verification URI in xAI response".into());
    }
    let mut interval = device
        .get("interval")
        .and_then(|v| v.as_u64())
        .filter(|i| *i > 0)
        .unwrap_or(5);
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_secs(
            device
                .get("expires_in")
                .and_then(|v| v.as_u64())
                .unwrap_or(600),
        );

    let _ = notify
        .send(format!("xAI: confirm code {user_code} in the browser"))
        .await;
    launch_browser(&verify_url, notify).await;

    loop {
        if cancellation.is_cancelled() {
            return Err("login cancelled".into());
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        if std::time::Instant::now() > deadline {
            return Err("xAI device code expired".into());
        }
        let response = crate::core::providers::http()
            .post(XAI_TOKEN_URL)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", XAI_CLIENT_ID),
                ("device_code", device_code.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("token polling failed: {e}"))?;
        let ok = response.status().is_success();
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("token polling returned invalid JSON: {e}"))?;
        if ok {
            let credential = xai_credential(&body, None)?;
            return crate::core::auth::set("xai", credential).map_err(|e| e.to_string());
        }
        match body.get("error").and_then(|v| v.as_str()) {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("access_denied") | Some("authorization_denied") => {
                return Err("xAI device authorization was denied".into())
            }
            Some("expired_token") => return Err("xAI device code expired".into()),
            other => {
                return Err(format!(
                    "xAI token polling failed: {}",
                    other.unwrap_or("unknown error")
                ))
            }
        }
    }
}

/// Exchange the refresh token; xAI may omit `refresh_token` when unrotated.
pub async fn xai_refresh(refresh: &str) -> Result<crate::core::auth::Credential, String> {
    let response = crate::core::providers::http()
        .post(XAI_TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", XAI_CLIENT_ID),
            ("refresh_token", refresh),
        ])
        // Refreshes run on the turn path: a hung token endpoint must fail
        // the turn, not park it before the provider request even starts.
        .timeout(REFRESH_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("xAI token refresh failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "xAI token refresh rejected ({}) — run /login xai again",
            response.status()
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("xAI token refresh returned invalid JSON: {e}"))?;
    xai_credential(&body, Some(refresh))
}

/// ChatGPT / Codex OAuth: refresh when within a minute of expiry; persist the
/// rotated pair. Returns `(access_token, chatgpt_account_id)`.
pub async fn codex_access(provider: &str) -> Result<(String, String), String> {
    let Some(Credential::OAuth {
        access,
        refresh,
        expires,
        account_id,
    }) = auth::load().get(provider).cloned()
    else {
        return Err(format!(
            "no OAuth credentials for {provider} — start e and run `/login {provider}`"
        ));
    };
    let account = account_id
        .or_else(|| auth::account_id_from_jwt(&access))
        .ok_or("credentials carry no account id")?;

    if auth::now_ms() + 60_000 < expires {
        return Ok((access, account));
    }

    let response = crate::core::providers::http()
        .post(format!("{AUTH_BASE}/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("client_id", CLIENT_ID),
        ])
        // Refreshes run on the turn path: a hung token endpoint must fail
        // the turn, not park it before the provider request even starts.
        .timeout(REFRESH_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("token refresh failed: {e}"))?;
    if !response.status().is_success() {
        let status = response.status();
        return Err(format!(
            "token refresh rejected ({status}) — start e and run `/login {provider}` again"
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

fn persist_xai_refresh<F>(
    provider: &str,
    credential: Credential,
    persist: F,
) -> Result<String, String>
where
    F: FnOnce(&str, Credential) -> std::io::Result<()>,
{
    let access = match &credential {
        Credential::OAuth { access, .. } => access.clone(),
        Credential::ApiKey { key } => key.clone(),
    };
    persist(provider, credential).map_err(|error| {
        format!("xAI token refresh succeeded, but saving refreshed credentials failed: {error}")
    })?;
    Ok(access)
}

/// A Bearer / x-api-key value for dialects that don't need an account header.
/// API keys pass through; OAuth refreshes lazily via the provider's declared
/// flow (`xai-device` or `codex`).
pub async fn access_token(provider: &str) -> Result<String, String> {
    match auth::load().get(provider).cloned() {
        Some(Credential::ApiKey { key }) => Ok(key),
        Some(Credential::OAuth {
            access,
            refresh,
            expires,
            ..
        }) => {
            if auth::now_ms() + 60_000 < expires {
                return Ok(access);
            }
            let flow =
                crate::core::providers::registry::find(provider).and_then(|p| p.auth.oauth.clone());
            match flow.as_deref() {
                Some("xai-device") => {
                    let fresh = xai_refresh(&refresh).await?;
                    persist_xai_refresh(provider, fresh, auth::set)
                }
                Some("codex") => codex_access(provider).await.map(|(access, _)| access),
                other => Err(format!(
                    "cannot refresh {provider} (oauth flow {:?}) — run /login",
                    other
                )),
            }
        }
        // A keyless local backend needs no credential; the placeholder rides
        // the Authorization header, which such servers ignore. A stored key
        // (matched above) still wins for locals configured to require one.
        None if crate::core::providers::registry::find(provider).is_some_and(|p| p.auth.none) => {
            Ok("unauthenticated".into())
        }
        None => Err(format!("no credentials for {provider} — run /login")),
    }
}

fn xai_credential(
    body: &serde_json::Value,
    previous_refresh: Option<&str>,
) -> Result<crate::core::auth::Credential, String> {
    let access = required(body, "access_token")?;
    let refresh = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| previous_refresh.map(String::from))
        .ok_or("xAI response missing refresh_token")?;
    let lifetime = body
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .unwrap_or(XAI_DEFAULT_LIFETIME_SECS);
    Ok(crate::core::auth::Credential::OAuth {
        access,
        refresh,
        // Short-lived test/dev tokens are valid too. Saturating arithmetic
        // keeps a smaller-than-skew lifetime from wrapping into an expiry
        // thousands of years in the future; use `now` to refresh it promptly.
        expires: crate::core::auth::now_ms()
            .saturating_add(lifetime.saturating_mul(1000))
            .saturating_sub(XAI_REFRESH_SKEW_MS)
            .max(crate::core::auth::now_ms()),
        account_id: None,
    })
}

fn required(value: &serde_json::Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| format!("invalid xAI OAuth response field: {field}"))
}

#[cfg(test)]
mod tests {
    /// The callback page must declare UTF-8 — the em-dash mojibake bug — and
    /// carry e's wordmark.
    #[test]
    fn callback_page_is_utf8_and_branded() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            super::respond(&mut stream, 200, "Signed in", "You can close this tab.");
        });
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        use std::io::Write;
        client.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        let mut response = Vec::new();
        let _ = client.read_to_end(&mut response);
        server.join().unwrap();
        let text = String::from_utf8(response).expect("response is valid UTF-8");
        assert!(text.contains("content-type: text/html; charset=utf-8"));
        assert!(text.contains("<meta charset=\"utf-8\">"));
        assert!(text.contains("𝑒"));
        assert!(text.contains("Signed in"));
        assert!(text.contains("You can close this tab."));
        // The body length header counts bytes, not chars.
        let body = text.split("\r\n\r\n").nth(1).unwrap();
        let declared: usize = text
            .lines()
            .find(|l| l.starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1)?.trim().parse().ok())
            .unwrap();
        assert_eq!(declared, body.len());
    }

    #[test]
    fn browser_opener_matches_the_platform() {
        #[cfg(target_os = "linux")]
        assert_eq!(super::browser_opener(), "xdg-open");
        #[cfg(target_os = "macos")]
        assert_eq!(super::browser_opener(), "open");
    }
    fn callback(addr: std::net::SocketAddr, path: &str) -> String {
        use std::io::{Read, Write};
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        write!(client, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn wrong_state_callback_does_not_abort_the_active_login() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let cancellation = super::LoginCancellation::default();
        let server_cancel = cancellation.clone();
        let server =
            std::thread::spawn(move || super::wait_for_code(&listener, "expected", &server_cancel));

        let wrong = callback(addr, "/auth/callback?code=stale&state=wrong");
        assert!(wrong.starts_with("HTTP/1.1 400"));
        let correct = callback(addr, "/auth/callback?code=fresh&state=expected");
        assert!(correct.starts_with("HTTP/1.1 200"));
        assert_eq!(server.join().unwrap().unwrap(), "fresh");
    }

    #[test]
    fn cancellation_releases_the_callback_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let cancellation = super::LoginCancellation::default();
        let server_cancel = cancellation.clone();
        let server =
            std::thread::spawn(move || super::wait_for_code(&listener, "expected", &server_cancel));

        cancellation.cancel();
        assert_eq!(server.join().unwrap().unwrap_err(), "login cancelled");
        std::net::TcpListener::bind(addr).expect("cancelled login releases its callback port");
    }

    #[test]
    fn xai_refresh_persistence_failure_is_not_reported_as_success() {
        let fresh = super::xai_credential(
            &serde_json::json!({
                "access_token": "new-access",
                "refresh_token": "rotated-refresh",
                "expires_in": 3600
            }),
            Some("old-refresh"),
        )
        .unwrap();
        let result = super::persist_xai_refresh("xai", fresh, |_, credential| {
            match credential {
                super::Credential::OAuth {
                    access, refresh, ..
                } => {
                    assert_eq!(access, "new-access");
                    assert_eq!(refresh, "rotated-refresh");
                }
                super::Credential::ApiKey { .. } => panic!("xAI refresh returned an API key"),
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "auth.json is read-only",
            ))
        });

        let error = result.unwrap_err();
        assert!(error.contains("refresh succeeded"));
        assert!(error.contains("saving refreshed credentials failed"));
        assert!(error.contains("Permission denied") || error.contains("read-only"));
    }

    #[test]
    fn short_lived_xai_token_expires_now_instead_of_wrapping() {
        let before = crate::core::auth::now_ms();
        let credential = super::xai_credential(
            &serde_json::json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "expires_in": 1
            }),
            None,
        )
        .unwrap();
        let after = crate::core::auth::now_ms();
        let super::Credential::OAuth { expires, .. } = credential else {
            panic!("xAI returns OAuth credentials");
        };
        assert!((before..=after).contains(&expires));
    }
}
