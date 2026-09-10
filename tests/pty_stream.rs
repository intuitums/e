//! Real-PTY session transitions: stream and tool tails survive resize, native
//! history is never erased, and graceful exit restores terminal modes.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

mod common;
use common::{env_lock, serve_sse, Home};

/// Serve enough individually flushed deltas to exercise event batching,
/// Markdown pacing, the paint mailbox, scrolling, and final-frame delivery.
fn streaming_server() -> (u16, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "TUI never requested the mock provider"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("mock accept failed: {error}"),
            }
        };
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = vec![0; 256 * 1024];
        let _ = socket.read(&mut request);

        let mut events = Vec::new();
        for index in 0..400 {
            let content = if index == 0 {
                "# Streaming\n\n```text\n".to_string()
            } else if index == 399 {
                "```\n\nPTY_STREAM_FINISHED\n".to_string()
            } else {
                format!("row {index:04} {}\n", "streaming text ".repeat(10))
            };
            let json = serde_json::json!({
                "choices": [{"delta": {"content": content}}]
            });
            events.push(format!("data: {json}\n\n"));
        }
        events.push("data: [DONE]\n\n".into());
        let content_len: usize = events.iter().map(String::len).sum();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {content_len}\r\nconnection: close\r\n\r\n"
        )
        .unwrap();
        for event in events {
            socket.write_all(event.as_bytes()).unwrap();
            socket.flush().unwrap();
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    (port, server)
}

/// Capture one session in a shared isolated home. Retain synthetic captures
/// under E_PTY_ARTIFACTS when manually replaying frames with scripts/term.py.
fn capture_session(home: &Home, port: u16, tools: bool, marker: &str, resize: &str) -> Vec<u8> {
    home.write("models.json", format!(
        r#"{{"providers":{{"mock":{{"base_url":"http://127.0.0.1:{port}","api":"openai-completions","catalog":"none","supports_tools":{tools},"models":["stream"]}}}}}}"#
    ));
    home.auth(r#"{"mock":{"key":"test"}}"#);
    let workspace = home.dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    home.write(
        "trust.json",
        serde_json::to_vec(&serde_json::json!({
            (workspace.to_str().unwrap()): {"trusted": true}
        }))
        .unwrap(),
    );

    let capture = home.dir.join("stream.raw");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ptycap.py");
    let mut command = Command::new("python3");
    command
        .arg(script)
        .arg(&capture)
        .args(["100", "30", "0.1", "20"])
        .arg(env!("CARGO_BIN_EXE_e"))
        .args(["--no-save", "--no-extensions", "--model", "mock/stream"]);
    if !tools {
        command.arg("--no-tools");
    }
    let mut child = command
        .current_dir(&workspace)
        .env("E_HOME", &home.dir)
        .env("CAP_PROMPT", "stream")
        .env("CAP_WAIT_FOR", marker)
        .env("CAP_RESIZE_AFTER", resize)
        .env("CAP_RESIZE_COLS", "72")
        .env("CAP_RESIZE_ROWS", "18")
        .env("CAP_EXIT", "\u{3}\u{3}")
        .env("CAP_EXIT_WAIT", "3")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if std::time::Instant::now() >= deadline {
            break true;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if timed_out {
        let _ = child.kill();
    }
    let output = child.wait_with_output().unwrap();
    assert!(!timed_out, "PTY capture exceeded its hard deadline");
    assert!(
        output.status.success(),
        "pty capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = std::fs::read(&capture).unwrap();
    let sizes = std::fs::read(capture.with_extension("raw.sizes.json")).unwrap();
    if let Some(dir) = std::env::var_os("E_PTY_ARTIFACTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{marker}.raw")), &raw).unwrap();
        std::fs::write(dir.join(format!("{marker}.raw.sizes.json")), &sizes).unwrap();
    }
    let sizes: Vec<serde_json::Value> = serde_json::from_slice(&sizes).unwrap();
    assert_eq!(
        sizes.len(),
        1,
        "the session never reached its resize trigger"
    );
    assert!(
        !raw.windows(4).any(|w| w == b"\x1b[3J"),
        "resize erased native history"
    );
    assert!(
        raw.windows(marker.len()).any(|w| w == marker.as_bytes()),
        "terminal never received the final reply"
    );
    assert!(
        raw.windows(b"\x1b[?2004l".len())
            .any(|w| w == b"\x1b[?2004l"),
        "bracketed paste was not disabled on exit"
    );
    assert!(
        raw.windows(b"\x1b[?25h".len()).any(|w| w == b"\x1b[?25h"),
        "cursor was not restored on exit"
    );
    raw
}

#[test]
fn sustained_stream_survives_resize_and_restores_the_terminal() {
    let _lock = env_lock();
    let home = Home::new("pty-stream");
    let (port, server) = streaming_server();
    capture_session(&home, port, false, "PTY_STREAM_FINISHED", "row 0100");
    server.join().unwrap();
}

#[test]
fn tool_completion_after_resize_keeps_the_final_reply() {
    let _lock = env_lock();
    let home = Home::new("pty-tool");
    let call = serde_json::json!({"choices": [{"delta": {"tool_calls": [{
        "index": 0, "id": "c1", "function": {
            "name": "bash",
            "arguments": serde_json::json!({"command": "printf 'TOOL_RESIZE\\n'; sleep 0.3; printf 'tool complete\\n'"}).to_string()
        }
    }]}}]});
    let first = format!("data: {call}\n\ndata: [DONE]\n\n");
    let reply =
        serde_json::json!({"choices": [{"delta": {"content": "## Result\n\nPTY_TOOL_FINISHED"}}]});
    let last = format!("data: {reply}\n\ndata: [DONE]\n\n");
    let (port, server) = serve_sse(&[&first, &last]);
    let raw = capture_session(&home, port, true, "PTY_TOOL_FINISHED", "TOOL_RESIZE");
    let requests = server.join().unwrap();
    assert!(
        requests[1].contains("tool complete"),
        "tool output never reached the follow-up request"
    );
    assert!(
        String::from_utf8_lossy(&raw).contains("Ran"),
        "completed tool row never painted"
    );
}

/// The global exit chord must reach the app even while a panel owns input.
#[test]
fn ctrl_c_exits_modal_panels_without_recording_trust() {
    let _lock = env_lock();
    common::clear_env_keys();
    for (name, prompt, marker) in [
        ("trust", "", "Trust this directory"),
        ("settings", "/settings", "Settings"),
        ("login", "/login", "Sign in"),
        ("viewer", "\u{f}", "Review"),
    ] {
        let home = Home::new("pty-panel-exit");
        home.write("models.json", r#"{"providers":{"mock":{"base_url":"http://127.0.0.1:1","catalog":"none","models":["audit"]}}}"#);
        home.auth(r#"{"mock":{"key":"synthetic"}}"#);
        home.write("settings.json", r#"{"auto_update":"off"}"#);
        let workspace = home.dir.join(if name == "trust" {
            "workspace\x07\x1b]0;BOUNTY_INJECTED\x07"
        } else {
            "workspace"
        });
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        if name != "trust" {
            home.write(
                "trust.json",
                serde_json::to_vec(&serde_json::json!({
                    (workspace.to_str().unwrap()): {"trusted": true}
                }))
                .unwrap(),
            );
        }
        let capture = home.dir.join("panel.raw");
        let output = Command::new("python3")
            .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ptycap.py"))
            .arg(&capture)
            .args(["100", "30", "0.2", "0.7"])
            .arg(env!("CARGO_BIN_EXE_e"))
            .args([
                "--no-save",
                "--no-extensions",
                "--no-tools",
                "--model",
                "mock/audit",
            ])
            .current_dir(&workspace)
            .env("E_HOME", &home.dir)
            .env("CAP_PROMPT", prompt)
            .env("CAP_EXIT", "\u{3}\u{3}")
            .env("CAP_EXIT_WAIT", "2")
            .env_remove("CAP_WAIT_FOR")
            .env_remove("CAP_RESIZE_AFTER")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let raw = std::fs::read(&capture).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains(marker), "{name} panel never opened");
        // ptycap saves bytes before its fallback SIGTERM. These cleanup bytes
        // prove Ctrl+C itself exited, not the capture script's forced stop.
        assert!(text.contains("\x1b[?2004l"), "Ctrl+C did not exit {name}");
        if name == "trust" {
            assert!(
                !text.contains("\x1b]0;BOUNTY_INJECTED\x07"),
                "path controls escaped through the trust UI or tab title"
            );
            assert!(
                !home.dir.join("trust.json").exists(),
                "exit wrote a trust decision"
            );
        }
    }
}
