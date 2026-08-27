use std::io::Write as _;
use std::process::{Command, Stdio};

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
    assert_eq!(value["error"], "--model requires a value");
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
