//! Per-directory trust: whether e may load a workspace's own instructions.
//!
//! Working in a directory means running model-directed tools in it (yolo), and
//! its AGENTS.md feeds the system prompt — an untrusted repo could steer the
//! agent through it. The first visit asks once; the answer is remembered in
//! `~/.e/trust.json` (merge-written, unknown keys survive). Untrusted means e
//! still works there, but the project's instructions stay out of context.

use sha2::{Digest, Sha256};
use std::path::Path;
use std::path::PathBuf;

use crate::core::config::{home, store};

pub const FORMAT_VERSION: u32 = 1;

fn file() -> std::path::PathBuf {
    home::home().join("trust.json")
}

fn canonical(cwd: &Path) -> PathBuf {
    cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
}

fn key(cwd: &Path) -> String {
    // Unlike `to_string_lossy`, this preserves the platform's complete path
    // representation (raw bytes on Unix, WTF-8 on Windows).
    let digest = Sha256::digest(cwd.as_os_str().as_encoded_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256-{hex}")
}

/// Old releases used the visible path as the object key. Preserve decisions
/// for valid UTF-8 paths, but never consult a lossy key for invalid bytes:
/// two distinct directories can collapse to the same replacement character.
fn legacy_key(cwd: &Path) -> Option<&str> {
    cwd.as_os_str().to_str()
}

/// Some(true) trusted, Some(false) declined, None never asked.
pub fn status(cwd: &Path) -> Option<bool> {
    let cwd = canonical(cwd);
    let object = store::read_object(&file()).unwrap_or_default();
    object
        .get(&key(&cwd))
        .or_else(|| legacy_key(&cwd).and_then(|legacy| object.get(legacy)))
        .and_then(|v| v.get("trusted"))
        .and_then(|v| v.as_bool())
}

pub fn trusted(cwd: &Path) -> bool {
    status(cwd) == Some(true)
}

pub fn set(cwd: &Path, trusted: bool) -> std::io::Result<()> {
    let cwd = canonical(cwd);
    let key = key(&cwd);
    let display = cwd.to_string_lossy().into_owned();
    store::update_versioned(&file(), 0o644, FORMAT_VERSION, |object| {
        object.insert(
            "format_version".into(),
            serde_json::Value::from(FORMAT_VERSION),
        );
        object.insert(
            key,
            serde_json::json!({ "path": display, "trusted": trusted }),
        );
    })
}
