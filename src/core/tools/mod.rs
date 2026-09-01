//! Built-in tools.
//!
//! Each tool declares an OpenAI-style function schema and runs off the async
//! runtime. Commands report pipe chunks through the same ordered session
//! stream while retaining a bounded result for the model and detail viewer.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::core::cli::ToolMode;

mod bash;
mod diffview;
mod edit;
mod fs;

/// Terminal state of one tool execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Completed,
    Failed,
    TimedOut,
    Blocked,
    Cancelled,
}

impl ToolOutcome {
    pub fn is_error(self) -> bool {
        self != Self::Completed
    }
}

/// Which command pipe produced a progress chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// Result of a tool call: text for the model, plus its display outcome.
pub struct ToolOutput {
    /// What the model reads. Kept lean on purpose: it enters the history and
    /// is resent with every later request of the session, so an echo of
    /// content the model already produced is paid for over and over.
    pub content: String,
    pub outcome: ToolOutcome,
    /// One-line summary for the transcript row, e.g. `12 lines`.
    pub summary: String,
    /// Richer text for the detail viewer only (e.g. a full diff). None means
    /// the viewer shows `content`.
    pub display: Option<String>,
}

impl ToolOutput {
    pub fn is_error(&self) -> bool {
        self.outcome.is_error()
    }

    /// The text the detail viewer shows: `display` when set, else `content`.
    pub fn display_text(&self) -> &str {
        self.display.as_deref().unwrap_or(&self.content)
    }
}

const MAX_BYTES: usize = 32 * 1024;

pub fn truncate(text: String) -> String {
    if text.len() <= MAX_BYTES {
        return text;
    }
    let mut cut = MAX_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n… [truncated: {} bytes total, cap {}]",
        &text[..cut],
        text.len(),
        MAX_BYTES
    )
}

/// Append a fixed `notice` to `body`, trimming `body` first if the two
/// together would blow the byte cap. A cap-specific explanation (e.g. "grep"
/// stopping early) must survive to the model — appending it before
/// a generic `truncate()` risked the generic "[truncated: N bytes]" marker
/// landing on top of it instead, once the hit list alone neared the cap.
pub fn truncate_with_notice(mut body: String, notice: &str) -> String {
    if body.len() + notice.len() <= MAX_BYTES {
        body.push_str(notice);
        return body;
    }
    let mut cut = MAX_BYTES.saturating_sub(notice.len());
    while cut > 0 && !body.is_char_boundary(cut) {
        cut -= 1;
    }
    body.truncate(cut);
    body.push_str(notice);
    body
}

/// The schemas advertised to the model (chat-completions `tools` shape;
/// the Responses dialect reshapes them at send time).
pub fn schemas() -> Vec<Value> {
    SPECS.iter().map(|s| (s.schema)()).collect()
}

/// Whether `name` identifies one of e's built-in tools.
pub fn is_builtin(name: &str) -> bool {
    SPECS.iter().any(|spec| spec.name == name)
}

/// Kill every shell process group still owned by this e process.
pub fn kill_tracked_processes() {
    bash::kill_tracked_processes();
}

/// Apply a run's safety mode after built-in and extension schemas have been
/// merged. Execution independently enforces the same policy.
pub fn filter_schemas(schemas: Vec<Value>, mode: ToolMode) -> Vec<Value> {
    if mode.allows() {
        schemas
    } else {
        Vec::new()
    }
}

/// Narrow the advertised schemas to a request's positive allowlist. `None`
/// leaves the set untouched. Execution enforces the same list.
pub fn restrict_to(schemas: Vec<Value>, allowed: Option<&[String]>) -> Vec<Value> {
    let Some(allowed) = allowed else {
        return schemas;
    };
    schemas
        .into_iter()
        .filter(|schema| {
            schema["function"]["name"]
                .as_str()
                .is_some_and(|name| allowed.iter().any(|a| a == name))
        })
        .collect()
}

/// Labels and category used to project one tool through its lifecycle.
#[derive(Clone, Debug)]
pub struct Presentation {
    pub category: String,
    pub running: String,
    pub completed: String,
    pub target: String,
}

/// Resolve state-aware labels without throwing away workspace-relative paths.
pub fn present(name: &str, args: &Value) -> Presentation {
    match SPECS.iter().find(|s| s.name == name) {
        Some(spec) => Presentation {
            category: spec.category.into(),
            running: spec.running.into(),
            completed: spec.completed.into(),
            target: (spec.target)(args),
        },
        None => Presentation {
            category: name.into(),
            running: format!("Running {name}"),
            completed: format!("Ran {name}"),
            target: String::new(),
        },
    }
}

/// `(name, one-line description)` for the system prompt's tools list.
pub fn snippets() -> impl Iterator<Item = (&'static str, &'static str)> {
    SPECS.iter().map(|s| (s.name, s.snippet))
}

fn target_path(args: &Value) -> String {
    sanitize_inline(args["path"].as_str().unwrap_or(""))
}
fn target_command(args: &Value) -> String {
    // A background check/kill call carries no `command` — fall back to the
    // handle so the transcript still shows what the call is about.
    let value = args["command"]
        .as_str()
        .or_else(|| args["handle"].as_str())
        .unwrap_or("");
    sanitize_inline(value)
}
fn target_pattern(args: &Value) -> String {
    sanitize_inline(args["pattern"].as_str().unwrap_or(""))
}

/// Extract the row-worthy reason after the exact tool/target prefix. Matching
/// the known boundary keeps `: ` inside a path from becoming part of the
/// reason rendered after that same path.
pub(crate) fn failure_summary(message: &str, tool: &str, target: &str) -> String {
    let prefix = if target.is_empty() {
        format!("{tool}: ")
    } else {
        format!("{tool} {target}: ")
    };
    message.strip_prefix(&prefix).unwrap_or(message).to_string()
}

/// Everything a built-in tool is, in one row: schema and runner, the
/// system-prompt one-liner, and the transcript's lifecycle labels. One
/// table so adding a tool cannot leave the prompt, the presentation, or the
/// dispatcher out of sync.
struct Spec {
    name: &'static str,
    /// One-line description for the system prompt's Available-tools list.
    snippet: &'static str,
    category: &'static str,
    running: &'static str,
    completed: &'static str,
    /// Project the transcript target out of the call arguments.
    target: fn(&Value) -> String,
    schema: fn() -> Value,
    run: fn(&Value, &Path) -> ToolOutput,
}

static SPECS: &[Spec] = &[
    Spec {
        name: "read",
        snippet: "Read the contents of a file. Use offset/limit for large files.",
        category: "read",
        running: "Reading",
        completed: "Read",
        target: target_path,
        schema: fs::read_schema,
        run: fs::read,
    },
    Spec {
        name: "write",
        snippet: "Write content to a file, creating it if needed, overwriting if it exists.",
        category: "write",
        running: "Writing",
        completed: "Wrote",
        target: target_path,
        schema: fs::write_schema,
        run: fs::write,
    },
    Spec {
        name: "edit",
        snippet: "Replace an exact string in a file; the old text must match once.",
        category: "edit",
        running: "Editing",
        completed: "Edited",
        target: target_path,
        schema: edit::schema,
        run: edit::run,
    },
    Spec {
        name: "grep",
        snippet: "Search file contents by regular expression across the workspace.",
        // The reference tallies a grep under `read` — searching is reading.
        category: "read",
        running: "Searching",
        completed: "Searched",
        target: target_pattern,
        schema: fs::grep_schema,
        run: fs::grep,
    },
    Spec {
        name: "bash",
        snippet: "Execute a bash command in the workspace root. Each call is a fresh shell — cd and variables don't persist. Use it for anything without a dedicated tool: listing directories, finding files by name, git, builds, tests. Pass background: true to start something long-lived (a server, a watcher) without blocking; check or kill it later with handle.",
        category: "command",
        running: "Running",
        completed: "Ran",
        target: target_command,
        schema: bash::schema,
        run: bash::run,
    },
];

/// Execute a named tool. `cwd` is the workspace root.
/// The `!` passthrough's entry: run a shell command through the bash tool
/// without hand-building its JSON arguments — the arg name lives in exactly
/// one place (the schema), so a caller can't drift from it again.
pub fn run_shell(command: &str, cwd: &Path) -> ToolOutput {
    let args = serde_json::json!({ "command": command }).to_string();
    run("bash", &args, cwd)
}

pub fn run(name: &str, arguments: &str, cwd: &Path) -> ToolOutput {
    run_streaming(
        name,
        arguments,
        cwd,
        &std::sync::atomic::AtomicBool::new(false),
        |_, _| {},
    )
}

/// Execute a tool, reporting command output as it is observed.
pub fn run_streaming<F>(
    name: &str,
    arguments: &str,
    cwd: &Path,
    cancel: &std::sync::atomic::AtomicBool,
    on_output: F,
) -> ToolOutput
where
    F: FnMut(OutputStream, &str),
{
    // Broken argument JSON must be reported as exactly that: falling back to
    // Null made every tool answer "missing <param>", sending the model off to
    // fix a parameter it did send instead of the JSON framing it broke.
    let args: Value = match serde_json::from_str(arguments) {
        Ok(v) => v,
        Err(_) if arguments.trim().is_empty() => Value::Null,
        Err(e) => {
            return ToolOutput {
                content: format!("tool arguments were not valid JSON: {e}"),
                outcome: ToolOutcome::Failed,
                summary: "bad arguments".into(),
                display: None,
            }
        }
    };
    if name == "bash" {
        return bash::run_streaming(&args, cwd, cancel, on_output);
    }
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return ToolOutput {
            content: "tool cancelled".into(),
            outcome: ToolOutcome::Cancelled,
            summary: "cancelled".into(),
            display: None,
        };
    }
    match SPECS.iter().find(|s| s.name == name) {
        Some(spec) => (spec.run)(&args, cwd),
        None => ToolOutput {
            content: format!("unknown tool: {name}"),
            outcome: ToolOutcome::Failed,
            summary: "unknown".into(),
            display: None,
        },
    }
}

/// Remove ANSI escape sequences — CSI (colours, cursor moves), OSC (titles,
/// hyperlinks), and two-byte ESC forms — leaving the plain text. Applied to
/// the model-facing capture and the display path alike: neither should pay
/// for (or render) colour codes.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: parameter and intermediate bytes, then one final byte.
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC: runs to BEL or the ESC \ string terminator.
                while let Some(n) = chars.next() {
                    if n == '\u{07}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next(); // ESC plus one byte (ESC 7, ESC c, …)
            }
            None => {}
        }
    }
    out
}

/// Resolve carriage-return overwrites the way a terminal would: within each
/// line only the text after the last `\r` survives, so a progress bar that
/// redrew itself a thousand times collapses to its final state instead of a
/// thousand concatenated frames.
pub fn resolve_carriage_returns(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(|line| line.rsplit('\r').next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Remove control sequences before untrusted process output reaches the TUI.
pub fn sanitize_display(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    for character in strip_ansi(text).chars() {
        match character {
            '\n' => clean.push('\n'),
            '\t' => clean.push_str("    "),
            character if !character.is_control() => clean.push(character),
            _ => {}
        }
    }
    clean
}

fn sanitize_inline(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Serializes the mutating filesystem tools' read-modify-write windows per
/// canonical path. Batch members run concurrently; without this, two edits
/// to one file both read the original and the last whole-file write
/// silently erases the other's change while both report success. Per-path
/// (not one global lock) so unrelated files never wait on each other and a
/// tool stalled on one path (dead NFS mount, Esc-detached task) can wedge
/// only that path, not every future edit and write in the process.
static FS_WRITE_HELD: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
static FS_WRITE_FREED: std::sync::Condvar = std::sync::Condvar::new();

struct PathWriteGuard {
    key: PathBuf,
}

impl Drop for PathWriteGuard {
    fn drop(&mut self) {
        let mut held = FS_WRITE_HELD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.retain(|p| p != &self.key);
        FS_WRITE_FREED.notify_all();
    }
}

fn fs_write_lock(path: &Path) -> PathWriteGuard {
    // Canonicalize the deepest existing ancestor, then append the normalized
    // missing tail. This gives a not-yet-created file the same key through
    // `new`, `./new`, `dir/../new`, and a symlinked parent.
    let key = stable_path_key(path);
    let mut held = FS_WRITE_HELD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while held.contains(&key) {
        held = FS_WRITE_FREED
            .wait(held)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    held.push(key.clone());
    PathWriteGuard { key }
}

/// Text-file tools only operate on regular files: a FIFO or device would
/// block `read_to_string` forever with Esc inert (issue: a `read` on a
/// writerless named pipe hangs the turn). Missing files pass — the caller's
/// own open reports the real error.
fn require_regular_file(path: &Path, tool: &str, shown: &str) -> Result<(), ToolOutput> {
    match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => Err(ToolOutput {
            content: format!("{tool} {shown}: not a regular file"),
            outcome: ToolOutcome::Failed,
            summary: "error".into(),
            display: None,
        }),
        _ => Ok(()),
    }
}

/// The state of each file as e last saw it (after a read, write, or edit),
/// keyed by canonical path. `edit` and `write` check against it so a file
/// that changed under them — a user's editor, a bash `sed -i`, another
/// process — fails with "changed on disk" instead of silently clobbering
/// work built on a stale copy. A file e never saw carries no record and
/// passes: the guard catches staleness, it does not impose read-before-edit.
static FS_SEEN: std::sync::Mutex<
    Option<std::collections::HashMap<PathBuf, (std::time::SystemTime, u64)>>,
> = std::sync::Mutex::new(None);

fn file_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn freshness_key(path: &Path) -> PathBuf {
    stable_path_key(path)
}

fn stable_path_key(path: &Path) -> PathBuf {
    let mut cursor = path;
    let mut tail = Vec::new();
    loop {
        if let Ok(mut existing) = cursor.canonicalize() {
            for part in tail.iter().rev() {
                existing.push(part);
            }
            return existing;
        }
        let Some(name) = cursor.file_name() else {
            return path.to_path_buf();
        };
        tail.push(name.to_os_string());
        let Some(parent) = cursor.parent() else {
            return path.to_path_buf();
        };
        cursor = parent;
    }
}

/// Record the file's current on-disk state as the one e has seen.
fn note_seen(path: &Path) {
    let Some(stamp) = file_stamp(path) else {
        return;
    };
    note_seen_stamp(path, stamp);
}

fn note_seen_stamp(path: &Path, stamp: (std::time::SystemTime, u64)) {
    let mut seen = FS_SEEN.lock().unwrap_or_else(|p| p.into_inner());
    seen.get_or_insert_with(Default::default)
        .insert(freshness_key(path), stamp);
}

/// Fail when a recorded file changed on disk since e last saw it.
fn check_fresh(path: &Path, tool: &str, shown: &str) -> Result<(), ToolOutput> {
    let recorded = {
        let seen = FS_SEEN.lock().unwrap_or_else(|p| p.into_inner());
        seen.as_ref()
            .and_then(|s| s.get(&freshness_key(path)).copied())
    };
    let Some(recorded) = recorded else {
        return Ok(());
    };
    if file_stamp(path) == Some(recorded) {
        return Ok(());
    }
    Err(ToolOutput {
        content: format!(
            "{tool} {shown}: the file changed on disk since it was last read — read it again before modifying it"
        ),
        outcome: ToolOutcome::Failed,
        summary: "stale".into(),
        display: None,
    })
}

/// Depth-first walk under `root` with the shared traversal rules — dotfiles
/// and the build/vendor directories are skipped, only regular files are
/// visited (a FIFO in the tree would hang the walk). `visit` returns false
/// to stop early (a result cap); the walk then unwinds immediately.
/// The one traversal for any file-scanning tool — a second walker with its
/// own skip rules would make "no matches" mean different things per tool.
fn walk_files(root: &Path, visit: &mut dyn FnMut(&Path) -> bool) -> bool {
    const SKIP: &[&str] = &[".git", "target", "node_modules", "dist", ".cache"];
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e.flatten().map(|e| e.path()).collect::<Vec<_>>(),
        Err(_) => return true,
    };
    for path in entries {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') || SKIP.contains(&name.as_str()) {
            continue;
        }
        // Never follow symlinks: a link cycle (`ln -s . loop`) would recurse
        // forever, and a link out of the tree would silently widen the walk.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if !walk_files(&path, visit) {
                return false;
            }
        } else if meta.is_file() && !visit(&path) {
            return false;
        }
    }
    true
}

/// Resolve a possibly-relative path against the workspace root.
fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn schema_object(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        failure_summary, filter_schemas, is_builtin, restrict_to, schemas, stable_path_key,
    };
    use crate::core::cli::ToolMode;

    fn names(mode: ToolMode) -> Vec<String> {
        filter_schemas(schemas(), mode)
            .into_iter()
            .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_string))
            .collect()
    }

    #[test]
    fn safety_modes_filter_the_model_visible_contract() {
        // No-tools sessions advertise nothing; the default run carries the
        // whole built-in toolset.
        assert!(names(ToolMode::None).is_empty());
        assert_eq!(names(ToolMode::All).len(), schemas().len());
    }

    #[test]
    fn a_request_allowlist_narrows_the_advertised_tools() {
        let allow = vec!["read".to_string(), "grep".to_string()];
        let kept = restrict_to(schemas(), Some(&allow));
        let names: Vec<String> = kept
            .into_iter()
            .filter_map(|s| s["function"]["name"].as_str().map(str::to_string))
            .collect();
        assert_eq!(names, vec!["read", "grep"]);
        // None leaves the set whole.
        assert_eq!(restrict_to(schemas(), None).len(), schemas().len());
        assert!(is_builtin("read"));
        assert!(!is_builtin("extension_tool"));
    }

    #[test]
    fn failure_summary_uses_the_known_target_boundary() {
        let target = "dir: name/file.rs";
        let message = "edit dir: name/file.rs: old_string not found: retry with more context";
        assert_eq!(
            failure_summary(message, "edit", target),
            "old_string not found: retry with more context"
        );
    }

    #[test]
    fn new_file_aliases_have_one_lock_key() {
        let root = std::env::temp_dir().join(format!(
            "e-path-key-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let real = root.join("real");
        std::fs::create_dir_all(real.join("child")).unwrap();

        let expected = stable_path_key(&real.join("new.txt"));
        assert_eq!(expected, stable_path_key(&real.join("./new.txt")));
        assert_eq!(expected, stable_path_key(&real.join("child/../new.txt")));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&real, root.join("alias")).unwrap();
            assert_eq!(expected, stable_path_key(&root.join("alias/new.txt")));
        }
        let _ = std::fs::remove_dir_all(root);
    }
}
