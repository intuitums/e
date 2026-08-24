//! The non-destructive store: writes preserve unknown keys, quarantine
//! corrupt files, and never wipe on a parse error.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
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
/// other's key. The mutex handles threads; the file lock handles independent
/// e processes. Every store shares one lock because all stores live in the
/// same small home and writes are rare.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

struct WriteGuard {
    _thread: MutexGuard<'static, ()>,
    _process: File,
}

fn lock_write() -> io::Result<WriteGuard> {
    // A panicked writer only poisons its own snapshot; the next writer starts
    // from disk anyway.
    let thread = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    home::ensure()?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let process = options.open(home::home().join(".store.lock"))?;
    process.lock()?;
    Ok(WriteGuard {
        _thread: thread,
        _process: process,
    })
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
    let _guard = lock_write()?;
    let mut object = read_object(path)?;
    mutate(&mut object);
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(object))?
    );
    write_atomic(path, &text, mode)
}

fn write_atomic(path: &Path, contents: &str, mode: u32) -> io::Result<()> {
    // Unique per attempt: stale files from a killed writer must never be
    // reused. The process lock prevents live writers from racing, while
    // create_new handles PID reuse and leftovers without clobbering them.
    static ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let (tmp, mut file) = loop {
        let n = ATTEMPT.fetch_add(1, Ordering::Relaxed);
        let candidate = path.with_extension(format!("tmp-{}-{n}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => break (candidate, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    if let Err(error) = file.write_all(contents.as_bytes()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = file.set_permissions(std::fs::Permissions::from_mode(mode)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(error);
        }
    }
    drop(file);
    std::fs::rename(&tmp, path).inspect_err(|_| {
        // Don't leave the temp behind if the destination cannot be replaced.
        let _ = std::fs::remove_file(&tmp);
    })
}
