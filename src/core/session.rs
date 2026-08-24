//! Sessions: an append-only JSONL log per conversation, under
//! `~/.e/sessions/<cwd-slug>/<timestamp>_<uuid>.jsonl`.
//!
//! One line per entry: a `session` header, then `message` entries carrying
//! the same ChatMessage the agent uses. The agent creates the file lazily on
//! the first user send, and listing also rejects header-only or assistant-only
//! files, so opening and closing e never counts as a session. Resume replays
//! messages back into the agent. The title is derived from the first user
//! message (first line, eight words), never model-generated.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::config::home;
use crate::core::providers::ChatMessage;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum Entry {
    #[serde(rename = "session")]
    Header {
        id: String,
        cwd: String,
        created: u64,
        model: String,
    },
    #[serde(rename = "message")]
    Message { message: ChatMessage },
    /// A display name an extension set via session_name — shown in /resume,
    /// overriding the title derived from the first user message.
    #[serde(rename = "name")]
    Name { name: String },
}

pub struct Session {
    path: PathBuf,
    file: File,
    /// Held for as long as this Session exists; its sidecar file marks
    /// ownership so a second e cannot append to the same log.
    _lock: LockGuard,
}

/// A sidecar `<session>.lock` holding the owner's PID. Exclusive creation
/// arbitrates ownership; a lock whose PID is no longer alive is stolen, so
/// a crashed e never wedges a session permanently.
struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    fn acquire(session_path: &Path) -> std::io::Result<LockGuard> {
        let lock_path = session_path.with_extension("lock");
        loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    writeln!(file, "{}", std::process::id())?;
                    return Ok(LockGuard { path: lock_path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_owner_alive(&lock_path) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            "this session is already active in another e",
                        ));
                    }
                    // The owner is gone — steal the stale lock and retry;
                    // if another stealer won the race, its live PID fails
                    // us on the next pass.
                    let _ = std::fs::remove_file(&lock_path);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// `kill -0` reports liveness without signaling. Only reached on a lock
/// conflict, so spawning `/bin/kill` costs nothing on the happy path.
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn lock_owner_alive(lock_path: &Path) -> bool {
    // An unreadable or empty lock (crashed between create and PID write)
    // counts as dead — it must never wedge a session shut.
    let Ok(content) = std::fs::read_to_string(lock_path) else {
        return false;
    };
    let Ok(pid) = content.trim().parse::<u32>() else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        pid_alive(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true // No liveness probe available; stay conservative.
    }
}

fn normalized_cwd(cwd: &Path) -> PathBuf {
    cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
}

/// A collision-resistant workspace key. The previous slash-to-hyphen scheme
/// mapped distinct paths such as `a/b-c` and `a-b/c` to the same directory.
fn cwd_slug(cwd: &Path) -> String {
    use sha2::Digest;
    use std::os::unix::ffi::OsStrExt;
    let cwd = normalized_cwd(cwd);
    let digest = sha2::Sha256::digest(cwd.as_os_str().as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("sha256-{hex}")
}

/// The pre-0.4 directory name, read-only so existing sessions remain
/// resumable. Files found there are checked against their header cwd because
/// the legacy encoding can contain sessions from more than one workspace.
fn legacy_cwd_slug(cwd: &Path) -> String {
    let joined = normalized_cwd(cwd).to_string_lossy().replace('/', "-");
    format!("-{joined}-")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Session {
    /// Create a fresh session log for this workspace.
    pub fn create(cwd: &Path, model: &str) -> std::io::Result<Session> {
        let cwd = normalized_cwd(cwd);
        let dir = home::sessions_dir().join(cwd_slug(&cwd));
        std::fs::create_dir_all(&dir)?;
        let id = uuid::Uuid::now_v7().to_string();
        let stamp = now_ms();
        let path = dir.join(format!("{stamp}_{id}.jsonl"));
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let lock = LockGuard::acquire(&path)?;
        let header = Entry::Header {
            id,
            cwd: cwd.to_string_lossy().into_owned(),
            created: stamp,
            model: model.to_string(),
        };
        writeln!(file, "{}", serde_json::to_string(&header)?)?;
        Ok(Session {
            path,
            file,
            _lock: lock,
        })
    }

    /// One serialized record per write call, newline included — even under
    /// an unexpected second writer, records never share a line. A failed
    /// write is lost history: callers must surface the error, not shrug.
    pub fn append(&mut self, message: &ChatMessage) -> std::io::Result<()> {
        let mut line = serde_json::to_string(&Entry::Message {
            message: message.clone(),
        })?;
        line.push('\n');
        self.file.write_all(line.as_bytes())
    }

    /// Set the display name e shows for this session. Idempotent; appends a
    /// name entry, so the most recent name wins on resume.
    pub fn set_name(&mut self, name: &str) -> std::io::Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let mut line = serde_json::to_string(&Entry::Name {
            name: name.to_string(),
        })?;
        line.push('\n');
        self.file.write_all(line.as_bytes())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read all messages out of a session file. A malformed record is
    /// corruption, not noise — returning a shortened history would present
    /// lost messages as a valid conversation.
    pub fn load(path: &Path) -> std::io::Result<Vec<ChatMessage>> {
        let reader = BufReader::new(File::open(path)?);
        let mut messages = Vec::new();
        for (index, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Entry>(&line) {
                Ok(Entry::Message { message }) => messages.push(message),
                Ok(_) => {}
                Err(e) => {
                    return Err(std::io::Error::other(format!(
                        "corrupt session record at line {}: {e}",
                        index + 1
                    )))
                }
            }
        }
        Ok(messages)
    }

    /// Re-open an existing session for appending. Fails while another
    /// process owns the session's lock.
    pub fn reopen(path: &Path) -> std::io::Result<Session> {
        let lock = LockGuard::acquire(path)?;
        let file = OpenOptions::new().append(true).open(path)?;
        Ok(Session {
            path: path.to_path_buf(),
            file,
            _lock: lock,
        })
    }
}

/// The latest persisted display name in a session file, if any — the name a
/// resume must adopt so the session doesn't drift from its log.
pub fn name_of(path: &Path) -> Option<String> {
    let reader = BufReader::new(File::open(path).ok()?);
    let mut name = None;
    for line in reader.lines() {
        if let Ok(Entry::Name { name: n }) = serde_json::from_str(&line.ok()?) {
            name = Some(n);
        }
    }
    name
}

#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    pub path: PathBuf,
    pub title: String,
    pub modified: u64,
    pub message_count: usize,
    /// A name an extension set, overriding the derived title.
    pub name: Option<String>,
}

/// List this workspace's sessions, newest first.
pub fn list(cwd: &Path) -> Vec<SessionInfo> {
    let cwd = normalized_cwd(cwd);
    let mut dirs = vec![(home::sessions_dir().join(cwd_slug(&cwd)), false)];
    let legacy = home::sessions_dir().join(legacy_cwd_slug(&cwd));
    if legacy != dirs[0].0 {
        dirs.push((legacy, true));
    }
    let mut sessions = Vec::new();
    for (dir, verify_header) in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        sessions.extend(
            entries
                .flatten()
                .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
                .filter_map(|e| info(&e.path(), if verify_header { Some(&cwd) } else { None })),
        );
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
    sessions
}

/// The most recent session for this workspace, if any.
pub fn most_recent(cwd: &Path) -> Option<PathBuf> {
    list(cwd).into_iter().next().map(|s| s.path)
}

fn info(path: &Path, expected_cwd: Option<&Path>) -> Option<SessionInfo> {
    // Read messages and any extension-set name in one pass.
    let reader = BufReader::new(File::open(path).ok()?);
    let mut messages = Vec::new();
    let mut name = None;
    let mut session_cwd = None;
    for line in reader.lines() {
        let line = line.ok()?;
        match serde_json::from_str(&line) {
            Ok(Entry::Header { cwd, .. }) => session_cwd = Some(PathBuf::from(cwd)),
            Ok(Entry::Message { message }) => messages.push(message),
            Ok(Entry::Name { name: n }) => name = Some(n),
            Err(_) => {}
        }
    }
    if let Some(expected_cwd) = expected_cwd {
        if normalized_cwd(&session_cwd?) != expected_cwd {
            return None;
        }
    }
    let modified = std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    // A session begins with user intent. Header-only files, or malformed logs
    // containing only assistant/tool entries, never appear in /resume or -c.
    let title = messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| title_of(&message.content))?;
    Some(SessionInfo {
        path: path.to_path_buf(),
        title: name.clone().unwrap_or(title),
        modified,
        message_count: messages.len(),
        name,
    })
}

fn title_of(content: &str) -> String {
    let first_line = content.lines().next().unwrap_or("");
    let words: Vec<&str> = first_line.split_whitespace().take(8).collect();
    let title = words.join(" ");
    if title.len() > 60 {
        title.chars().take(60).collect()
    } else {
        title
    }
}
