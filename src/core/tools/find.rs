//! The find tool: locate files by name with a glob pattern.
//!
//! Content search (grep) can't answer "where is the file called X" without
//! bash gymnastics; this closes that gap. Same traversal rules as grep —
//! dotfiles and the build/vendor directories are skipped — and the same
//! honesty rule: a capped result says it was capped.

use serde_json::{json, Value};
use std::path::Path;

use super::{resolve, schema_object, truncate, truncate_with_notice, ToolOutcome, ToolOutput};

/// Results after which the walk stops rather than flooding the context.
const RESULT_CAP: usize = 500;

pub fn schema() -> Value {
    schema_object(
        "find",
        "Find files by name. The pattern is a glob (`*`, `?`, `**`) matched against the workspace-relative path, or against the file name alone when it contains no slash. Traversal skips dotfiles and .git/target/node_modules/dist/.cache, and stops at 500 results — the result says so when capped.",
        json!({
            "pattern": {"type": "string", "description": "Glob, e.g. `*.rs`, `src/**/*.json`, `Cargo.toml`"},
            "path": {"type": "string", "description": "Directory to search (default: workspace root)"}
        }),
        &["pattern"],
    )
}

pub fn run(args: &Value, cwd: &Path) -> ToolOutput {
    let err = |m: String| ToolOutput {
        content: m,
        outcome: ToolOutcome::Failed,
        summary: "error".into(),
        display: None,
    };
    let Some(pattern) = args["pattern"].as_str() else {
        return err("find: missing pattern".into());
    };
    let re = match glob_regex(pattern) {
        Ok(r) => r,
        Err(e) => return err(format!("find: bad pattern: {e}")),
    };
    // A bare name matches anywhere in the tree; a pattern with a slash
    // addresses the relative path itself.
    let by_name = !pattern.contains('/');
    let root = resolve(cwd, args["path"].as_str().unwrap_or("."));
    let mut hits: Vec<String> = Vec::new();
    super::walk_files(&root, &mut |path| {
        let rel = path.strip_prefix(cwd).unwrap_or(path).display().to_string();
        let candidate = if by_name {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            rel.clone()
        };
        if re.is_match(&candidate) {
            hits.push(rel);
        }
        hits.len() < RESULT_CAP
    });
    hits.sort();
    let count = hits.len();
    let body = hits.join("\n");
    let (content, summary) = if count >= RESULT_CAP {
        let notice = format!(
            "\n… [stopped at {RESULT_CAP} results — narrow the pattern or path to see the rest]"
        );
        (
            truncate_with_notice(body, &notice),
            format!("{count}+ files"),
        )
    } else {
        (truncate(body), format!("{count} files"))
    };
    ToolOutput {
        content,
        outcome: ToolOutcome::Completed,
        summary,
        display: None,
    }
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
