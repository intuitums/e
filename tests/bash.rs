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
    assert!(first_seen.lock().unwrap().unwrap() < Duration::from_millis(300));
    assert!(started.elapsed() >= Duration::from_millis(350));
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
    let clean = tools::sanitize_display("ok\x1b]0;owned\x07\tend\r\n");
    assert_eq!(clean, "ok]0;owned    end\n");
    assert!(!clean
        .chars()
        .any(|character| character.is_control() && character != '\n'));
}

#[test]
fn run_shell_reaches_the_tool() {
    let out = tools::run_shell("printf typed-entry-ok", std::path::Path::new("."));
    assert!(!out.is_error());
    assert_eq!(out.content, "typed-entry-ok");
}
