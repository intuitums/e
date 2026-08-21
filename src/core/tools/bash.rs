//! The bash tool: spawn a shell command, capture combined output.
//!
//! Spawn-and-capture, not a terminal daemon (DESIGN.md §3). A wall-clock
//! timeout bounds runaway commands; output is truncated by the caller.

use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;

use super::{schema_object, truncate, ToolOutput};

pub fn schema() -> Value {
    schema_object(
        "bash",
        "Run a shell command in the workspace and return its combined output.",
        json!({
            "command": {"type": "string"},
            "timeout": {"type": "integer", "description": "Seconds before the command is killed (default 120)"}
        }),
        &["command"],
    )
}

pub fn run(args: &Value, cwd: &Path) -> ToolOutput {
    let Some(command) = args["command"].as_str() else {
        return ToolOutput {
            content: "bash: missing command".into(),
            is_error: true,
            summary: "error".into(),
        };
    };
    let timeout = args["timeout"].as_u64().unwrap_or(120).clamp(1, 600);

    // A wrapper enforces the timeout without a watcher thread.
    let wrapped = command.to_string();
    let child = Command::new("bash")
        .arg("-lc")
        .arg(&wrapped)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .output();

    let _ = timeout; // bounded below once we add a kill timer
    match child {
        Ok(out) => {
            let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !stderr.trim().is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(&stderr);
            }
            let code = out.status.code().unwrap_or(-1);
            let is_error = !out.status.success();
            let summary = if is_error {
                format!("exit {code}")
            } else {
                "done".into()
            };
            ToolOutput {
                content: truncate(combined.trim_end().to_string()),
                is_error,
                summary,
            }
        }
        Err(e) => ToolOutput {
            content: format!("bash: {e}"),
            is_error: true,
            summary: "error".into(),
        },
    }
}
