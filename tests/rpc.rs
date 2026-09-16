//! `e rpc` as a session server: `hello` names the protocol, a session keeps
//! its history across prompts, events stream tagged with their session and
//! request, sessions interleave, saved sessions list and resume, extension
//! questions reach a client that answers them, and a version-1 line still
//! gets its one flat response. `tests/fixtures/rpc/v2-requests.jsonl` pins
//! the request shapes.

mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use common::{env_lock, serve_sse, Home};
use serde_json::{json, Value};

const OK: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
     data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n\
     data: [DONE]\n\n";
const AGAIN: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"again\"}}]}\n\n\
     data: {\"choices\":[],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":1}}\n\n\
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

/// A running `e rpc` with a line reader on its stdout.
struct Rpc {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Rpc {
    fn spawn(home: &Home, extra: &[&str]) -> Rpc {
        // The process cwd is the workspace a version-1 one-shot runs in, so it
        // has to be trusted; keep it out of the temp root, whose other
        // children are workspaces tests refuse.
        let cwd = std::env::temp_dir().join(format!("e-rpc-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        e::core::config::trust::set(&cwd, true).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
            .args(extra)
            .arg("rpc")
            .env("E_HOME", &home.dir)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Rpc {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, value: Value) {
        self.stdin
            .write_all(format!("{value}\n").as_bytes())
            .unwrap();
    }

    fn send_raw(&mut self, line: &str) {
        self.stdin
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
    }

    fn next(&self) -> Value {
        match self.lines.recv_timeout(Duration::from_secs(20)) {
            Ok(line) => serde_json::from_str(&line).unwrap_or_else(|_| panic!("not JSON: {line}")),
            Err(RecvTimeoutError::Timeout) => panic!("no line within 20s"),
            Err(RecvTimeoutError::Disconnected) => panic!("rpc exited"),
        }
    }

    /// Lines until the response carrying `id`, inclusive.
    fn until_response(&self, id: &str) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let line = self.next();
            let done = line.get("type").is_none() && line["id"] == id;
            seen.push(line);
            if done {
                return seen;
            }
        }
    }

    fn call(&mut self, id: &str, method: &str, params: Value) -> Value {
        self.send(json!({"id": id, "method": method, "params": params}));
        self.until_response(id).pop().unwrap()
    }

    fn create(&mut self, cwd: &std::path::Path, extra: Value) -> String {
        let mut params = json!({"cwd": cwd, "model": "mock/test"});
        for (k, v) in extra.as_object().unwrap() {
            params[k] = v.clone();
        }
        let created = self.call("c", "session.create", params);
        created["result"]["session"]
            .as_str()
            .unwrap_or_else(|| panic!("create failed: {created}"))
            .to_string()
    }

    fn finish(mut self) -> std::process::ExitStatus {
        drop(self.stdin);
        self.child.wait().unwrap()
    }
}

fn workspace(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("e-rpc-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A session refuses a workspace nobody has trusted; these tests are about
    // what a session does once it is open, and the refusal has its own test
    // below.
    e::core::config::trust::set(&dir, true).unwrap();
    dir
}

/// Trust is a precondition for a session, not a filter on what one loads.
#[test]
fn a_session_cannot_open_an_untrusted_workspace() {
    let _lock = env_lock();
    let home = mock_home("rpc-untrusted", 1);
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let dir = std::env::temp_dir().join(format!("e-rpc-refused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let created = rpc.call(
        "c",
        "session.create",
        json!({"cwd": dir, "model": "mock/test"}),
    );
    let error = created["error"].as_str().expect("refused");
    assert!(error.contains("must be trusted to run e"), "{error}");
    assert!(error.contains("e trust"), "{error}");
    assert!(rpc.finish().success());
}

#[test]
fn hello_reports_the_protocol_and_every_method() {
    let _lock = env_lock();
    let home = Home::new("rpc-hello");
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let hello = rpc.call("h", "hello", json!({"ask": true}));
    let result = &hello["result"];
    assert_eq!(result["protocol"], 2);
    assert_eq!(result["ask"], true);
    let methods: Vec<&str> = result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    for needed in [
        "session.create",
        "session.prompt",
        "session.steer",
        "session.interrupt",
        "session.fork",
        "ask.reply",
        "shutdown",
    ] {
        assert!(
            methods.contains(&needed),
            "{needed} missing from {methods:?}"
        );
    }
    assert!(rpc.finish().success());
}

#[test]
fn a_prompt_streams_tagged_events_then_answers_with_the_result() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK]);
    let home = mock_home("rpc-prompt", port);
    let ws = workspace("prompt");
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let session = rpc.create(&ws, json!({}));

    rpc.send(json!({"id": "p1", "method": "session.prompt", "params": {"session": session, "prompt": "say ok"}}));
    let lines = rpc.until_response("p1");
    let events: Vec<&Value> = lines.iter().filter(|l| l.get("type").is_some()).collect();
    assert_eq!(events.first().unwrap()["type"], "turn_start");
    assert_eq!(events.last().unwrap()["type"], "turn_end");
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "text" && e["delta"] == "ok"),
        "{events:?}"
    );
    for event in &events {
        assert_eq!(
            event["session"],
            session.as_str(),
            "every event names its session"
        );
        assert_eq!(
            event["request"], "p1",
            "every event names the request it serves"
        );
    }
    let result = &lines.last().unwrap()["result"];
    assert_eq!(result["final_output"], "ok");
    assert_eq!(result["session"], session.as_str());
    assert_eq!(result["path"], Value::Null, "memory-only by default");
    assert_eq!(result["usage"]["output_tokens"], 1);
    assert!(rpc.finish().success());
    server.join().unwrap();
}

#[test]
fn a_session_keeps_its_history_across_prompts() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK, AGAIN]);
    let home = mock_home("rpc-history", port);
    let ws = workspace("history");
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let session = rpc.create(&ws, json!({}));
    rpc.send(json!({"id": 1, "method": "session.prompt", "params": {"session": session, "prompt": "first"}}));
    rpc.until_response_num(1);
    rpc.send(json!({"id": 2, "method": "session.prompt", "params": {"session": session, "prompt": "second"}}));
    let second = rpc.until_response_num(2).pop().unwrap();
    assert_eq!(second["result"]["final_output"], "again");
    let info = rpc.call("i", "session.info", json!({"session": session}));
    assert_eq!(info["result"]["messages"], 4, "two user turns, two replies");
    assert!(rpc.finish().success());
    let requests = server.join().unwrap();
    assert!(
        requests[1].contains("first") && requests[1].contains("\"ok\""),
        "the second request carries the first exchange: {}",
        requests[1]
    );
}

impl Rpc {
    fn until_response_num(&self, id: u64) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let line = self.next();
            let done = line.get("type").is_none() && line["id"] == id;
            seen.push(line);
            if done {
                return seen;
            }
        }
    }
}

#[test]
fn two_sessions_run_side_by_side_and_each_result_matches_its_request() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK, AGAIN]);
    let home = mock_home("rpc-two", port);
    let ws = workspace("two");
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let a = rpc.create(&ws, json!({}));
    let b = rpc.create(&ws, json!({}));
    assert_ne!(a, b);
    rpc.send(
        json!({"id": "a1", "method": "session.prompt", "params": {"session": a, "prompt": "one"}}),
    );
    rpc.send(
        json!({"id": "b1", "method": "session.prompt", "params": {"session": b, "prompt": "two"}}),
    );
    let mut results = std::collections::HashMap::new();
    let mut lines = Vec::new();
    while results.len() < 2 {
        let line = rpc.next();
        if line.get("type").is_none() {
            results.insert(line["id"].as_str().unwrap().to_string(), line.clone());
        }
        lines.push(line);
    }
    // Every event belongs to exactly the session its request named.
    for line in lines.iter().filter(|l| l.get("type").is_some()) {
        let expected = if line["request"] == "a1" { &a } else { &b };
        assert_eq!(line["session"], expected.as_str(), "{line}");
    }
    let outputs: std::collections::HashSet<String> = results
        .values()
        .map(|r| r["result"]["final_output"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        outputs,
        ["ok".to_string(), "again".to_string()]
            .into_iter()
            .collect()
    );
    assert!(rpc.finish().success());
    server.join().unwrap();
}

#[test]
fn saved_sessions_are_listed_and_resume_with_their_history() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK]);
    let home = mock_home("rpc-saved", port);
    let ws = workspace("saved");
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let session = rpc.create(&ws, json!({"save": true, "name": "thread 42"}));
    rpc.send(json!({"id": "p", "method": "session.prompt", "params": {"session": session, "prompt": "hello"}}));
    let result = rpc.until_response("p").pop().unwrap();
    let path = result["result"]["path"].as_str().unwrap().to_string();
    assert!(std::path::Path::new(&path).is_file(), "{path}");

    let listed = rpc.call("l", "session.list", json!({"cwd": ws}));
    let sessions = listed["result"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["path"], path.as_str());
    assert_eq!(sessions[0]["name"], "thread 42");
    assert_eq!(sessions[0]["turns"], 1);

    rpc.call("x", "session.close", json!({"session": session}));
    let resumed = rpc.create(&ws, json!({"save": true, "resume": path}));
    let info = rpc.call("i", "session.info", json!({"session": resumed}));
    assert_eq!(info["result"]["messages"], 2);
    assert_eq!(info["result"]["name"], "thread 42");
    assert_eq!(info["result"]["path"], path.as_str());
    assert!(rpc.finish().success());
    server.join().unwrap();
}

/// Reading a saved conversation must not acquire a writer or repair its tail.
#[test]
fn memory_only_resume_leaves_the_saved_file_untouched() {
    let _lock = env_lock();
    let home = mock_home("rpc-read-only", 1);
    let ws = workspace("read-only");
    let path = home.dir.join("saved.jsonl");
    let original = concat!(
        "{\"type\":\"session\",\"id\":\"legacy\",\"cwd\":\"/tmp\",\"created\":1,\"model\":\"mock/test\"}\n",
        "{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"saved message\"}}\n",
        "{\"torn\":"
    );
    std::fs::write(&path, original).unwrap();
    for (flags, save) in [(&[][..], false), (&["--no-save"][..], true)] {
        let mut rpc = Rpc::spawn(&home, flags);
        let session = rpc.create(&ws, json!({"resume": path, "save": save}));
        let info = rpc.call("i", "session.info", json!({"session": session}));
        assert_eq!(info["result"]["messages"], 1);
        assert!(info["result"]["path"].is_null());
        assert!(rpc.finish().success());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }
}

/// A fork inherits the effort currently selected, including session.set changes.
#[test]
fn a_fork_keeps_the_current_effort() {
    let _lock = env_lock();
    let home = mock_home("rpc-fork-effort", 1);
    home.write("models.json", r#"{"providers":{"mock":{"base_url":"http://127.0.0.1:1","api":"completions","models":[{"id":"test","effort":["low","high"]}]}}}"#);
    let ws = workspace("fork-effort");
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let session = rpc.create(&ws, json!({"effort": "high"}));
    let changed = rpc.call(
        "s",
        "session.set",
        json!({"session": session, "effort": "low"}),
    );
    assert_eq!(changed["result"]["effort"], "low");
    let fork = rpc.call("f", "session.fork", json!({"session": session}));
    let info = rpc.call(
        "i",
        "session.info",
        json!({"session": fork["result"]["session"]}),
    );
    assert_eq!(info["result"]["effort"], "low");
    assert!(rpc.finish().success());
}

#[test]
fn a_version_one_line_answers_flat_and_streams_nothing() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK]);
    let home = mock_home("rpc-v1", port);
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    rpc.send(json!({"id": "legacy", "prompt": "say ok", "model": "mock/test"}));
    let first = rpc.next();
    assert_eq!(first["id"], "legacy");
    assert!(
        first.get("type").is_none() && first.get("result").is_none(),
        "flat: {first}"
    );
    assert_eq!(first["final_output"], "ok");
    assert_eq!(first["session"], Value::Null);
    // Nothing else follows: no events, no second line.
    assert!(matches!(
        rpc.lines.recv_timeout(Duration::from_millis(300)),
        Err(RecvTimeoutError::Timeout)
    ));
    assert!(rpc.finish().success());
    server.join().unwrap();
}

#[test]
fn bad_requests_each_get_one_error_line_and_the_server_keeps_serving() {
    let _lock = env_lock();
    let home = Home::new("rpc-errors");
    let ws = workspace("errors");
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let unknown = rpc.call("u", "session.info", json!({"session": "nope"}));
    assert!(unknown["error"]
        .as_str()
        .unwrap()
        .contains("unknown session"));
    let method = rpc.call("m", "session.dance", json!({}));
    assert!(method["error"].as_str().unwrap().contains("unknown method"));
    let missing = rpc.call("n", "session.create", json!({"cwd": ws.join("absent")}));
    assert!(missing["error"]
        .as_str()
        .unwrap()
        .contains("not a directory"));
    rpc.send_raw("{not json");
    let malformed = rpc.next();
    assert_eq!(malformed["id"], Value::Null);
    assert!(malformed["error"]
        .as_str()
        .unwrap()
        .contains("invalid request"));
    let hello = rpc.call("h", "hello", json!({}));
    assert_eq!(hello["result"]["protocol"], 2, "still serving");
    assert!(rpc.finish().success());
}

#[test]
fn steering_needs_a_running_turn() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK]);
    let home = mock_home("rpc-steer", port);
    let ws = workspace("steer");
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let session = rpc.create(&ws, json!({}));
    let idle = rpc.call(
        "s",
        "session.steer",
        json!({"session": session, "text": "also"}),
    );
    assert!(idle["error"]
        .as_str()
        .unwrap()
        .contains("no turn is running"));
    // One prompt so the mock's single response is consumed.
    rpc.send(json!({"id": "p", "method": "session.prompt", "params": {"session": session, "prompt": "go"}}));
    rpc.until_response("p");
    assert!(rpc.finish().success());
    server.join().unwrap();
}

#[test]
fn shutdown_answers_then_exits_zero() {
    let _lock = env_lock();
    let home = Home::new("rpc-shutdown");
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let bye = rpc.call("bye", "shutdown", json!({}));
    assert_eq!(bye["result"], json!({}));
    let status = rpc.child.wait().unwrap();
    assert!(status.success());
}

/// An extension whose `before_turn` hook asks the person a yes/no question
/// and logs the answer. Over `e rpc` that question must reach the client.
const ASKER: &str = r#"#!/bin/sh
log="$E_HOME/ext.log"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\),"method".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{"name":"asker","hooks":["before_turn"]}}\n' "$id" ;;
    *'"method":"hook.before_turn"'*)
      hook_id=$id
      printf '{"id":"q","method":"ui.confirm","params":{"title":"Deploy?","message":"to prod"}}\n' ;;
    *'"id":"q"'*)
      printf 'reply %s\n' "$line" >> "$log"
      printf '{"id":%s,"result":{}}\n' "$hook_id" ;;
    *'"method":"shutdown"'*) exit 0 ;;
  esac
done
"#;

#[test]
fn an_extension_question_reaches_the_client_and_its_answer_returns() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK]);
    let home = mock_home("rpc-ask", port);
    let ext = home.dir.join("extensions");
    std::fs::create_dir_all(&ext).unwrap();
    let path = ext.join("asker.sh");
    std::fs::write(&path, ASKER).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let ws = workspace("ask");
    let mut rpc = Rpc::spawn(&home, &[]);
    rpc.call("h", "hello", json!({"ask": true}));
    let session = rpc.create(&ws, json!({}));
    rpc.send(json!({"id": "p", "method": "session.prompt", "params": {"session": session, "prompt": "ship it"}}));
    let ask = loop {
        let line = rpc.next();
        if line["type"] == "ask" {
            break line;
        }
    };
    assert_eq!(ask["extension"], "asker");
    assert_eq!(ask["method"], "ui.confirm");
    assert_eq!(ask["params"]["title"], "Deploy?");
    let n = ask["ask"].as_u64().unwrap();
    let replied = rpc.call(
        "r",
        "ask.reply",
        json!({"ask": n, "result": {"confirmed": true}}),
    );
    assert_eq!(replied["result"], json!({}));
    let result = rpc.until_response("p").pop().unwrap();
    assert_eq!(result["result"]["final_output"], "ok");
    assert!(rpc.finish().success());
    let log = std::fs::read_to_string(home.dir.join("ext.log")).unwrap();
    assert!(log.contains(r#""confirmed":true"#), "{log}");
    server.join().unwrap();
}

#[test]
fn the_fixture_requests_are_all_understood() {
    let _lock = env_lock();
    let (port, server) = serve_sse(&[OK, AGAIN]);
    let home = mock_home("rpc-fixture", port);
    let ws = workspace("fixture");
    let fixture = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rpc/v2-requests.jsonl"),
    )
    .unwrap();
    let mut rpc = Rpc::spawn(&home, &["--no-extensions"]);
    let mut session = String::new();
    for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
        let line = line
            .replace("$CWD", &ws.display().to_string())
            .replace("$SESSION", &session);
        let request: Value = serde_json::from_str(&line).unwrap();
        let id = request["id"].as_str().unwrap();
        rpc.send_raw(&line);
        let response = rpc.until_response(id).pop().unwrap();
        assert!(
            response.get("error").is_none() || response["error"].is_null(),
            "{id}: {response}"
        );
        if id == "create" {
            session = response["result"]["session"].as_str().unwrap().to_string();
        }
        if id == "one-shot" {
            assert_eq!(response["final_output"], "again");
        }
    }
    assert!(std::path::Path::new(&ws).join("fixture.html").is_file());
    assert!(rpc.child.wait().unwrap().success());
    server.join().unwrap();
}
