//! Non-destructive JSON stores — the safeguard behind every file e writes
//! under `~/.e/`.
//!
//! A write never overwrites with an in-memory snapshot. It re-reads the file,
//! merges only the keys it means to change onto everything already there, and
//! renames a temp file into place atomically. Keys e doesn't recognize — a
//! future field, a hand-edit, another tool's addition — survive every write.
//!
//! A file that exists but won't parse is **quarantined**, not reset: it is
//! copied aside to `<name>.corrupt-<ms>` and a fresh object started, so the
//! user's data is always recoverable and never silently lost.

use std::io;
use std::path::Path;

use serde_json::{Map, Value};

use crate::core::config::home;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Read a JSON object from `path`. On a parse failure of an existing file,
/// quarantine it and return an empty object (the data lives on in the
/// `.corrupt-*` copy).
pub fn read_object(path: &Path) -> Map<String, Value> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => map,
            _ => {
                // Corrupt or non-object: preserve it, don't clobber it.
                let aside = path.with_extension(format!("corrupt-{}", now_ms()));
                let _ = std::fs::rename(path, &aside);
                Map::new()
            }
        },
        Err(_) => Map::new(),
    }
}

/// Merge changes into the file, preserving every other key. `mutate` sees the
/// current on-disk object and edits it in place. Written atomically.
pub fn update<F: FnOnce(&mut Map<String, Value>)>(
    path: &Path,
    mode: u32,
    mutate: F,
) -> io::Result<()> {
    home::ensure()?;
    let mut object = read_object(path);
    mutate(&mut object);
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(object))?
    );
    write_atomic(path, &text, mode)
}

fn write_atomic(path: &Path, contents: &str, mode: u32) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&tmp, path)
}
