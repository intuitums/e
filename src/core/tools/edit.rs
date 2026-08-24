//! The edit tool: exact single-occurrence string replacement.

use serde_json::{json, Value};
use std::path::Path;

use super::{resolve, schema_object, ToolOutcome, ToolOutput};

pub fn schema() -> Value {
    schema_object(
        "edit",
        "Replace an exact string in a file. old_string must occur exactly once.",
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
    // Hold the write lock across read-modify-write so a concurrent batch
    // member can't overwrite this edit (or vice versa) unseen.
    let _guard = super::fs_write_lock();
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
            let delta = updated.lines().count() as isize - text.lines().count() as isize;
            let additions = new.lines().count();
            let deletions = old.lines().count();
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
                content: super::truncate(detail.trim_end().to_string()),
                outcome: ToolOutcome::Completed,
                summary: if additions == 0 && deletions == 0 {
                    format!("{delta:+} lines")
                } else {
                    format!("+{additions} -{deletions}")
                },
            }
        }
        Err(e) => err(format!("edit {path}: {e}")),
    }
}
