//! Workspace file listing for the `@` picker.
//!
//! A bounded recursive walk: skips VCS and build directories and hidden
//! entries, caps depth and count so a huge tree cannot stall the frame.
//! Paths come back workspace-relative.

use std::path::Path;

const SKIP: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    ".cache",
    "__pycache__",
];
const MAX_FILES: usize = 2000;
const MAX_DEPTH: usize = 6;

pub fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk(root, root, 0, &mut out);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || SKIP.contains(&name.as_ref()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            // Directories are pickable too — the reference's file index
            // offers them, shown (and inserted) with a trailing slash.
            if let Ok(relative) = path.strip_prefix(root) {
                out.push(format!("{}/", relative.display()));
            }
            walk(root, &path, depth + 1, out);
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(relative.display().to_string());
        }
    }
}
