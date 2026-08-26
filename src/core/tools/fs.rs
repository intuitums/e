//! Filesystem tools: read, write, grep.

use serde_json::{json, Value};
use std::path::Path;

use super::{resolve, schema_object, truncate, truncate_with_notice, ToolOutcome, ToolOutput};

fn ok(content: String, summary: String) -> ToolOutput {
    ToolOutput {
        content,
        outcome: ToolOutcome::Completed,
        summary,
        display: None,
    }
}
fn err(message: String) -> ToolOutput {
    ToolOutput {
        content: message,
        outcome: ToolOutcome::Failed,
        summary: "error".into(),
        display: None,
    }
}

pub fn read_schema() -> Value {
    schema_object(
        "read",
        "Read a UTF-8 text file. Each returned line is prefixed with its 1-based line number and a tab; the prefix is not part of the file. Use offset/limit to window large files.",
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
    // Reads and mutations share the same stable path lock. Record the stamp
    // paired with the bytes we actually return, not a later independent stat.
    let _guard = super::fs_write_lock(&full);
    if let Err(output) = super::require_regular_file(&full, "read", path) {
        return output;
    }
    let mut stable = None;
    for _ in 0..2 {
        let before = super::file_stamp(&full);
        let text = match std::fs::read_to_string(&full) {
            Ok(t) => t,
            Err(e) => return err(format!("read {path}: {e}")),
        };
        let after = super::file_stamp(&full);
        if before.is_some() && before == after {
            stable = Some((text, after.unwrap()));
            break;
        }
    }
    let Some((text, stamp)) = stable else {
        return err(format!(
            "read {path}: the file changed while it was being read"
        ));
    };
    super::note_seen_stamp(&full, stamp);
    let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
    let limit = args["limit"].as_u64().map(|n| n as usize);
    let lines: Vec<&str> = text.lines().collect();
    let start = offset - 1;
    // Numbered lines: the model can quote "line 42" from compiler output
    // straight back at the file, and a windowed read says where it sits.
    let slice: Vec<String> = lines
        .iter()
        .enumerate()
        .skip(start)
        .take(limit.unwrap_or(usize::MAX))
        .map(|(i, line)| format!("{}\t{}", i + 1, line))
        .collect();
    let count = slice.len();
    ok(truncate(slice.join("\n")), format!("{count} lines"))
}

pub fn write_schema() -> Value {
    schema_object(
        "write",
        "Write content to a file, creating parent directories. Overwrites the existing file; fails if the file changed on disk since it was last read.",
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
    if let Err(output) = super::require_regular_file(&full, "write", path) {
        return output;
    }
    // Same per-path lock as edit: a concurrent mutation through any spelling
    // of this path must finish before this overwrite starts.
    let _guard = super::fs_write_lock(&full);
    if let Err(output) = super::check_fresh(&full, "write", path) {
        return output;
    }
    let before = std::fs::read_to_string(&full).unwrap_or_default();
    if let Some(parent) = full.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&full, content) {
        Ok(()) => {
            super::note_seen(&full);
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
            // The model wrote this content one message ago — echoing it back
            // would bill the whole file into the context a second time (and
            // again on every later request). The full diff goes to the
            // detail viewer instead.
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
            ToolOutput {
                content: format!("wrote {path} ({} lines)", content.lines().count()),
                outcome: ToolOutcome::Completed,
                summary: format!("+{additions} -{deletions}"),
                display: Some(truncate(detail.trim_end().to_string())),
            }
        }
        Err(e) => err(format!("write {path}: {e}")),
    }
}

pub fn grep_schema() -> Value {
    schema_object(
        "grep",
        "Search file contents for a regular expression, workspace-wide. Traversal skips dotfiles and .git/target/node_modules/dist/.cache (an explicitly named file is always searched), and stops at 200 matches — the result says so when capped.",
        json!({
            "pattern": {"type": "string"},
            "path": {"type": "string", "description": "Directory or file to search (default: workspace root)"},
            "glob": {"type": "string", "description": "Restrict the search to files matching this glob (`*`, `?`, `**`), e.g. `*.rs` or `src/**/*.json`. Matched against the file name alone when it has no slash, otherwise the workspace-relative path — same rule the old `find` tool used."}
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
    // A glob with no slash matches the bare file name anywhere in the tree;
    // one with a slash addresses the workspace-relative path itself.
    let glob = match args["glob"].as_str() {
        Some(g) => match glob_regex(g) {
            Ok(r) => Some((r, !g.contains('/'))),
            Err(e) => return err(format!("grep: bad glob: {e}")),
        },
        None => None,
    };
    let root = resolve(cwd, args["path"].as_str().unwrap_or("."));
    let mut hits = Vec::new();
    let mut count = 0usize;
    if root.is_dir() {
        super::walk_files(&root, &mut |path| {
            if glob_allows(glob.as_ref(), path, cwd) {
                search_file(path, cwd, &re, &mut hits, &mut count);
            }
            count < MATCH_CAP
        });
    } else {
        // An explicitly requested file is searched as asked — the dotfile
        // skip rule is a traversal heuristic, not a veto over `.env`, and
        // `glob` narrows a directory walk, not an explicit single-file ask.
        search_file(&root, cwd, &re, &mut hits, &mut count);
    }
    if hits.is_empty() {
        return ok(String::new(), "0 matches".into());
    }
    // A capped search must say so: "200 matches" alone reads as an exact
    // count, and the model can't tell a complete result from a stopped one.
    let body = hits.join("\n");
    let (content, summary) = if count >= MATCH_CAP {
        let notice = format!(
            "\n… [stopped at {MATCH_CAP} matches — narrow the pattern or path to see the rest]"
        );
        (
            truncate_with_notice(body, &notice),
            format!("{count}+ matches"),
        )
    } else {
        (truncate(body), format!("{count} matches"))
    };
    ok(content, summary)
}

/// Matches after which a search stops rather than flooding the context.
const MATCH_CAP: usize = 200;

/// Whether a walked file passes grep's optional `glob` filter — no filter
/// always passes. `by_name` mirrors the old `find` tool's rule: a pattern
/// with no slash matches the bare file name, one with a slash matches the
/// workspace-relative path.
fn glob_allows(glob: Option<&(regex::Regex, bool)>, path: &Path, cwd: &Path) -> bool {
    let Some((re, by_name)) = glob else {
        return true;
    };
    let candidate = if *by_name {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        path.strip_prefix(cwd).unwrap_or(path).display().to_string()
    };
    re.is_match(&candidate)
}

/// Compile a glob into an anchored regex: `*` matches within one path
/// segment, `?` one character, `**` across segments (`**/` also matches the
/// empty prefix so `**/x` finds a root-level `x`).
fn glob_regex(pattern: &str) -> Result<regex::Regex, regex::Error> {
    let mut re = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                if chars.peek() == Some(&'*') {
                    chars.next();
                    if chars.peek() == Some(&'/') {
                        chars.next();
                        re.push_str("(?:.*/)?");
                    } else {
                        re.push_str(".*");
                    }
                } else {
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re)
}

fn search_file(
    path: &Path,
    cwd: &Path,
    re: &regex::Regex,
    hits: &mut Vec<String>,
    count: &mut usize,
) {
    // Only regular files: a FIFO in the tree would block the walk forever.
    if !std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
    {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let rel = path.strip_prefix(cwd).unwrap_or(path).display().to_string();
    for (n, line) in text.lines().enumerate() {
        if re.is_match(line) {
            hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            *count += 1;
            if *count >= MATCH_CAP {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::glob_regex;

    #[test]
    fn glob_semantics() {
        let m = |p: &str, s: &str| glob_regex(p).unwrap().is_match(s);
        assert!(m("*.rs", "main.rs"));
        assert!(!m("*.rs", "src/main.rs"), "* must not cross a separator");
        assert!(m("src/**/*.json", "src/a/b/c.json"));
        assert!(m("**/Cargo.toml", "Cargo.toml"), "**/ matches empty prefix");
        assert!(m("a?c.txt", "abc.txt"));
        assert!(!m("a?c.txt", "a/c.txt"));
        assert!(m("**", "any/depth/at/all"));
    }
}
