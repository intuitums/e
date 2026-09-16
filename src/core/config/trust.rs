//! Per-directory trust: whether e may load a workspace's own instructions.
//!
//! Working in a directory means running model-directed tools in it (yolo), and
//! its AGENTS.md feeds the system prompt — an untrusted repo could steer the
//! agent through it. So the answer is a precondition, not a filter: e runs in a
//! trusted workspace, or it refuses and says how to trust one. The first visit
//! asks once; the answer is remembered in `~/.e/trust.json` (merge-written,
//! unknown keys survive). Sessions with no terminal record it with `e trust`.

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

/// The recorded decision for exactly this directory, if any.
fn decision(object: &serde_json::Map<String, serde_json::Value>, cwd: &Path) -> Option<bool> {
    object
        .get(&key(cwd))
        .or_else(|| legacy_key(cwd).and_then(|legacy| object.get(legacy)))
        .and_then(|v| v.get("trusted"))
        .and_then(|v| v.as_bool())
}

/// Some(true) trusted, Some(false) declined, None never asked. A trusted
/// ancestor extends to everything inside it — trusting `~/code` covers
/// `~/code/clones/e-1` — while a *declined* ancestor answers only for
/// itself, so its other children still get their own first-visit question.
/// This directory's own recorded answer always wins over an ancestor's.
pub fn status(cwd: &Path) -> Option<bool> {
    let cwd = canonical(cwd);
    let object = store::read_object(&file()).unwrap_or_default();
    if let Some(answer) = decision(&object, &cwd) {
        return Some(answer);
    }
    cwd.ancestors()
        .skip(1)
        .any(|ancestor| decision(&object, ancestor) == Some(true))
        .then_some(true)
}

/// The broader ancestor the trust panel offers as its middle choice: the
/// top-most directory under $HOME that contains `cwd` (for
/// `~/code/clones/e-1` that is `~/code`), or the immediate parent when the
/// workspace lives outside home. None when nothing broader is sensible —
/// the workspace sits directly under home, or its parent is the root.
pub fn parent_option(cwd: &Path) -> Option<PathBuf> {
    let cwd = canonical(cwd);
    if let Some(home) = home::user_home() {
        let home = canonical(&home);
        if let Ok(relative) = cwd.strip_prefix(&home) {
            let first = relative.components().next()?;
            let top = home.join(first);
            return (top != cwd).then_some(top);
        }
    }
    cwd.parent()
        .filter(|parent| parent.parent().is_some())
        .map(Path::to_path_buf)
}

pub fn trusted(cwd: &Path) -> bool {
    status(cwd) == Some(true)
}

/// Why e will not run in this directory, or `None` when trust was accepted.
///
/// Every frontend says the same thing with this, so the rule and its remedy
/// cannot drift between the terminal, `-p`, and `e rpc`.
pub fn refusal(cwd: &Path) -> Option<String> {
    let dir = cwd.display();
    match status(cwd) {
        Some(true) => None,
        Some(false) => Some(format!(
            "{dir} must be trusted to run e — it was declined, so run `e trust {dir}` to allow it"
        )),
        None => Some(format!(
            "{dir} must be trusted to run e — run `e trust {dir}`, or run `e` in that directory to answer the trust dialog"
        )),
    }
}

/// Whether trusting `cwd` would cover the user's home directory, whose trust is
/// never recorded: a trusted ancestor extends to everything inside it, so
/// trusting home would trust every project under it at once.
fn covers_home(cwd: &Path) -> bool {
    home::user_home().is_some_and(|home| canonical(&home).starts_with(cwd))
}

pub fn set(cwd: &Path, trusted: bool) -> std::io::Result<()> {
    let cwd = canonical(cwd);
    if trusted && covers_home(&cwd) {
        return Err(std::io::Error::other(format!(
            "{} covers every project in your home directory — trust the project itself",
            cwd.display()
        )));
    }
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
