//! The edit tool: exact single-occurrence string replacement.

use serde_json::{json, Value};
use std::path::Path;

use super::{resolve, schema_object, ToolOutcome, ToolOutput};

pub fn schema() -> Value {
    schema_object(
        "edit",
        "Replace an exact string in a file. old_string must occur exactly once and is the file's raw text — never include the line-number prefix the read tool adds. Fails if the file changed on disk since it was last read.",
        json!({
            "path": {"type": "string"},
            "old_string": {"type": "string", "description": "Exact text to replace, including whitespace"},
            "new_string": {"type": "string"}
        }),
        &["path", "old_string", "new_string"],
    )
}

pub fn run(args: &Value, cwd: &Path) -> ToolOutput {
    let err = |m: String| ToolOutput {
        content: m,
        outcome: ToolOutcome::Failed,
        summary: "error".into(),
        display: None,
    };
    let Some(path) = args["path"].as_str() else {
        return err("edit: missing path".into());
    };
    let (Some(old), Some(new)) = (args["old_string"].as_str(), args["new_string"].as_str()) else {
        return err("edit: missing old_string or new_string".into());
    };
    let full = resolve(cwd, path);
    if let Err(output) = super::require_regular_file(&full, "edit", path) {
        return output;
    }
    // Hold this path's write lock across read-modify-write so a concurrent
    // batch member can't overwrite this edit (or vice versa) unseen.
    let _guard = super::fs_write_lock(&full);
    if let Err(output) = super::check_fresh(&full, "edit", path) {
        return output;
    }
    let text = match std::fs::read_to_string(&full) {
        Ok(t) => t,
        Err(e) => return err(format!("edit {path}: {e}")),
    };
    let occurrences = text.matches(old).count();
    if occurrences == 0 {
        return err(format!("edit {path}: old_string not found"));
    }
    if occurrences > 1 {
        return err(format!(
            "edit {path}: old_string occurs {occurrences} times; make it unique"
        ));
    }
    let updated = text.replacen(old, new, 1);
    match std::fs::write(&full, &updated) {
        Ok(()) => {
            super::note_seen(&full);
            let delta = updated.lines().count() as isize - text.lines().count() as isize;
            let additions = new.lines().count();
            let deletions = old.lines().count();
            // The model authored old_string and new_string one message ago —
            // echoing them back would pay for the diff a second time on
            // every later request. The diff goes to the detail viewer.
            let mut detail = format!("edited {path}");
            if !old.is_empty() || !new.is_empty() {
                detail.push_str("\n--- before\n");
                for line in old.lines() {
                    detail.push_str(&format!("-{line}\n"));
                }
                detail.push_str("+++ after\n");
                for line in new.lines() {
                    detail.push_str(&format!("+{line}\n"));
                }
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
        Err(e) => err(format!("edit {path}: {e}")),
    }
}
