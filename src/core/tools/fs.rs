//! Filesystem tools: read, write, grep.

use serde_json::{json, Value};
use std::io::{self, BufRead, BufReader};
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
        let text = match read_window(&full, args["offset"].as_u64(), args["limit"].as_u64()) {
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
    let count = text.lines().count();
    ok(truncate(text), format!("{count} lines"))
}

/// A line-oriented tool must never allocate an arbitrarily long input line.
/// The viewer/output cap is 32 KiB, so accepting more than twice that from
/// one line cannot improve the result and turns a hostile file into an OOM.
const MAX_LINE_BYTES: usize = 64 * 1024;

fn bounded_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            break;
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let take = end.map(|index| index + 1).unwrap_or(available.len());
        if bytes.len().saturating_add(take) > MAX_LINE_BYTES {
            reader.consume(take);
            // Leave the reader at the start of the next physical line so a
            // caller that keeps scanning (grep) can move past an oversized
            // one instead of aborting the whole file. Bounded: we never hold
            // more than the current buffer chunk.
            drain_line_remainder(reader);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("line exceeds {MAX_LINE_BYTES} bytes"),
            ));
        }
        bytes.extend_from_slice(&available[..take]);
        reader.consume(take);
        if end.is_some() {
            break;
        }
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "file is not valid UTF-8"))
}

/// Discard the rest of the current physical line (through the next `\n`, or
/// EOF) without allocating it, so a caller can resume scanning later lines
/// after an oversized one. Reads in fixed chunks; never retains more than one.
fn drain_line_remainder<R: BufRead>(reader: &mut R) {
    loop {
        // Capture plain counts so the `fill_buf` borrow ends before `consume`.
        let (consume, found_newline) = match reader.fill_buf() {
            Ok(a) => {
                if a.is_empty() {
                    return;
                }
                match a.iter().position(|b| *b == b'\n') {
                    Some(pos) => (pos + 1, true),
                    None => (a.len(), false),
                }
            }
            Err(_) => return,
        };
        reader.consume(consume);
        if found_newline {
            return;
        }
    }
}

fn read_window(path: &Path, offset: Option<u64>, limit: Option<u64>) -> io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let first = offset.unwrap_or(1).max(1) as usize;
    let limit = limit.map(|n| n as usize).unwrap_or(usize::MAX);
    let mut output = String::new();
    let mut line_number = 0usize;
    let mut returned = 0usize;
    loop {
        // Stop before reading the next line once the window is full: a line
        // past `limit` (or the 32 KiB output cap) may be oversized or invalid
        // UTF-8, and reading it would turn a valid window into an error.
        if returned >= limit || output.len() > 32 * 1024 {
            break;
        }
        let Some(line) = bounded_line(&mut reader)? else {
            break;
        };
        line_number += 1;
        if line_number < first {
            continue;
        }
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!("{line_number}\t{line}"));
        returned += 1;
    }
    Ok(output)
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
    let Some(content) = args["content"].as_str() else {
        return err("write: content must be a string".into());
    };
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
    let before_lines = match count_text_lines(&full) {
        Ok(lines) => lines,
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return err(format!("write {path}: {error}")),
    };
    if let Some(parent) = full.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return err(format!("write {path}: {error}"));
        }
    }
    match std::fs::write(&full, content) {
        Ok(()) => {
            super::note_seen(&full);
            let additions = content.lines().count();
            let deletions = before_lines;
            // The model wrote this content one message ago — echoing it back
            // would bill the whole file into the context a second time (and
            // again on every later request). The full diff goes to the
            // detail viewer instead.
            let detail = format!(
                "wrote {path}\n[replaced {before_lines} line(s) with {} line(s)]",
                content.lines().count()
            );
            ToolOutput {
                content: format!("wrote {path} ({} lines)", content.lines().count()),
                outcome: ToolOutcome::Completed,
                summary: format!("+{additions} -{deletions}"),
                display: Some(detail),
            }
        }
        Err(e) => err(format!("write {path}: {e}")),
    }
}

fn count_text_lines(path: &Path) -> io::Result<usize> {
    // Counting lines for the write summary must not enforce the viewer's
    // per-line cap: a valid file with long lines (minified JS/JSON) is still
    // overwritable, and bounding the read here would wrongly reject those
    // writes. We still refuse non-UTF-8 files (they are binary, not text to
    // overwrite) — validated incrementally so a long line is never held in
    // memory, matching bounded_line's contract without its length limit.
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut newlines = 0usize;
    let mut ends_with_newline = true;
    let mut saw_any = false;
    let mut pending = 0u8; // continuation bytes still expected for a lead
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        saw_any = true;
        for &byte in &buffer[..n] {
            if byte == b'\n' {
                newlines += 1;
                ends_with_newline = true;
            } else {
                ends_with_newline = false;
            }
            if pending > 0 {
                if byte & 0xC0 != 0x80 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "file is not valid UTF-8",
                    ));
                }
                pending -= 1;
            } else if byte >= 0x80 {
                pending = match byte {
                    0xC2..=0xDF => 1,
                    0xE0..=0xEF => 2,
                    0xF0..=0xF4 => 3,
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "file is not valid UTF-8",
                        ))
                    }
                };
            }
        }
    }
    if pending > 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file is not valid UTF-8",
        ));
    }
    // A trailing segment without a newline still counts as one line, matching
    // bounded_line's "last partial line counts" semantics.
    Ok(if saw_any && !ends_with_newline {
        newlines + 1
    } else {
        newlines
    })
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
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let rel = path.strip_prefix(cwd).unwrap_or(path).display().to_string();
    let mut reader = BufReader::new(file);
    let mut n = 0usize;
    loop {
        match bounded_line(&mut reader) {
            Ok(Some(line)) => {
                n += 1;
                if re.is_match(&line) {
                    let line = truncate_match_line(line.trim());
                    hits.push(format!("{rel}:{n}: {line}"));
                    *count += 1;
                    if *count >= MATCH_CAP {
                        return;
                    }
                }
            }
            Ok(None) => break,
            // Oversized or non-UTF-8 line: skip it (bounded_line advanced past
            // it) and keep scanning later lines instead of dropping them.
            Err(_) => n += 1,
        }
    }
}

fn truncate_match_line(line: &str) -> &str {
    const MAX_MATCH_LINE_BYTES: usize = 4096;
    if line.len() <= MAX_MATCH_LINE_BYTES {
        return line;
    }
    let mut end = MAX_MATCH_LINE_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::{glob_regex, write};
    use serde_json::json;

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

    #[test]
    fn write_rejects_missing_content_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("e-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("kept.txt");
        std::fs::write(&file, "keep me").unwrap();
        let output = write(&json!({"path": "kept.txt"}), &dir);
        assert!(output.is_error());
        assert_eq!(std::fs::read_to_string(file).unwrap(), "keep me");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn write_refuses_non_utf8_existing_file() {
        let dir = std::env::temp_dir().join(format!("e-fs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("binary");
        std::fs::write(&file, [0xff, 0x00]).unwrap();
        let output = write(&json!({"path": "binary", "content": "replacement"}), &dir);
        assert!(output.is_error());
        assert_eq!(std::fs::read(file).unwrap(), vec![0xff, 0x00]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
