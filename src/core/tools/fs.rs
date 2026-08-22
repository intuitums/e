//! Filesystem tools: read, write, ls, grep.

use serde_json::{json, Value};
use std::path::Path;

use super::{resolve, schema_object, truncate, ToolOutcome, ToolOutput};

fn ok(content: String, summary: String) -> ToolOutput {
    ToolOutput {
        content,
        outcome: ToolOutcome::Completed,
        summary,
    }
}
fn err(message: String) -> ToolOutput {
    ToolOutput {
        content: message,
        outcome: ToolOutcome::Failed,
        summary: "error".into(),
    }
}

pub fn read_schema() -> Value {
    schema_object(
        "read",
        "Read a UTF-8 text file. Optional 1-based line offset and count.",
        json!({
            "path": {"type": "string", "description": "File path, absolute or workspace-relative"},
            "offset": {"type": "integer", "description": "1-based first line"},
            "limit": {"type": "integer", "description": "Max lines to return"}
        }),
        &["path"],
    )
}

pub fn read(args: &Value, cwd: &Path) -> ToolOutput {
    let Some(path) = args["path"].as_str() else {
        return err("read: missing path".into());
    };
    let full = resolve(cwd, path);
    let text = match std::fs::read_to_string(&full) {
        Ok(t) => t,
        Err(e) => return err(format!("read {path}: {e}")),
    };
    let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = args["limit"].as_u64().map(|n| n as usize);
    let lines: Vec<&str> = text.lines().collect();
    let start = offset - 1;
    let slice: Vec<&str> = match limit {
        Some(n) => lines.iter().skip(start).take(n).copied().collect(),
        None => lines.iter().skip(start).copied().collect(),
    };
    let count = slice.len();
    ok(truncate(slice.join("\n")), format!("{count} lines"))
}

pub fn write_schema() -> Value {
    schema_object(
        "write",
        "Write content to a file, creating parent directories. Overwrites.",
        json!({
            "path": {"type": "string"},
            "content": {"type": "string"}
        }),
        &["path", "content"],
    )
}

pub fn write(args: &Value, cwd: &Path) -> ToolOutput {
    let Some(path) = args["path"].as_str() else {
        return err("write: missing path".into());
    };
    let content = args["content"].as_str().unwrap_or("");
    let full = resolve(cwd, path);
    let before = std::fs::read_to_string(&full).unwrap_or_default();
    if let Some(parent) = full.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&full, content) {
        Ok(()) => {
            let additions = if before == content {
                0
            } else {
                content.lines().count()
            };
            let deletions = if before.is_empty() || before == content {
                0
            } else {
                before.lines().count()
            };
            let mut detail = format!("wrote {path}");
            if before != content {
                if !before.is_empty() {
                    detail.push_str("\n--- before\n");
                    for line in before.lines() {
                        detail.push_str(&format!("-{line}\n"));
                    }
                }
                if !content.is_empty() {
                    detail.push_str("+++ after\n");
                    for line in content.lines() {
                        detail.push_str(&format!("+{line}\n"));
                    }
                }
            }
            ok(
                truncate(detail.trim_end().to_string()),
                format!("+{additions} -{deletions}"),
            )
        }
        Err(e) => err(format!("write {path}: {e}")),
    }
}

pub fn ls_schema() -> Value {
    schema_object(
        "ls",
        "List a directory's entries.",
        json!({"path": {"type": "string", "description": "Directory (default: workspace root)"}}),
        &[],
    )
}

pub fn ls(args: &Value, cwd: &Path) -> ToolOutput {
    let path = args["path"].as_str().unwrap_or(".");
    let full = resolve(cwd, path);
    let entries = match std::fs::read_dir(&full) {
        Ok(e) => e,
        Err(e) => return err(format!("ls {path}: {e}")),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    names.sort();
    let count = names.len();
    ok(names.join("\n"), format!("{count} entries"))
}

pub fn grep_schema() -> Value {
    schema_object(
        "grep",
        "Search file contents for a regular expression, workspace-wide.",
        json!({
            "pattern": {"type": "string"},
            "path": {"type": "string", "description": "Directory or file to search (default: workspace root)"}
        }),
        &["pattern"],
    )
}

pub fn grep(args: &Value, cwd: &Path) -> ToolOutput {
    let Some(pattern) = args["pattern"].as_str() else {
        return err("grep: missing pattern".into());
    };
    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => return err(format!("grep: bad pattern: {e}")),
    };
    let root = resolve(cwd, args["path"].as_str().unwrap_or("."));
    let mut hits = Vec::new();
    let mut count = 0usize;
    walk(&root, cwd, &re, &mut hits, &mut count);
    if hits.is_empty() {
        return ok(String::new(), "0 matches".into());
    }
    ok(truncate(hits.join("\n")), format!("{count} matches"))
}

fn walk(dir: &Path, cwd: &Path, re: &regex::Regex, hits: &mut Vec<String>, count: &mut usize) {
    const SKIP: &[&str] = &[".git", "target", "node_modules", "dist", ".cache"];
    if *count >= 200 {
        return;
    }
    let entries = if dir.is_dir() {
        match std::fs::read_dir(dir) {
            Ok(e) => e.flatten().map(|e| e.path()).collect::<Vec<_>>(),
            Err(_) => return,
        }
    } else {
        vec![dir.to_path_buf()]
    };
    for path in entries {
        if *count >= 200 {
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with('.') || SKIP.contains(&name.as_str()) {
            continue;
        }
        if path.is_dir() {
            walk(&path, cwd, re, hits, count);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            let rel = path
                .strip_prefix(cwd)
                .unwrap_or(&path)
                .display()
                .to_string();
            for (n, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                    *count += 1;
                    if *count >= 200 {
                        return;
                    }
                }
            }
        }
    }
}
