//! The generic `e rpc` knobs a subagent extension shapes a delegated turn
//! with: the `tools` allowlist and the saved-transcript `session` path. Core
//! stays generic — it never learns what an "agent" is, and nothing is
//! appended to the delegated turn's prompt.

#[cfg(unix)]
use std::io::Read as _;
use std::io::Write as _;
use std::process::{Command, Stdio};

#[cfg(unix)]
fn wait_for_rpc_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(std::time::Instant::now() < deadline, "rpc ignored SIGTERM");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

mod common;

use common::{env_lock, request_json, serve_sse, Home};

fn run_rpc(home: &Home, request_line: &str) -> Vec<u8> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "rpc"])
        .env("E_HOME", &home.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(request_line.as_bytes())
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn mock_home(label: &str, port: u16) -> Home {
    let home = Home::new(label);
    home.write(
        "models.json",
        format!(
            r#"{{"providers":{{"mock":{{"base_url":"http://127.0.0.1:{port}","api":"completions","models":["test"]}}}}}}"#
        ),
    );
    home.auth(r#"{"format_version":1,"mock":{"key":"k"}}"#);
    home
}

const OK_STREAM: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
     data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n\
     data: [DONE]\n\n";

#[test]
fn rpc_rejects_unknown_names_in_the_builtin_allowlist() {
    let _lock = env_lock();
    let home = Home::new("delegation-unknown-tool");
    let stdout = run_rpc(
        &home,
        "{\"id\":1,\"prompt\":\"go\",\"tools\":[\"extension_tool\"]}\n",
    );
    let response: serde_json::Value = serde_json::from_slice(&stdout).expect("one JSON response");
    assert!(response["error"]
        .as_str()
        .is_some_and(|error| error.contains("unknown built-in tool")));
}

/// `tools` restricts the request schemas to named built-ins. Execution checks
/// the same list, and the prompt gets a generic policy notice.
#[test]
fn rpc_tools_field_restricts_the_advertised_tools() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK_STREAM]);
    let home = mock_home("delegation-tools", port);

    run_rpc(
        &home,
        "{\"id\":1,\"prompt\":\"go\",\"tools\":[\"read\",\"grep\"]}\n",
    );

    let captured = server.join().unwrap().remove(0);
    let names: Vec<String> = request_json(&captured)["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        names,
        vec!["read", "grep"],
        "tools not restricted to the allowlist"
    );
}

/// A saved turn returns its session's JSONL path, so a caller can read the full
/// transcript — every tool call — not just the final text.
#[test]
fn rpc_saved_turn_returns_its_transcript_path() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK_STREAM]);
    let home = mock_home("delegation-session", port);

    let stdout = run_rpc(&home, "{\"id\":1,\"prompt\":\"go\",\"save\":true}\n");
    server.join().unwrap();

    let response: serde_json::Value = serde_json::from_slice(&stdout).expect("one JSON response");
    let session = response["session"].as_str().unwrap_or_default();
    assert!(
        session.ends_with(".jsonl"),
        "expected a saved transcript path, got {:?}",
        response["session"]
    );
}

/// The subagent watchdog sends SIGTERM to its `e rpc` child. RPC must kill
/// the active bash process group before exiting, or a detached grandchild can
/// wake later and keep changing the workspace.
#[cfg(unix)]
#[test]
fn rpc_sigterm_kills_delegated_bash_descendants() {
    let _lock = env_lock();
    let command = "printf started > rpc-started; sleep 1; printf escaped > rpc-marker";
    let arguments = serde_json::json!({"command": command}).to_string();
    let event = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "c1",
                    "function": {"name": "bash", "arguments": arguments}
                }]
            }
        }]
    });
    let stream = format!(
        "data: {event}\n\ndata: {{\"choices\":[{{\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n"
    );
    let (port, server) = serve_sse(&[&stream]);
    let home = mock_home("delegation-sigterm", port);
    let workspace = std::env::temp_dir().join(format!(
        "e-delegation-sigterm-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&workspace).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "rpc"])
        .env("E_HOME", &home.dir)
        .current_dir(&workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"id\":1,\"prompt\":\"run it\"}\n")
        .unwrap();
    drop(child.stdin.take());

    let started = workspace.join("rpc-started");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !started.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "delegated bash command never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    unsafe {
        assert_eq!(libc::kill(child.id() as i32, libc::SIGTERM), 0);
    }
    let status = wait_for_rpc_exit(&mut child);
    assert_eq!(status.code(), Some(143));

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.is_empty(), "stderr: {stderr}");
    server.join().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1_200));
    assert!(
        !workspace.join("rpc-marker").exists(),
        "delegated bash descendant survived RPC shutdown"
    );
    let _ = std::fs::remove_dir_all(workspace);
}
