//! Built-in tools.
//!
//! Each tool declares an OpenAI-style function schema and runs off the async
//! runtime. Commands report pipe chunks through the same ordered session
//! stream while retaining a bounded result for the model and detail viewer.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

mod bash;
mod edit;
mod fs;
mod skill;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// Result of a tool call: text for the model, plus its display outcome.
pub struct ToolOutput {
    pub content: String,
    pub outcome: ToolOutcome,
    /// One-line summary for the transcript row, e.g. `12 lines`.
    pub summary: String,
}

impl ToolOutput {
    pub fn is_error(&self) -> bool {
        self.outcome.is_error()
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

/// The schemas advertised to the model (chat-completions `tools` shape;
/// the Responses dialect reshapes them at send time).
pub fn schemas() -> Vec<Value> {
    SPECS.iter().map(|s| (s.schema)()).collect()
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
    let path = || sanitize_inline(args["path"].as_str().unwrap_or(""));
    let command = || sanitize_inline(args["command"].as_str().unwrap_or(""));
    match name {
        "read" => Presentation {
            category: "read".into(),
            running: "Reading".into(),
            completed: "Read".into(),
            target: path(),
        },
        "write" => Presentation {
            category: "write".into(),
            running: "Writing".into(),
            completed: "Wrote".into(),
            target: path(),
        },
        "edit" => Presentation {
            category: "edit".into(),
            running: "Editing".into(),
            completed: "Edited".into(),
            target: path(),
        },
        "bash" => Presentation {
            category: "command".into(),
            running: "Running".into(),
            completed: "Ran".into(),
            target: command(),
        },
        "grep" => Presentation {
            category: "search".into(),
            running: "Searching".into(),
            completed: "Searched".into(),
            target: sanitize_inline(args["pattern"].as_str().unwrap_or("")),
        },
        "ls" => Presentation {
            category: "list".into(),
            running: "Listing".into(),
            completed: "Listed".into(),
            target: sanitize_inline(args["path"].as_str().unwrap_or(".")),
        },
        "skill" => Presentation {
            category: "skill".into(),
            running: "Loading".into(),
            completed: "Loaded".into(),
            target: sanitize_inline(args["name"].as_str().unwrap_or("")),
        },
        other => Presentation {
            category: other.into(),
            running: format!("Running {other}"),
            completed: format!("Ran {other}"),
            target: String::new(),
        },
    }
}

struct Spec {
    name: &'static str,
    schema: fn() -> Value,
    run: fn(&Value, &Path) -> ToolOutput,
}

static SPECS: &[Spec] = &[
    Spec {
        name: "read",
        schema: fs::read_schema,
        run: fs::read,
    },
    Spec {
        name: "write",
        schema: fs::write_schema,
        run: fs::write,
    },
    Spec {
        name: "edit",
        schema: edit::schema,
        run: edit::run,
    },
    Spec {
        name: "ls",
        schema: fs::ls_schema,
        run: fs::ls,
    },
    Spec {
        name: "grep",
        schema: fs::grep_schema,
        run: fs::grep,
    },
    Spec {
        name: "bash",
        schema: bash::schema,
        run: bash::run,
    },
    Spec {
        name: "skill",
        schema: skill::schema,
        run: skill::run,
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
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    if name == "bash" {
        return bash::run_streaming(&args, cwd, cancel, on_output);
    }
    if cancel.load(std::sync::atomic::Ordering::SeqCst) {
        return ToolOutput {
            content: "tool cancelled".into(),
            outcome: ToolOutcome::Cancelled,
            summary: "cancelled".into(),
        };
    }
    match SPECS.iter().find(|s| s.name == name) {
        Some(spec) => (spec.run)(&args, cwd),
        None => ToolOutput {
            content: format!("unknown tool: {name}"),
            outcome: ToolOutcome::Failed,
            summary: "unknown".into(),
        },
    }
}

/// Remove control sequences before untrusted process output reaches the TUI.
pub fn sanitize_display(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    for character in text.chars() {
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
