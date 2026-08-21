//! The bash tool's contract: output captured, runaway commands killed at the
//! wall-clock timeout.

use std::time::{Duration, Instant};

use e::core::tools;

fn run_cmd(cmd: &str, timeout: u64) -> tools::ToolOutput {
    let args = serde_json::json!({ "command": cmd, "timeout": timeout });
    tools::run("bash", &args.to_string(), std::path::Path::new("."))
}

#[test]
fn captures_output_and_exit_code() {
    let out = run_cmd("echo hello; echo err >&2; exit 3", 10);
    assert!(out.is_error);
    assert_eq!(out.summary, "exit 3");
    assert!(out.content.contains("hello"));
    assert!(out.content.contains("err"));
}

#[test]
fn timeout_kills_a_runaway_command() {
    // A command that would run forever must return an error promptly —
    // well under its own sleep time — not hang the agent.
    let started = Instant::now();
    let out = run_cmd("sleep 30 && echo never", 1);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(out.is_error);
    assert_eq!(out.summary, "timeout 1s");
    assert!(!out.content.contains("never"));
}

#[test]
fn timeout_kills_spawned_children_too() {
    // The kill reaches the process group: a backgrounded child of the
    // script cannot survive past the deadline.
    let started = Instant::now();
    let out = run_cmd("sh -c 'sleep 30' && echo never", 1);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(out.is_error);
}
