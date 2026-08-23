//! Credentials: e's own `~/.e/auth.json`, nothing borrowed (DESIGN.md §2).
//!
//! Two credential shapes: a static API key, and OAuth (access/refresh/expiry,
//! plus the account id some backends demand in a header). The file is written
//! atomically with owner-only permissions. Refresh happens lazily when a
//! token is within a minute of expiry.

pub mod login;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;

use crate::core::config::home;

#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Credential {
    OAuth {
        access: String,
        refresh: String,
        /// Unix millis.
        expires: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        account_id: Option<String>,
    },
    ApiKey {
        key: String,
    },
}

pub type AuthFile = BTreeMap<String, Credential>;

/// Load the credentials e can interpret. Entries in a shape e doesn't
/// understand are skipped here but left untouched on disk — never wiped.
/// A provider with no stored credential falls back to its conventional
/// environment variable (ANTHROPIC_API_KEY and friends, declared in the
/// provider registry) — the reference behavior, and what CI wants.
pub fn load() -> AuthFile {
    let mut out = AuthFile::new();
    for (provider, value) in
        crate::core::config::store::read_object(&home::auth_path()).unwrap_or_default()
    {
        if let Ok(cred) = serde_json::from_value::<Credential>(value) {
            out.insert(provider, cred);
        }
    }
    // The Zen provider was once id'd `opencode`; honor old auth.json keys
    // under the new name. Read-only — the file stays as the user left it.
    if !out.contains_key("opencode-zen") {
        if let Some(cred) = out.remove("opencode") {
            out.insert("opencode-zen".into(), cred);
        }
    }
    for provider in crate::core::provider::registry::all() {
        if out.contains_key(&provider.name) {
            continue;
        }
        if let Some(env) = &provider.auth.key_env {
            if let Ok(key) = std::env::var(env) {
                if !key.trim().is_empty() {
                    out.insert(provider.name.clone(), Credential::ApiKey { key });
                }
            }
        }
    }
    out
}

/// Store one provider's credential, merging into the file so every other
/// provider — including any e couldn't parse — survives.
pub fn set(provider: &str, credential: Credential) -> io::Result<()> {
    let value = serde_json::to_value(credential).unwrap_or(serde_json::Value::Null);
    crate::core::config::store::update(&home::auth_path(), 0o600, |obj| {
        obj.insert(provider.to_string(), value);
    })
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The account id lives in the access token's JWT claims under the
/// `https://api.openai.com/auth` object as `chatgpt_account_id`.
pub fn account_id_from_jwt(access: &str) -> Option<String> {
    use base64::Engine;
    let payload = access.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .map(String::from)
}
