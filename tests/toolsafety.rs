//! Filesystem-tool safety: parallel mutations can't silently lose changes,
//! non-regular files fail fast instead of hanging the turn, live bash output
//! survives UTF-8 split across pipe reads, and grep honours an explicitly
//! requested dotfile.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Barrier};

use e::core::tools;

fn workspace(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("e-toolsafety-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn parallel_edits_to_one_file_both_survive() {
    let ws = workspace("paredit");
    let file = ws.join("big.txt");
    let mut body = String::from("LEFT ");
    body.push_str(&"x".repeat(2_000_000));
    body.push_str(" RIGHT");
    std::fs::write(&file, &body).unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let spawn = |old: &'static str, new: &'static str| {
        let ws = ws.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let args = serde_json::json!({
                "path": "big.txt", "old_string": old, "new_string": new
            });
            barrier.wait();
            tools::run("edit", &args.to_string(), &ws)
        })
    };
    let left = spawn("LEFT", "LEFT-EDITED");
    let right = spawn("RIGHT", "RIGHT-EDITED");
    let left = left.join().unwrap();
    let right = right.join().unwrap();

    let saved = std::fs::read_to_string(&file).unwrap();
    // Both edits report success, so both must be in the file.
    assert!(!left.is_error(), "left edit failed: {}", left.content);
    assert!(!right.is_error(), "right edit failed: {}", right.content);
    assert!(saved.contains("LEFT-EDITED"), "left change lost");
    assert!(saved.contains("RIGHT-EDITED"), "right change lost");
    let _ = std::fs::remove_dir_all(&ws);
}

#[cfg(unix)]
#[test]
fn read_and_edit_on_a_fifo_fail_fast_instead_of_hanging() {
    let ws = workspace("fifo");
    let fifo = ws.join("blocked-input");
    assert!(std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());

    let started = std::time::Instant::now();
    let read = tools::run("read", r#"{"path":"blocked-input"}"#, &ws);
    let edit = tools::run(
        "edit",
        r#"{"path":"blocked-input","old_string":"a","new_string":"b"}"#,
        &ws,
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "non-regular files must fail fast, not block"
    );
    assert!(read.is_error() && read.content.contains("not a regular file"));
    assert!(edit.is_error() && edit.content.contains("not a regular file"));
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn live_bash_output_reassembles_utf8_split_across_reads() {
    let ws = workspace("utf8");
    let args = serde_json::json!({
        "command": "printf '\\303'; sleep 0.2; printf '\\251'",
        "timeout": 10
    });
    let streamed = std::sync::Mutex::new(String::new());
    let out = tools::run_streaming(
        "bash",
        &args.to_string(),
        &ws,
        &AtomicBool::new(false),
        |_, chunk| streamed.lock().unwrap().push_str(chunk),
    );
    assert_eq!(out.content, "é", "final capture decodes the code point");
    assert_eq!(
        streamed.lock().unwrap().as_str(),
        "é",
        "live output must not turn a split code point into U+FFFD"
    );
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn edit_fails_when_the_file_changed_on_disk_since_e_saw_it() {
    let ws = workspace("stale");
    let file = ws.join("config.txt");
    std::fs::write(&file, "alpha = 1\n").unwrap();

    // e reads the file (recording its on-disk state)…
    let read = tools::run("read", r#"{"path":"config.txt"}"#, &ws);
    assert!(!read.is_error());

    // …then something else rewrites it behind e's back.
    std::fs::write(&file, "alpha = 1\nbeta = 2\n").unwrap();

    let stale = tools::run(
        "edit",
        r#"{"path":"config.txt","old_string":"alpha = 1","new_string":"alpha = 9"}"#,
        &ws,
    );
    assert!(stale.is_error(), "a stale edit must not clobber the file");
    assert!(stale.content.contains("changed on disk"));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "alpha = 1\nbeta = 2\n",
        "the external change survives"
    );

    // Re-reading refreshes the record and the edit goes through.
    let reread = tools::run("read", r#"{"path":"config.txt"}"#, &ws);
    assert!(!reread.is_error());
    let edit = tools::run(
        "edit",
        r#"{"path":"config.txt","old_string":"alpha = 1","new_string":"alpha = 9"}"#,
        &ws,
    );
    assert!(
        !edit.is_error(),
        "after a re-read the edit proceeds: {}",
        edit.content
    );
    assert!(std::fs::read_to_string(&file)
        .unwrap()
        .contains("alpha = 9"));
    let _ = std::fs::remove_dir_all(&ws);
}

#[cfg(unix)]
#[test]
fn search_tools_survive_a_symlink_cycle() {
    let ws = workspace("symlink");
    std::fs::write(ws.join("real.txt"), "needle\n").unwrap();
    // A directory symlink pointing back at its parent: following it would
    // recurse forever.
    std::os::unix::fs::symlink(&ws, ws.join("cycle")).unwrap();

    let grep = tools::run("grep", r#"{"pattern":"needle"}"#, &ws);
    assert_eq!(grep.summary, "1 matches", "{}", grep.content);
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn grep_searches_an_explicitly_requested_dotfile() {
    let ws = workspace("dotgrep");
    std::fs::write(ws.join(".env"), "SECRET_NAME=needle\n").unwrap();

    let explicit = tools::run("grep", r#"{"pattern":"needle","path":".env"}"#, &ws);
    assert_eq!(explicit.summary, "1 matches");
    assert!(explicit.content.contains(".env:1:"));

    // The traversal heuristic still skips dotfiles it merely walks past.
    let walked = tools::run("grep", r#"{"pattern":"needle"}"#, &ws);
    assert_eq!(walked.summary, "0 matches");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn grep_glob_restricts_the_search_to_matching_files() {
    let ws = workspace("grep-glob");
    std::fs::write(ws.join("main.rs"), "needle in rust\n").unwrap();
    std::fs::write(ws.join("notes.txt"), "needle in text\n").unwrap();
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(ws.join("src").join("lib.rs"), "needle in lib\n").unwrap();

    let by_name = tools::run("grep", r#"{"pattern":"needle","glob":"*.rs"}"#, &ws);
    assert_eq!(by_name.summary, "2 matches", "{}", by_name.content);
    assert!(by_name.content.contains("main.rs"));
    assert!(by_name.content.contains("lib.rs"));
    assert!(!by_name.content.contains("notes.txt"));

    let by_path = tools::run("grep", r#"{"pattern":"needle","glob":"src/**/*.rs"}"#, &ws);
    assert_eq!(by_path.summary, "1 matches", "{}", by_path.content);
    assert!(by_path.content.contains("lib.rs"));
    assert!(!by_path.content.contains("main.rs"));
    let _ = std::fs::remove_dir_all(&ws);
}

/// When the matched-line list alone is big enough to hit the 32KB output
/// cap, the "stopped at N matches" explanation must still survive — not get
/// overwritten by the generic byte-truncation marker.
#[test]
fn grep_cap_notice_survives_the_output_truncation() {
    let ws = workspace("grep-cap");
    let filler = "x".repeat(200);
    let mut content = String::new();
    for _ in 0..250 {
        content.push_str(&format!("MATCHME {filler}\n"));
    }
    std::fs::write(ws.join("big.txt"), content).unwrap();

    let out = tools::run("grep", r#"{"pattern":"MATCHME"}"#, &ws);
    assert!(!out.is_error(), "{}", out.content);
    assert_eq!(out.summary, "200+ matches");
    assert!(
        out.content
            .ends_with("… [stopped at 200 matches — narrow the pattern or path to see the rest]"),
        "cap notice missing or swallowed by truncation, tail was: {:?}",
        &out.content[out.content.len().saturating_sub(120)..]
    );
    let _ = std::fs::remove_dir_all(&ws);
}
