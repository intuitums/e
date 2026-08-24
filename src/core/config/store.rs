//! The non-destructive store: writes preserve unknown keys, quarantine
//! corrupt files, and never wipe on a parse error.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::core::config::home;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One lock for every store write. The read-modify-write in [`update`] must
/// not interleave — two racing writers would each rename a snapshot over the
/// other's key. In-process writers serialize here; cross-process writers
/// serialize on the unique temp file (a second process's `rename` onto the
/// path we just claimed fails with `NotFound` and is retried whole).
static WRITE_LOCK: Mutex<()> = Mutex::new(());

fn lock_write() -> MutexGuard<'static, ()> {
    // A panicked writer only poisons its own snapshot; the next writer starts
    // from disk anyway.
    WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read a JSON object from `path`. A file that exists but won't parse is
/// quarantined aside as `<name>.corrupt-<ms>` (never clobbered) and an empty
/// object returned; a genuinely absent file reads as empty. Any *other*
/// read error — permissions, I/O — aborts: writing on top of it would erase
/// data e can't see.
pub fn read_object(path: &Path) -> io::Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            _ => {
                // Corrupt or non-object: preserve it, don't clobber it.
                quarantine(path)?;
                Ok(Map::new())
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e),
    }
}

/// Move an unreadable/corrupt file aside so the next write starts clean while
/// the user's bytes stay recoverable. Failing to preserve the original aborts
/// the caller instead of silently proceeding.
fn quarantine(path: &Path) -> io::Result<()> {
    let aside = path.with_extension(format!("corrupt-{}", now_ms()));
    std::fs::rename(path, &aside)
}

/// Merge changes into the file, preserving every other key. `mutate` sees the
/// current on-disk object and edits it in place. Written atomically.
///
/// Errors are loud: an unreadable source file (`PermissionDenied`, …) or a
/// failed preservation of corrupt bytes returns without touching anything.
pub fn update<F: FnOnce(&mut Map<String, Value>)>(
    path: &Path,
    mode: u32,
    mutate: F,
) -> io::Result<()> {
    let _guard = lock_write();
    home::ensure()?;
    let mut object = read_object(path)?;
    mutate(&mut object);
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(object))?
    );
    write_atomic(path, &text, mode)
}

fn write_atomic(path: &Path, contents: &str, mode: u32) -> io::Result<()> {
    // Unique per attempt: concurrent processes must never share one temp file
    // (one writer renaming it out from under another used to fail the write
    // or lose its keys). The pid keeps parallel tests distinct; the counter
    // keeps repeated attempts in one process distinct.
    static ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let n = ATTEMPT.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
    loop {
        match std::fs::write(&tmp, contents) {
            Ok(()) => break,
            // Another process renamed our temp name into place between our
            // claim and our write; pick a fresh name and go again.
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        // Don't leave the temp behind if the rename lost a race.
        let _ = std::fs::remove_file(&tmp);
    })
}
