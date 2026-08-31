//! The generic `e rpc` knobs a subagent extension shapes a delegated turn
//! with: the `tools` allowlist and the saved-transcript `session` path. Core
//! stays generic — it never learns what an "agent" is, and nothing is
//! appended to the delegated turn's prompt.

use std::io::Write as _;
use std::process::{Command, Stdio};

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

/// `tools` restricts the delegated turn to a positive allowlist — the request
/// advertises only those built-ins. This is how an agent's tool envelope is
/// enforced; no system prompt is involved.
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
