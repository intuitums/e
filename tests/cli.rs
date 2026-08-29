use std::io::Write as _;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

mod common;

use common::{env_lock, request_json, serve_sse, Home};

/// Collect a child process without letting a hanging CLI regression stall the suite.
fn output_with_timeout(mut child: std::process::Child) -> Output {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "CLI did not terminate; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Configure one isolated home with a single mock model endpoint.
fn mock_home(name: &str, port: u16) -> Home {
    let home = Home::new(name);
    home.write(
        "models.json",
        format!(
            r#"{{"providers":{{"mock":{{"base_url":"http://127.0.0.1:{port}","api":"completions","models":["test"]}}}}}}"#
        ),
    );
    home.write("auth.json", r#"{"format_version":1,"mock":{"key":"k"}}"#);
    home
}

/// Provider turns that ask once, then recover after the question is cancelled.
fn question_turns() -> [&'static str; 2] {
    [
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"ask-1\",",
            "\"function\":{\"name\":\"ask\",\"arguments\":\"{\\\"question\\\":\\\"Need input\\\"}\"}}]},",
            "\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ),
        "data: {\"choices\":[{\"delta\":{\"content\":\"continued\"}}]}\n\ndata: [DONE]\n\n",
    ]
}

#[test]
fn ask_json_keeps_validation_errors_on_stdout_as_json() {
    let output = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "--json", "ask"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"], "prompt is empty");

    let output = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "--json", "ask", "--model"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(value["error"]
        .as_str()
        .unwrap()
        .starts_with("--model requires a value"));
    assert!(value["error"].as_str().unwrap().contains("usage: e ask"));
}

#[test]
fn noninteractive_modes_cancel_questions_and_terminate() {
    let _lock = env_lock();

    let turns = question_turns();
    let (port, server) = serve_sse(&turns);
    let home = mock_home("headless-question", port);
    let child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "--no-save", "ask", "run"])
        .env("E_HOME", &home.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let output = output_with_timeout(child);
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "continued");
    assert!(String::from_utf8_lossy(&output.stderr).contains(
        "ask tool question cancelled: headless mode does not support interactive answers"
    ));
    assert!(server.join().unwrap()[1].contains("dismissed the question"));

    let turns = question_turns();
    let (port, server) = serve_sse(&turns);
    let home = mock_home("json-question", port);
    let child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "--no-save", "--json", "ask", "run"])
        .env("E_HOME", &home.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let output = output_with_timeout(child);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["final_output"], "continued");
    assert_eq!(body["tools"]["failures"], 1);
    assert!(body["warnings"].as_array().unwrap().iter().any(|warning| {
        warning == "ask tool question cancelled: headless mode does not support interactive answers"
    }));
    assert!(server.join().unwrap()[1].contains("dismissed the question"));

    let turns = question_turns();
    let (port, server) = serve_sse(&turns);
    let home = mock_home("rpc-question", port);
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
        .write_all(b"{\"id\":7,\"prompt\":\"run\"}\n")
        .unwrap();
    drop(child.stdin.take());
    let output = output_with_timeout(child);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["id"], 7);
    assert_eq!(body["final_output"], "continued");
    assert_eq!(body["tools"]["failures"], 1);
    assert!(body["warnings"].as_array().unwrap().iter().any(|warning| {
        warning == "ask tool question cancelled: RPC does not support interactive answers"
    }));
    assert!(server.join().unwrap()[1].contains("dismissed the question"));
}

#[test]
fn json_auth_is_rejected_like_other_unsupported_subcommands() {
    let home = std::env::temp_dir().join(format!(
        "e-cli-json-auth-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "--json", "auth"])
        .env("E_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        output.stdout.is_empty(),
        "no human report on stdout under --json"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--json is supported by"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn rpc_stops_cleanly_on_an_oversized_request_line() {
    let home = std::env::temp_dir().join(format!(
        "e-cli-rpc-oversized-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "rpc"])
        .env("E_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Past the 10 MiB cap, no trailing newline — a client that never
    // terminates the line must not hang the process or grow it unbounded.
    let oversized = vec![b'x'; 10 * 1024 * 1024 + 1];
    child.stdin.as_mut().unwrap().write_all(&oversized).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
    let lines = String::from_utf8(output.stdout).unwrap();
    let values = lines
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        values.len(),
        1,
        "one error response, then the process stops"
    );
    assert_eq!(values[0]["id"], serde_json::Value::Null);
    assert!(values[0]["error"]
        .as_str()
        .unwrap()
        .contains("invalid request"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn rpc_keeps_one_response_per_input_line_after_a_bad_request() {
    let home = std::env::temp_dir().join(format!(
        "e-cli-rpc-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "rpc"])
        .env("E_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{bad\n{\"id\":\"keep-me\"}\n{\"id\":2,\"prompt\":\"\"}\n")
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
    let lines = String::from_utf8(output.stdout).unwrap();
    let values = lines
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 3);
    assert!(values[0]["error"]
        .as_str()
        .unwrap()
        .contains("invalid request"));
    assert_eq!(values[0]["id"], serde_json::Value::Null);
    assert_eq!(values[1]["id"], "keep-me");
    assert!(values[1]["error"]
        .as_str()
        .unwrap()
        .contains("missing field `prompt`"));
    assert_eq!(values[2]["id"], 2);
    assert_eq!(values[2]["error"], "prompt is empty");

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn typo_flags_are_rejected_with_suggestions_not_prompts() {
    let home = std::env::temp_dir().join(format!(
        "e-cli-typo-flag-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "--modle", "x"])
        .env("E_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown option --modle"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("did you mean --model?"), "stderr: {stderr}");
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn standalone_near_miss_words_suggest_commands_not_sessions() {
    let home = std::env::temp_dir().join(format!(
        "e-cli-help-word-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "help"])
        .env("E_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did you mean `e --help`?"),
        "stderr: {stderr}"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "docss"])
        .env("E_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("did you mean `e docs`?"),
        "stderr: {stderr}"
    );
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn piped_stdin_is_prompt_text_not_a_terminal() {
    let home = std::env::temp_dir().join(format!(
        "e-cli-piped-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    // An empty pipe and no typed words: a usage error, never an open TUI.
    let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions"])
        .env("E_HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no prompt"), "stderr: {stderr}");

    // Piped text joins typed words into one prompt for the headless path,
    // proven end to end against a mock provider: the request the binary
    // sends must carry both the typed words and the piped text.
    let _lock = env_lock();
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"pipe works\"}}]}\n\n\
                data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n\
                data: [DONE]\n\n";
    let (port, server) = serve_sse(&[body]);
    let home = Home::new("piped-ask");
    home.write(
        "models.json",
        format!(
            r#"{{"providers":{{"mock":{{"base_url":"http://127.0.0.1:{port}","api":"completions","models":["test"]}}}}}}"#
        ),
    );
    home.write("auth.json", r#"{"format_version":1,"mock":{"key":"k"}}"#);
    let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "--no-save", "ask", "summarize:"])
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
        .write_all(b"piped body")
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let captured = server.join().unwrap().remove(0);
    let sent = String::from_utf8_lossy(captured.as_bytes()).into_owned();
    let request = request_json(&sent);
    // The user message is the last one in the completions payload.
    let prompt = request["messages"]
        .as_array()
        .and_then(|messages| messages.iter().rev().find(|m| m["role"] == "user"))
        .map(|m| {
            m["content"]
                .as_str()
                .map(str::to_string)
                .or_else(|| m["content"][0]["text"].as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .unwrap_or_default();
    assert!(prompt.contains("summarize:"), "prompt: {prompt}");
    assert!(prompt.contains("piped body"), "prompt: {prompt}");
    assert!(prompt.rfind("summarize:").unwrap() < prompt.rfind("piped body").unwrap());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "pipe works");
} // Home::drop removes the isolated E_HOME
