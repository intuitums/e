//! Sessions: an append-only JSONL log per conversation, under
//! `~/.e/sessions/<cwd-slug>/<timestamp>_<uuid>.jsonl`.
//!
//! One line per entry: a `session` header, then `message` entries carrying
//! the same ChatMessage the agent uses. Resume replays the messages back into
//! the agent. The title is derived from the first user message (first line,
//! eight words) — never model-generated.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::home;
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
}

pub struct Session {
    path: PathBuf,
    file: File,
}

/// `/Users/x/proj` → `--Users-x-proj--`, matching the established scheme.
fn cwd_slug(cwd: &Path) -> String {
    let joined = cwd.to_string_lossy().replace('/', "-");
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
        let dir = home::sessions_dir().join(cwd_slug(cwd));
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

pub struct SessionInfo {
    pub path: PathBuf,
    pub title: String,
    pub modified: u64,
    pub message_count: usize,
}

/// List this workspace's sessions, newest first.
pub fn list(cwd: &Path) -> Vec<SessionInfo> {
    let dir = home::sessions_dir().join(cwd_slug(cwd));
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut sessions: Vec<SessionInfo> = entries
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
        .filter_map(|e| info(&e.path()))
        .collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
    sessions
}

/// The most recent session for this workspace, if any.
pub fn most_recent(cwd: &Path) -> Option<PathBuf> {
    list(cwd).into_iter().next().map(|s| s.path)
}

fn info(path: &Path) -> Option<SessionInfo> {
    let messages = Session::load(path).ok()?;
    let modified = std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    let title = messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| title_of(&m.content))
        .unwrap_or_else(|| "Untitled".into());
    Some(SessionInfo {
        path: path.to_path_buf(),
        title,
        modified,
        message_count: messages.len(),
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
