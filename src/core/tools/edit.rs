//! The edit tool: exact single-occurrence string replacement.

use serde_json::{json, Value};
use std::borrow::Cow;
use std::path::Path;

use super::{resolve, schema_object, ToolOutcome, ToolOutput};

pub fn schema() -> Value {
    schema_object(
        "edit",
        "Replace an exact string in a file. old_string must occur exactly once and is the file's raw text — never include the line-number prefix the read tool adds. Line endings: when old_string does not match as written, it is matched with the file's CRLF read as LF (as read shows it); the file keeps its own line endings. Fails if the file changed on disk since it was last read.",
        json!({
            "path": {"type": "string"},
            "old_string": {"type": "string", "description": "Exact text to replace, including whitespace"},
            "new_string": {"type": "string"}
        }),
        &["path", "old_string", "new_string"],
    )
}

pub fn run(args: &Value, cwd: &Path, state: &super::ToolRuntime) -> ToolOutput {
    let err = |m: String, target: &str| ToolOutput {
        summary: super::failure_summary(&m, "edit", target),
        content: m,
        outcome: ToolOutcome::Failed,
        display: None,
    };
    let Some(path) = args["path"].as_str() else {
        return err("edit: missing path".into(), "");
    };
    let (Some(old), Some(new)) = (args["old_string"].as_str(), args["new_string"].as_str()) else {
        return err("edit: missing old_string or new_string".into(), "");
    };
    let full = resolve(cwd, path);
    if let Err(output) = super::require_regular_file(&full, "edit", path) {
        return output;
    }
    // Hold this path's write lock across read-modify-write so a concurrent
    // batch member can't overwrite this edit (or vice versa) unseen.
    let _guard = super::fs_write_lock(&full);
    if let Err(output) = super::check_fresh(state, &full, "edit", path) {
        return output;
    }
    let text = match std::fs::read_to_string(&full) {
        Ok(t) => t,
        Err(e) => return err(format!("edit {path}: {e}"), path),
    };
    // Match the raw bytes first. A CRLF file is shown to the model with plain
    // newlines (read strips the `\r`), so a multi-line old_string built from
    // what it saw cannot match raw; retry on the LF-normalized text and put
    // the file's dominant ending back on the result.
    let normalized = text.matches(old).count() == 0 && text.contains("\r\n");
    let (subject, old, new): (Cow<str>, Cow<str>, Cow<str>) = if normalized {
        (
            text.replace("\r\n", "\n").into(),
            old.replace("\r\n", "\n").into(),
            new.replace("\r\n", "\n").into(),
        )
    } else {
        (text.as_str().into(), old.into(), new.into())
    };
    let occurrences = subject.matches(&*old).count();
    if occurrences == 0 {
        return err(format!("edit {path}: old_string not found"), path);
    }
    if occurrences > 1 {
        return err(
            format!("edit {path}: old_string occurs {occurrences} times; make it unique"),
            path,
        );
    }
    let mut updated = subject.replacen(&*old, &new, 1);
    if normalized && mostly_crlf(&text) {
        updated = updated.replace('\n', "\r\n");
    }
    match super::staged_write(&full, updated.as_bytes()) {
        Ok(()) => {
            super::note_seen(state, &full);
            let delta = updated.lines().count() as isize - text.lines().count() as isize;
            let additions = new.lines().count();
            let deletions = old.lines().count();
            // The model authored old_string and new_string one message ago —
            // echoing them back would pay for the diff a second time on
            // every later request. The diff goes to the detail viewer: real
            // file line numbers, context, and elision, computed against the
            // whole file so the numbers are the ones an editor would show.
            let mut detail = format!("edited {path}");
            let diff = super::diffview::render(&text, &updated);
            if !diff.is_empty() {
                detail.push('\n');
                detail.push_str(&diff);
            }
            ToolOutput {
                content: format!("edited {path}"),
                outcome: ToolOutcome::Completed,
                summary: if additions == 0 && deletions == 0 {
                    format!("{delta:+} lines")
                } else {
                    format!("+{additions} -{deletions}")
                },
                display: Some(super::truncate(detail.trim_end().to_string())),
            }
        }
        Err(e) => err(format!("edit {path}: {e}"), path),
    }
}

/// Whether most of `text`'s lines end in CRLF — the ending a normalized
/// edit is written back with.
fn mostly_crlf(text: &str) -> bool {
    text.matches("\r\n").count() * 2 >= text.matches('\n').count()
}
