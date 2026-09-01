use std::io::Write as _;
#[cfg(unix)]
use std::io::{BufRead as _, Read as _};
use std::process::{Command, Stdio};

#[cfg(unix)]
fn wait_for_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child ignored its shutdown signal"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
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

#[cfg(unix)]
#[test]
fn rpc_sigterm_exits_while_waiting_for_input() {
    let home = std::env::temp_dir().join(format!(
        "e-cli-rpc-sigterm-{}-{}",
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
        .write_all(b"{\"id\":1,\"prompt\":\"\"}\n")
        .unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut response = String::new();
    stdout.read_line(&mut response).unwrap();
    assert!(response.contains("prompt is empty"));

    unsafe {
        assert_eq!(libc::kill(child.id() as i32, libc::SIGTERM), 0);
    }
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(143));
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.is_empty(), "stderr: {stderr}");
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
fn piped_stdin_is_refused_with_a_pointer_to_rpc() {
    let home = std::env::temp_dir().join(format!(
        "e-cli-piped-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    // The interactive frame loop needs a terminal it owns; piped stdin has
    // none, and headless one-shots go through `e rpc`. Piping text in is a
    // usage error that names the headless path, never a half-open TUI.
    let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions"])
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
        .write_all(b"summarize this repository")
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("interactive terminal"), "stderr: {stderr}");
    assert!(stderr.contains("e rpc"), "stderr: {stderr}");
}
