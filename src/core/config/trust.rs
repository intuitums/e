//! Per-directory trust: whether e may load a workspace's own instructions.
//!
//! Working in a directory means running model-directed tools in it (yolo), and
//! its AGENTS.md feeds the system prompt — an untrusted repo could steer the
//! agent through it. The first visit asks once; the answer is remembered in
//! `~/.e/trust.json` (merge-written, unknown keys survive). Untrusted means e
//! still works there, but the project's instructions stay out of context.

use std::path::Path;

use crate::core::config::{home, store};

fn file() -> std::path::PathBuf {
    home::home().join("trust.json")
}

fn key(cwd: &Path) -> String {
    cwd.canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Some(true) trusted, Some(false) declined, None never asked.
pub fn status(cwd: &Path) -> Option<bool> {
    store::read_object(&file())
        .unwrap_or_default()
        .get(&key(cwd))
        .and_then(|v| v.get("trusted"))
        .and_then(|v| v.as_bool())
}

pub fn trusted(cwd: &Path) -> bool {
    status(cwd) == Some(true)
}

pub fn set(cwd: &Path, trusted: bool) -> std::io::Result<()> {
    let key = key(cwd);
    store::update(&file(), 0o644, |object| {
        object.insert(key, serde_json::json!({ "trusted": trusted }));
    })
}
