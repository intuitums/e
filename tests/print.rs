//! `e -p`: one headless turn. Plain mode streams the reply to stdout and
//! exits 0; `--json` streams every event as a line and ends with the same
//! result object `e rpc` returns; the prompt may come from stdin; a missing
//! prompt is a usage error; a failed turn exits 1.

mod common;

use std::io::Write as _;
use std::process::{Command, Stdio};

use common::{env_lock, serve_sse, Home};

const OK_STREAM: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
     data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n\
     data: [DONE]\n\n";

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

fn run(home: &Home, args: &[&str], stdin: Option<&str>) -> std::process::Output {
    // Trust is a precondition for a run, and this test home starts empty: the
    // launch directory is trusted here the way a developer's checkout is in
    // their own home. `cli::print_mode_refuses_an_untrusted_workspace` covers
    // the refusal.
    e::core::config::trust::set(&std::env::current_dir().unwrap(), true).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "--no-save"])
        .args(args)
        .env("E_HOME", &home.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(text) = stdin {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

#[test]
fn print_streams_the_reply_and_exits_zero() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK_STREAM]);
    let home = mock_home("print-plain", port);
    let output = run(&home, &["-p", "say ok"], None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
    let requests = server.join().unwrap();
    assert!(requests[0].contains("say ok"));
}

#[test]
fn print_json_streams_events_then_one_result_line() {
    let _lock = env_lock();
    let (port, _server) = serve_sse(&[OK_STREAM]);
    let home = mock_home("print-json", port);
    let output = run(&home, &["-p", "--json", "hello"], None);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<serde_json::Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("every line is JSON"))
        .collect();
    assert_eq!(lines[0]["type"], "turn_start");
    assert!(lines
        .iter()
        .any(|l| l["type"] == "text" && l["delta"] == "ok"));
    assert!(lines
        .iter()
        .any(|l| l["type"] == "usage" && l["input_tokens"] == 5));
    let last = lines.last().unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["final_output"], "ok");
    assert_eq!(last["model"], "mock/test");
    assert!(last["session"].is_null(), "memory-only run");
}

#[test]
fn print_takes_the_prompt_from_stdin_when_no_argument() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK_STREAM]);
    let home = mock_home("print-stdin", port);
    let output = run(&home, &["-p"], Some("from a pipe\n"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "ok\n");
    assert!(server.join().unwrap()[0].contains("from a pipe"));
}

#[test]
fn print_without_any_prompt_is_a_usage_error() {
    let _lock = env_lock();
    let home = Home::new("print-empty");
    let output = run(&home, &["-p"], Some(""));
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("needs a prompt"));
    let output = run(&home, &["-p", "-c", "x"], Some(""));
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_failed_turn_exits_one_and_says_why() {
    let _lock = env_lock();
    // Two blank successful streams: one free retry, then the turn errors.
    let blank = "data: [DONE]\n\n";
    let (port, _server) = serve_sse(&[blank, blank]);
    let home = mock_home("print-fail", port);
    let output = run(&home, &["-p", "hi"], None);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
    assert!(output.stdout.is_empty());
}

#[test]
fn print_mode_tells_extensions_there_is_no_ui() {
    let _lock = env_lock();
    let (port, _server) = serve_sse(&[OK_STREAM]);
    let home = mock_home("print-headless", port);
    // This test spawns e itself rather than through `run`, and trust is a
    // precondition for the turn it is about.
    e::core::config::trust::set(&std::env::current_dir().unwrap(), true).unwrap();
    let ext = home.dir.join("extensions");
    std::fs::create_dir_all(&ext).unwrap();
    let path = ext.join("asker.sh");
    // Hold the turn hook until the extension records its reply, before shutdown can race it.
    std::fs::write(
        &path,
        r#"#!/bin/sh
log="$E_HOME/ext.log"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\),"method".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' "$line" | grep -o '"ui":[a-z]*' >> "$log"
      printf '{"id":%s,"result":{"name":"asker","hooks":["before_turn"]}}\n' "$id"
      ;;
    *'"method":"hook.before_turn"'*)
      hook_id=$id
      printf '{"id":"q","method":"session.info","params":{}}\n'
      ;;
    *'"id":"q"'*)
      printf 'reply %s\n' "$line" >> "$log"
      printf '{"id":%s,"result":{}}\n' "$hook_id"
      ;;
    *'"method":"shutdown"'*) exit 0 ;;
  esac
done
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-save", "-p", "hi"])
        .env("E_HOME", &home.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = std::fs::read_to_string(home.dir.join("ext.log")).unwrap_or_default();
    assert!(log.contains("\"ui\":false"), "{log}");
    assert!(log.contains(r#"reply {"error":"no ui","id":"q"}"#), "{log}");
}
