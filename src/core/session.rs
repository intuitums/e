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
use crate::core::provider::ChatMessage;

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
        let header = Entry::Header {
            id,
            cwd: cwd.to_string_lossy().into_owned(),
            created: stamp,
            model: model.to_string(),
        };
        writeln!(file, "{}", serde_json::to_string(&header)?)?;
        Ok(Session { path, file })
    }

    pub fn append(&mut self, message: &ChatMessage) {
        if let Ok(line) = serde_json::to_string(&Entry::Message {
            message: message.clone(),
        }) {
            let _ = writeln!(self.file, "{line}");
        }
    }

    /// Set the display name e shows for this session. Idempotent; appends a
    /// name entry, so the most recent name wins on resume.
    pub fn set_name(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Ok(line) = serde_json::to_string(&Entry::Name {
            name: name.to_string(),
        }) {
            let _ = writeln!(self.file, "{line}");
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read all messages out of a session file.
    pub fn load(path: &Path) -> std::io::Result<Vec<ChatMessage>> {
        let reader = BufReader::new(File::open(path)?);
        let mut messages = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if let Ok(Entry::Message { message }) = serde_json::from_str(&line) {
                messages.push(message);
            }
        }
        Ok(messages)
    }

    /// Re-open an existing session for appending.
    pub fn reopen(path: &Path) -> std::io::Result<Session> {
        let file = OpenOptions::new().append(true).open(path)?;
        Ok(Session {
            path: path.to_path_buf(),
            file,
        })
    }
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
