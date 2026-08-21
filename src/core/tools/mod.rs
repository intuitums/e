//! Built-in tools.
//!
//! Each tool declares an OpenAI-style function schema and runs synchronously
//! off the async runtime (`spawn_blocking` at the call site). Output is
//! head-truncated to a byte cap with a marker; the full text is not spilled
//! to a handle yet (that lands with ctrl+o expansion).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

mod bash;
mod edit;
mod fs;
mod skill;

/// Result of a tool call: text for the model, plus a short display line.
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
    /// One-line summary for the transcript row, e.g. `12 lines`.
    pub summary: String,
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

/// Present a tool call for the transcript: `Verb target`.
pub fn present(name: &str, args: &Value) -> (String, String) {
    let base = |p: &str| Path::new(p).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| p.into());
    match name {
        "read" => ("Read".into(), base(args["path"].as_str().unwrap_or(""))),
        "write" => ("Wrote".into(), base(args["path"].as_str().unwrap_or(""))),
        "edit" => ("Edited".into(), base(args["path"].as_str().unwrap_or(""))),
        "bash" => ("Ran".into(), args["command"].as_str().unwrap_or("").lines().next().unwrap_or("").to_string()),
        "grep" => ("Searched".into(), args["pattern"].as_str().unwrap_or("").to_string()),
        "ls" => ("Listed".into(), args["path"].as_str().unwrap_or(".").to_string()),
        "skill" => ("Skill".into(), args["name"].as_str().unwrap_or("").to_string()),
        other => (other.to_string(), String::new()),
    }
}

struct Spec {
    name: &'static str,
    schema: fn() -> Value,
    run: fn(&Value, &Path) -> ToolOutput,
}

static SPECS: &[Spec] = &[
    Spec { name: "read", schema: fs::read_schema, run: fs::read },
    Spec { name: "write", schema: fs::write_schema, run: fs::write },
    Spec { name: "edit", schema: edit::schema, run: edit::run },
    Spec { name: "ls", schema: fs::ls_schema, run: fs::ls },
    Spec { name: "grep", schema: fs::grep_schema, run: fs::grep },
    Spec { name: "bash", schema: bash::schema, run: bash::run },
    Spec { name: "skill", schema: skill::schema, run: skill::run },
];

/// Execute a named tool. `cwd` is the workspace root.
pub fn run(name: &str, arguments: &str, cwd: &Path) -> ToolOutput {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    match SPECS.iter().find(|s| s.name == name) {
        Some(spec) => (spec.run)(&args, cwd),
        None => ToolOutput {
            content: format!("unknown tool: {name}"),
            is_error: true,
            summary: "unknown".into(),
        },
    }
}

/// Resolve a possibly-relative path against the workspace root.
fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) }
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
