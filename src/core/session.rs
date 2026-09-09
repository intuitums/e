//! Sessions: an append-only JSONL log per conversation, under
//! `~/.e/sessions/<cwd-slug>/<timestamp>_<uuid>.jsonl`.
//!
//! One line per entry: a `session` header, then `message` entries carrying
//! the same ChatMessage the agent uses. The agent creates the file lazily on
//! the first user send, and listing also rejects header-only or assistant-only
//! files, so opening and closing e never counts as a session. Resume replays
//! messages back into the agent. The title is derived from the first user
//! message (first line, eight words), never model-generated.
//!
//! Every message entry also carries an `id` and its `parent` id: an
//! in-place tree, not just a line. Normal use never notices — each append
//! chains onto whatever was last written, so a plain session reads exactly
//! like the linear log it looks like. `/tree` is what makes the shape
//! visible: pick an earlier point and continue, and the new messages chain
//! onto that point's id instead of the file's last line, growing a second
//! branch in the same file. The abandoned tail is never touched — `nodes`
//! reads every branch a file holds, while `SessionLog::load` follows parents
//! from the most recently appended node and restores only that active path.
//! Records written before branching existed carry neither
//! field; `nodes` synthesizes both positionally so an old session still
//! resumes onto its real tail instead of quietly starting a second root.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::config::home;
use crate::core::providers::ChatMessage;

/// Current on-disk session format. Version 0 is the header shape written by
/// pre-release builds before the field existed; readers deliberately retain
/// support for it.
pub const FORMAT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum Entry {
    #[serde(rename = "session")]
    Header {
        #[serde(default)]
        format_version: u32,
        id: String,
        cwd: String,
        created: u64,
        model: String,
    },
    #[serde(rename = "message")]
    Message {
        /// Empty on a record written before branching existed; `nodes`
        /// synthesizes a stable id for those positionally.
        #[serde(default)]
        id: String,
        #[serde(default)]
        parent: Option<String>,
        /// Wall-clock write time, epoch milliseconds. Absent (0) on records
        /// written before timestamps existed; diagnosis of a session — where
        /// did the time and tokens go — reads this beside each message's
        /// `usage`.
        #[serde(default)]
        timestamp: u64,
        message: ChatMessage,
    },
    /// A display name an extension set via session_name — shown in /resume,
    /// overriding the title derived from the first user message.
    #[serde(rename = "name")]
    Name { name: String },
}

pub struct SessionLog {
    path: PathBuf,
    file: File,
    /// False only when an append failed and even truncating its partial tail
    /// failed. Such a log is retired permanently; later records must never be
    /// written behind a possibly torn line.
    healthy: bool,
    /// The OS releases exclusive ownership when this file handle closes.
    _lock: LockGuard,
    /// The node the next appended message attaches to as parent — the tip of
    /// whichever branch is active. None only before this file holds any
    /// message yet; `/tree` moves it to rewind without touching the file.
    current: Option<String>,
}

/// One message as an explicit tree node.
#[derive(Clone)]
pub struct Node {
    pub id: String,
    pub parent: Option<String>,
    pub message: ChatMessage,
}

/// An OS-held lock on a persistent sidecar. Never unlink it: contenders
/// must all lock the same inode, including after an owner exits or crashes.
struct LockGuard {
    _file: File,
}

impl LockGuard {
    fn acquire(session_path: &Path) -> std::io::Result<LockGuard> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(session_path.with_extension("lock"))?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "this session is already active in another e",
            ),
            std::fs::TryLockError::Error(error) => error,
        })?;
        Ok(LockGuard { _file: file })
    }
}

pub fn normalized_cwd(cwd: &Path) -> PathBuf {
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

impl SessionLog {
    /// Create a fresh session log for this workspace.
    pub fn create(cwd: &Path, model: &str) -> std::io::Result<SessionLog> {
        home::ensure()?;
        let cwd = normalized_cwd(cwd);
        let dir = home::sessions_dir().join(cwd_slug(&cwd));
        home::private_dir(&home::sessions_dir())?;
        home::private_dir(&dir)?;
        let id = uuid::Uuid::now_v7().to_string();
        let stamp = now_ms();
        let path = dir.join(format!("{stamp}_{id}.jsonl"));
        let mut options = OpenOptions::new();
        options.create_new(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        let lock = LockGuard::acquire(&path)?;
        let header = Entry::Header {
            format_version: FORMAT_VERSION,
            id,
            cwd: cwd.to_string_lossy().into_owned(),
            created: stamp,
            model: model.to_string(),
        };
        writeln!(file, "{}", serde_json::to_string(&header)?)?;
        Ok(SessionLog {
            path,
            file,
            healthy: true,
            _lock: lock,
            current: None,
        })
    }

    /// One serialized record per write call, newline included — even under
    /// an unexpected second writer, records never share a line. A failed
    /// write is lost history: callers must surface the error, not shrug.
    /// The new record's parent is wherever `current` points — the file's
    /// last line on an un-rewound session, or an earlier node right after
    /// `/tree` moved it, which is exactly how a second branch is grown.
    pub fn append(&mut self, message: &ChatMessage) -> std::io::Result<()> {
        let id = uuid::Uuid::now_v7().to_string();
        let mut line = serde_json::to_string(&Entry::Message {
            id: id.clone(),
            parent: self.current.clone(),
            timestamp: now_ms(),
            message: message.clone(),
        })?;
        line.push('\n');
        self.append_record(line.as_bytes())?;
        self.current = Some(id);
        Ok(())
    }

    /// Move the node subsequent appends attach to. `/tree` calls this with
    /// an earlier node's id to rewind: the file is untouched, the next
    /// append grows a new branch instead of extending the old tail.
    pub fn set_head(&mut self, id: Option<String>) {
        self.current = id;
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
        self.append_record(line.as_bytes())
    }

    /// Append one whole JSONL record. `write_all` may have committed a prefix
    /// before returning an error (disk full, quota, I/O failure), so roll back
    /// to the known-good end. If rollback itself fails, permanently retire the
    /// handle instead of ever turning that torn tail into interior corruption.
    fn append_record(&mut self, record: &[u8]) -> std::io::Result<()> {
        if !self.healthy {
            return Err(std::io::Error::other(
                "session log was retired after an incomplete write",
            ));
        }
        let good_len = self.file.metadata()?.len();
        if let Err(write_error) = self.file.write_all(record) {
            if let Err(rollback_error) = self.file.set_len(good_len) {
                self.healthy = false;
                return Err(std::io::Error::other(format!(
                    "{write_error}; could not remove the partial session record: {rollback_error}"
                )));
            }
            return Err(write_error);
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The stable per-conversation id — the UUID minted in `create` and
    /// recovered from the log's filename (`<stamp>_<id>.jsonl`; the stamp is
    /// digits and the id a UUID, so neither carries an underscore). Stable
    /// across resume, since `reopen` keeps the same filename. e sends this as
    /// the opaque session handle to gateways that ask for one; it encodes no
    /// user identity. Empty only for an unexpected filename shape.
    pub fn id(&self) -> &str {
        self.path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.split_once('_'))
            .map(|(_stamp, id)| id)
            .unwrap_or("")
    }

    /// Read all messages out of a session file. An interior malformed record
    /// is corruption, not noise — returning a shortened history would present
    /// lost messages as a valid conversation. The one exception is a torn
    /// final line: a crash mid-append leaves exactly that, it is the most
    /// common artifact an append-only log ever shows, and refusing to resume
    /// the whole session over it turns a lost record into a lost session.
    pub fn load(path: &Path) -> std::io::Result<Vec<ChatMessage>> {
        let nodes = SessionLog::nodes(path)?;
        let mut by_id = std::collections::HashMap::new();
        for (index, node) in nodes.iter().enumerate() {
            if by_id.insert(node.id.as_str(), index).is_some() {
                return Err(std::io::Error::other(format!(
                    "corrupt session tree: duplicate message id `{}`",
                    node.id
                )));
            }
        }

        // The last appended message is the active head. A rewind leaves the
        // abandoned tail earlier in the file and appends the replacement
        // branch afterward, so walking parent links is the durable head
        // marker without another mutable metadata record.
        let mut path_indexes = Vec::new();
        let mut cursor = nodes.last().map(|node| node.id.as_str());
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = cursor {
            if !seen.insert(id) {
                return Err(std::io::Error::other(format!(
                    "corrupt session tree: cycle at message `{id}`"
                )));
            }
            let Some(index) = by_id.get(id).copied() else {
                return Err(std::io::Error::other(format!(
                    "corrupt session tree: missing parent message `{id}`"
                )));
            };
            path_indexes.push(index);
            cursor = nodes[index].parent.as_deref();
        }
        path_indexes.reverse();
        drop(seen);
        drop(by_id);
        let mut nodes: Vec<Option<Node>> = nodes.into_iter().map(Some).collect();
        // Every index was validated against by_id above; get_mut degrades
        // to a dropped message instead of a panic if that ever breaks.
        let mut messages = path_indexes
            .into_iter()
            .filter_map(|index| {
                nodes
                    .get_mut(index)
                    .and_then(|slot| slot.take())
                    .map(|n| n.message)
            })
            .collect();
        repair_history(&mut messages);
        Ok(messages)
    }

    /// Every message in the file as an explicit tree node — every branch a
    /// session ever grew, not just the one `load` walks. A record from
    /// before branching existed (empty `id`) gets an id and parent
    /// synthesized from its position, chained onto whatever came before it,
    /// so an old session reads as the same straight line it always was.
    /// Reject broken links and cycles on every branch before exposing nodes.
    pub fn nodes(path: &Path) -> std::io::Result<Vec<Node>> {
        let reader = BufReader::new(File::open(path)?);
        let lines: Vec<String> = reader.lines().collect::<std::io::Result<_>>()?;
        let last_nonempty = lines.iter().rposition(|l| !l.trim().is_empty());
        let mut out: Vec<Node> = Vec::new();
        let mut previous: Option<String> = None;
        let mut saw_header = false;
        for (index, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Entry>(line) {
                Ok(Entry::Header { format_version, .. }) => {
                    validate_format(format_version)?;
                    saw_header = true;
                }
                Ok(Entry::Message {
                    id,
                    parent,
                    timestamp: _,
                    message,
                }) => {
                    let (id, parent) = if id.is_empty() {
                        (format!("legacy-{}", out.len()), previous.clone())
                    } else {
                        (id, parent)
                    };
                    previous = Some(id.clone());
                    out.push(Node {
                        id,
                        parent,
                        message,
                    });
                }
                Ok(_) => {}
                Err(_) if Some(index) == last_nonempty => break,
                Err(e) => {
                    return Err(std::io::Error::other(format!(
                        "corrupt session record at line {}: {e}",
                        index + 1
                    )))
                }
            }
        }
        if !saw_header {
            return Err(std::io::Error::other("session header is missing"));
        }
        let mut by_id = std::collections::HashMap::new();
        for node in &out {
            if by_id.insert(node.id.as_str(), node).is_some() {
                return Err(std::io::Error::other(
                    "corrupt session tree: duplicate message id",
                ));
            }
        }
        let mut validated = std::collections::HashSet::new();
        for node in &out {
            let mut path = std::collections::HashSet::new();
            let mut cursor = Some(node.id.as_str());
            while let Some(id) = cursor {
                if validated.contains(id) {
                    break;
                }
                if !path.insert(id) {
                    return Err(std::io::Error::other("corrupt session tree: cycle"));
                }
                cursor = by_id
                    .get(id)
                    .ok_or_else(|| {
                        std::io::Error::other("corrupt session tree: missing parent message")
                    })?
                    .parent
                    .as_deref();
            }
            validated.extend(path);
        }
        Ok(out)
    }

    /// Re-open an existing session for appending. Fails while another
    /// process owns the session's lock. A torn final record — what a crash
    /// mid-append leaves — is truncated away first: `nodes` skips it on
    /// read, but appending behind it would fuse the next record onto the
    /// torn bytes, and that fused line becomes interior corruption that
    /// turns one lost record into an unresumable session. The reopened
    /// handle then picks up exactly where the file's last branch left off —
    /// reading the file once here is what lets a resumed session keep
    /// growing that branch instead of quietly starting a second root next
    /// to it.
    pub fn reopen(path: &Path) -> std::io::Result<SessionLog> {
        home::ensure()?;
        let lock = LockGuard::acquire(path)?;
        let file = OpenOptions::new().append(true).open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = file.metadata()?.permissions().mode() & 0o600;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        file.set_len(intact_len(path)?)?;
        let current = SessionLog::nodes(path)
            .ok()
            .and_then(|nodes| nodes.last().map(|n| n.id.clone()));
        Ok(SessionLog {
            path: path.to_path_buf(),
            file,
            healthy: true,
            _lock: lock,
            current,
        })
    }
}

/// The file's length up to the end of its last intact record: the whole
/// file unless the last non-empty line fails to parse — the torn tail
/// `nodes` tolerates, cut back to where it starts.
fn intact_len(path: &Path) -> std::io::Result<u64> {
    let bytes = std::fs::read(path)?;
    let Some(end) = bytes.iter().rposition(|b| !b.is_ascii_whitespace()) else {
        return Ok(bytes.len() as u64);
    };
    let start = bytes[..end]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |newline| newline + 1);
    match serde_json::from_slice::<Entry>(&bytes[start..=end]) {
        Ok(_) => Ok(bytes.len() as u64),
        Err(_) => Ok(start as u64),
    }
}

fn validate_format(version: u32) -> std::io::Result<()> {
    if version <= FORMAT_VERSION {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "session format {version} is newer than this e supports ({FORMAT_VERSION})"
        )))
    }
}

/// Make a history replayable when a crash cut it mid-record. A reasoning
/// block with no assistant turn after it fails signature replay and is
/// dropped; an assistant tool call whose result never got appended is a
/// dangling tool_use every dialect rejects and gets an honest synthetic
/// result right after its batch, so the content survives. The whole
/// history is scanned, not just the tail: the repair lives in memory only,
/// and the next prompt is appended behind the unrepaired records, so from
/// the second resume on (and on a `/tree` rewind to that prompt) the hole
/// sits in the middle of the file.
pub fn repair_history(messages: &mut Vec<ChatMessage>) {
    // A reasoning block belongs to the next non-reasoning message, which
    // must be its assistant turn.
    let mut follower = None;
    let mut kept = Vec::with_capacity(messages.len());
    for message in std::mem::take(messages).into_iter().rev() {
        let role = message.role();
        if role != "reasoning" {
            follower = Some(role);
            kept.push(message);
        } else if follower == Some("assistant") {
            kept.push(message);
        }
    }
    kept.reverse();
    // Answer every call still open when its batch of results ends.
    let mut unanswered: Vec<String> = Vec::new();
    let synthetic = |id: String| {
        ChatMessage::tool_result(
            id,
            "not executed — the session ended before this call completed",
        )
    };
    for message in kept {
        if let Some(id) = message.tool_call_id() {
            unanswered.retain(|open| open != id);
        } else {
            messages.extend(unanswered.drain(..).map(synthetic));
            unanswered = message.tool_calls().iter().map(|c| c.id.clone()).collect();
        }
        messages.push(message);
    }
    messages.extend(unanswered.into_iter().map(synthetic));
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
    /// User messages only — the picker's "N turns" count.
    pub user_turns: usize,
    /// The workspace the session was recorded in, from its header.
    pub cwd: PathBuf,
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

/// Every saved session across all workspaces, newest first — the resume
/// picker's "All workspaces" scope.
pub fn list_all() -> Vec<SessionInfo> {
    let Ok(workspaces) = std::fs::read_dir(home::sessions_dir()) else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for workspace in workspaces.flatten() {
        let Ok(entries) = std::fs::read_dir(workspace.path()) else {
            continue;
        };
        sessions.extend(
            entries
                .flatten()
                .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
                .filter_map(|e| info(&e.path(), None)),
        );
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.modified));
    sessions
}

fn info(path: &Path, expected_cwd: Option<&Path>) -> Option<SessionInfo> {
    // One lean pass over the log. Listing needs the header, the name, the
    // message and user-turn counts, and the first user line for the title —
    // not the full content of every recorded tool result. A full ChatMessage
    // parse materializes megabytes of content per session; the lean shape
    // tokenizes the same lines without allocating them, so /resume across
    // every workspace stays fast. Only the title line is parsed in full.
    #[derive(Deserialize)]
    struct LeanMessage {
        role: String,
        #[serde(default)]
        internal: bool,
    }
    #[derive(Deserialize)]
    #[serde(tag = "type")]
    enum LeanEntry {
        #[serde(rename = "session")]
        Header {
            #[serde(default)]
            format_version: u32,
            cwd: String,
        },
        #[serde(rename = "message")]
        Message { message: LeanMessage },
        #[serde(rename = "name")]
        Name { name: String },
    }
    let reader = BufReader::new(File::open(path).ok()?);
    let mut message_count = 0usize;
    let mut user_turns = 0usize;
    let mut name = None;
    let mut session_cwd = None;
    let mut title: Option<String> = None;
    for line in reader.lines() {
        let line = line.ok()?;
        match serde_json::from_str::<LeanEntry>(&line) {
            Ok(LeanEntry::Header {
                cwd,
                format_version,
            }) => {
                // Same contract as the full parse: a header this build
                // cannot read means the whole file is unreadable.
                if validate_format(format_version).is_err() {
                    return None;
                }
                session_cwd = Some(PathBuf::from(cwd));
            }
            Ok(LeanEntry::Message { message }) => {
                message_count += 1;
                if message.role != "user" {
                    continue;
                }
                // Harness-authored messages (steering, continuations) fill
                // the log as user rows but are not user turns.
                if !message.internal {
                    user_turns += 1;
                }
                if title.is_none() {
                    title = match serde_json::from_str::<Entry>(&line) {
                        Ok(Entry::Message { message, .. }) => Some(title_of(&message.content)),
                        _ => None,
                    };
                }
            }
            Ok(LeanEntry::Name { name: n }) => name = Some(n),
            Err(_) => {}
        }
    }
    let session_cwd = session_cwd.unwrap_or_default();
    if let Some(expected_cwd) = expected_cwd {
        if session_cwd.as_os_str().is_empty() || normalized_cwd(&session_cwd) != expected_cwd {
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
    let title = title?;
    Some(SessionInfo {
        path: path.to_path_buf(),
        title: name.clone().unwrap_or(title),
        modified,
        message_count,
        user_turns,
        cwd: session_cwd,
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn listing_counts_user_turns_not_harness_messages() {
        let path = std::env::temp_dir().join(format!(
            "e-session-turns-{}-{}.jsonl",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let mut steered = ChatMessage::user("steering echo");
        steered.mark_internal();
        let entries = [
            Entry::Header {
                format_version: FORMAT_VERSION,
                id: "session".into(),
                cwd: "/tmp".into(),
                created: 0,
                model: "test/model".into(),
            },
            Entry::Message {
                id: "a".into(),
                parent: None,
                timestamp: 0,
                message: ChatMessage::user("the real prompt"),
            },
            Entry::Message {
                id: "b".into(),
                parent: Some("a".into()),
                timestamp: 0,
                message: steered,
            },
            Entry::Message {
                id: "c".into(),
                parent: Some("b".into()),
                timestamp: 0,
                message: ChatMessage::assistant("reply", Vec::new()),
            },
        ];
        let body = entries
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{body}\n")).unwrap();

        let info = info(&path, None).unwrap();
        assert_eq!(info.user_turns, 1);
        assert_eq!(info.message_count, 3);
        assert_eq!(info.title, "the real prompt");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_restores_only_the_most_recent_branch() {
        let path = std::env::temp_dir().join(format!(
            "e-session-branch-{}-{}.jsonl",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let entries = [
            Entry::Header {
                format_version: FORMAT_VERSION,
                id: "session".into(),
                cwd: "/tmp".into(),
                created: 0,
                model: "test/model".into(),
            },
            Entry::Message {
                id: "root".into(),
                parent: None,
                timestamp: 0,
                message: ChatMessage::user("root"),
            },
            Entry::Message {
                id: "abandoned".into(),
                parent: Some("root".into()),
                timestamp: 0,
                message: ChatMessage::assistant("old tail", Vec::new()),
            },
            Entry::Message {
                id: "branch".into(),
                parent: Some("root".into()),
                timestamp: 0,
                message: ChatMessage::user("new branch"),
            },
            Entry::Message {
                id: "head".into(),
                parent: Some("branch".into()),
                timestamp: 0,
                message: ChatMessage::assistant("new answer", Vec::new()),
            },
        ];
        let body = entries
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{body}\n")).unwrap();

        let loaded = SessionLog::load(&path).unwrap();
        let content = loaded
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>();
        assert_eq!(content, vec!["root", "new branch", "new answer"]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_log_is_retired_when_a_failed_append_cannot_be_rolled_back() {
        // /dev/full rejects writes and truncation, exercising the otherwise
        // difficult disk-full + failed-rollback path deterministically.
        let file = OpenOptions::new().append(true).open("/dev/full").unwrap();
        let lock_path = std::env::temp_dir().join(format!(
            "e-dev-full-{}-{}.lock",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let mut session = SessionLog {
            path: PathBuf::from("/dev/full"),
            file,
            healthy: true,
            _lock: LockGuard::acquire(&lock_path).unwrap(),
            current: None,
        };

        let first = session.append(&ChatMessage::user("lost")).unwrap_err();
        assert!(first.to_string().contains("could not remove the partial"));
        let second = session
            .append(&ChatMessage::user("must not append"))
            .unwrap_err();
        assert!(second.to_string().contains("retired"));
        drop(session);
        std::fs::remove_file(lock_path).unwrap();
    }
}
