//! The bash tool: spawn a shell command, capture combined output.
//!
//! Spawn-and-capture, not a terminal daemon (DESIGN.md §3). A wall-clock
//! timeout bounds runaway commands: the command runs as its own process
//! group and the whole group is killed at the deadline; output is truncated
//! by the caller.

use serde_json::{json, Value};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{schema_object, truncate, ToolOutput};

pub fn schema() -> Value {
    schema_object(
        "bash",
        "Run a shell command in the workspace and return its combined output.",
        json!({
            "command": {"type": "string"},
            "timeout": {"type": "integer", "description": "Seconds before the command is killed (default 120)"}
        }),
        &["command"],
    )
}

/// Kill a process group (the child was started as its own group leader).
/// Falls back to the direct child if the group signal fails.
fn kill_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        if libc::kill(-(pid as i32), libc::SIGKILL) == 0 {
            return;
        }
    }
    if let Ok(mut child) = Command::new("kill").arg("-9").arg(pid.to_string()).spawn() {
        let _ = child.wait();
    }
}

pub fn run(args: &Value, cwd: &Path) -> ToolOutput {
    let Some(command) = args["command"].as_str() else {
        return ToolOutput {
            content: "bash: missing command".into(),
            is_error: true,
            summary: "error".into(),
        };
    };
    let timeout = args["timeout"].as_u64().unwrap_or(120).clamp(1, 600);

    // The child leads its own process group, so the timeout kill reaches
    // everything a `bash -lc` script spawned, not just the shell itself.
    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return ToolOutput {
                content: format!("bash: {e}"),
                is_error: true,
                summary: "error".into(),
            }
        }
    };

    // Drain both pipes on threads while we poll: a command whose output
    // exceeds the pipe buffer must not block on write and deadlock the wait.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = std::io::Read::read_to_string(pipe, &mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = std::io::Read::read_to_string(pipe, &mut buf);
        }
        buf
    });

    // Poll for exit; at the deadline, kill the group and reap.
    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                kill_group(child.id());
                match child.wait() {
                    Ok(status) => break status,
                    Err(e) => {
                        return ToolOutput {
                            content: format!("bash: {e}"),
                            is_error: true,
                            summary: "error".into(),
                        }
                    }
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                return ToolOutput {
                    content: format!("bash: {e}"),
                    is_error: true,
                    summary: "error".into(),
                }
            }
        }
    };

    let mut combined = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    if !stderr.trim().is_empty() {
        if !combined.trim_end().is_empty() {
            combined.push('\n');
        }
        combined.push_str(stderr.trim_start());
    }
    let mut combined = combined.trim_end().to_string();
    if timed_out {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&format!("… [killed: exceeded the {timeout}s timeout]"));
    }

    let code = status.code().unwrap_or(-1);
    let is_error = !status.success();
    let summary = if timed_out {
        format!("timeout {timeout}s")
    } else if is_error {
        format!("exit {code}")
    } else {
        "done".into()
    };
    ToolOutput {
        content: truncate(combined),
        is_error,
        summary,
    }
}
