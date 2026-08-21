//! Credentials: e's own `~/.e/auth.json`, nothing borrowed (DESIGN.md §2).
//!
//! Two credential shapes: a static API key, and OAuth (access/refresh/expiry,
//! plus the account id some backends demand in a header). The file is written
//! atomically with owner-only permissions. Refresh happens lazily when a
//! token is within a minute of expiry.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;

use crate::core::home;

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
    ApiKey { key: String },
}

pub type AuthFile = BTreeMap<String, Credential>;

pub fn load() -> AuthFile {
    match std::fs::read_to_string(home::auth_path()) {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(_) => AuthFile::default(),
    }
}

pub fn save(auth: &AuthFile) -> io::Result<()> {
    home::ensure()?;
    let path = home::auth_path();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(auth)?;
    std::fs::write(&tmp, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
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
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .map(String::from)
}
