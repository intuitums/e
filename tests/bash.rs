//! The bash tool's contract: ordered live pipe chunks, bounded capture, and
//! process-group cancellation at timeout or Esc.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use e::core::tools::{self, ToolOutcome};

fn run_cmd(cmd: &str, timeout: u64) -> tools::ToolOutput {
    let args = serde_json::json!({ "command": cmd, "timeout": timeout });
    tools::run("bash", &args.to_string(), std::path::Path::new("."))
}

#[test]
fn captures_output_and_exit_code() {
    let out = run_cmd("echo hello; echo err >&2; exit 3", 10);
    assert!(out.is_error());
    assert_eq!(out.summary, "exit 3");
    assert!(out.content.contains("hello"));
    assert!(out.content.contains("err"));
}

#[test]
fn timeout_kills_a_runaway_command() {
    let started = Instant::now();
    let out = run_cmd("sleep 30 && echo never", 1);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(out.outcome, ToolOutcome::TimedOut);
    assert_eq!(out.summary, "timeout 1s");
    assert!(!out.content.contains("never"));
}

#[test]
fn timeout_kills_spawned_children_too() {
    let started = Instant::now();
    let out = run_cmd("sh -c 'sleep 30' && echo never", 1);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(out.is_error());
}

#[test]
fn publishes_output_before_the_process_exits() {
    let args = serde_json::json!({
        "command": "printf first; sleep 0.4; printf second",
        "timeout": 10
    });
    let started = Instant::now();
    let first_seen = Arc::new(Mutex::new(None));
    let seen = first_seen.clone();
    let out = tools::run_streaming(
        "bash",
        &args.to_string(),
        std::path::Path::new("."),
        &AtomicBool::new(false),
        move |_, chunk| {
            if chunk.contains("first") && seen.lock().unwrap().is_none() {
                *seen.lock().unwrap() = Some(started.elapsed());
            }
        },
    );
    assert_eq!(out.outcome, ToolOutcome::Completed);
    // Pin streaming, not absolute spawn latency: first must arrive while the
    // command is still sleeping, not only when it exits. Absolute <300ms
    // flakes on busy CI runners where process start alone can exceed that.
    let first = first_seen
        .lock()
        .unwrap()
        .expect("first chunk should stream");
    let total = started.elapsed();
    assert!(
        first + Duration::from_millis(200) < total,
        "streamed too late: first={first:?} total={total:?}"
    );
    assert!(total >= Duration::from_millis(350));
}

#[test]
fn preserves_observed_stdout_stderr_order() {
    let out = run_cmd(
        "printf stdout-1; sleep 0.1; printf stderr-1 >&2; sleep 0.1; printf stdout-2",
        10,
    );
    assert_eq!(out.content, "stdout-1stderr-1stdout-2");
}

#[test]
fn cancellation_kills_the_process_group() {
    let cancel = Arc::new(AtomicBool::new(false));
    let trigger = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        trigger.store(true, Ordering::SeqCst);
    });
    let args = serde_json::json!({ "command": "sleep 30", "timeout": 10 });
    let started = Instant::now();
    let out = tools::run_streaming(
        "bash",
        &args.to_string(),
        std::path::Path::new("."),
        &cancel,
        |_, _| {},
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(out.outcome, ToolOutcome::Cancelled);
}

#[test]
fn process_output_is_safe_for_terminal_rendering() {
    // A whole OSC sequence disappears — payload included: stripping only the
    // ESC byte used to leave "]0;owned" behind as visible garbage.
    let clean = tools::sanitize_display("ok\x1b]0;owned\x07\tend\r\n");
    assert_eq!(clean, "ok    end\n");
    // CSI colour codes go the same way, parameters and all.
    assert_eq!(tools::sanitize_display("\x1b[1;31mred\x1b[0m"), "red");
    assert!(!clean
        .chars()
        .any(|character| character.is_control() && character != '\n'));
}

#[test]
fn model_facing_output_drops_ansi_and_progress_rewrites() {
    // The model's copy of command output pays no tokens for colour codes,
    // and a progress bar that redrew itself collapses to its final frame.
    let out = run_cmd(
        "printf '\\033[32mgreen\\033[0m\\nstep 1\\rstep 2\\rstep 3\\ndone\\n'",
        10,
    );
    assert_eq!(out.content, "green\nstep 3\ndone");
}

#[test]
fn long_output_keeps_the_tail_where_the_verdict_lives() {
    // 64KB of filler then the failure line: compilers and test runners put
    // the verdict at the end, so the retained window must be the tail.
    let out = run_cmd(
        "for i in $(seq 1 4000); do echo \"filler line $i\"; done; echo 'error: the actual failure'",
        30,
    );
    assert!(
        out.content.contains("error: the actual failure"),
        "the tail was dropped: {}",
        &out.content[..200.min(out.content.len())]
    );
    assert!(
        out.content.starts_with("… [truncated"),
        "a truncated log must announce itself up front"
    );
    assert!(
        !out.content.contains("filler line 1\n"),
        "head was retained"
    );
}

#[test]
fn retained_tail_never_starts_inside_a_utf8_code_point() {
    // 32,769 bytes makes the raw 32 KiB suffix start on the continuation
    // byte of the opening é. The retained text should drop that orphan byte,
    // not manufacture U+FFFD at the truncation boundary.
    let out = run_cmd(
        "printf '\\303\\251'; head -c 32767 /dev/zero | tr '\\000' x",
        10,
    );
    assert!(out.content.starts_with("… [truncated"));
    assert!(!out.content.contains('\u{FFFD}'));
    assert!(out.content.ends_with("xxxxxxxx"));
}

#[cfg(unix)]
#[test]
fn a_background_child_holding_the_pipe_does_not_hold_the_tool_open() {
    let started = Instant::now();
    let out = run_cmd("sleep 30 & echo hi", 10);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "background pipe owner delayed return for {:?}",
        started.elapsed()
    );
    assert_eq!(out.outcome, ToolOutcome::Completed);
    assert_eq!(out.content, "hi");
}

#[test]
fn run_shell_reaches_the_tool() {
    let out = tools::run_shell("printf typed-entry-ok", std::path::Path::new("."));
    assert!(!out.is_error());
    assert_eq!(out.content, "typed-entry-ok");
}
