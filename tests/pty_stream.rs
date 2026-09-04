//! Real-PTY streaming contract: a sustained provider stream reaches its tail
//! and a graceful exit restores the terminal modes the TUI enabled.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

struct Fixture {
    root: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "e-pty-stream-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> std::path::PathBuf {
        self.root.join("home")
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.root.join("workspace")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

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
            let content = if index == 399 {
                "PTY_STREAM_FINISHED\n".to_string()
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

#[test]
fn sustained_stream_finishes_and_restores_the_terminal() {
    let fixture = Fixture::new();
    let (port, server) = streaming_server();
    std::fs::write(
        fixture.home().join("models.json"),
        format!(
            r#"{{"providers":{{"mock":{{"base_url":"http://127.0.0.1:{port}","api":"openai-completions","catalog":"none","supports_tools":false,"models":["stream"]}}}}}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        fixture.home().join("auth.json"),
        r#"{"mock":{"key":"test"}}"#,
    )
    .unwrap();
    let workspace = fixture.workspace().canonicalize().unwrap();
    std::fs::write(
        fixture.home().join("trust.json"),
        serde_json::to_vec(&serde_json::json!({
            (workspace.to_str().unwrap()): {"trusted": true}
        }))
        .unwrap(),
    )
    .unwrap();

    let capture = fixture.root.join("stream.raw");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ptycap.py");
    let mut child = Command::new("python3")
        .arg(script)
        .arg(&capture)
        .args(["100", "30", "0.5", "20"])
        .arg(env!("CARGO_BIN_EXE_e"))
        .args([
            "--no-save",
            "--no-tools",
            "--no-extensions",
            "--model",
            "mock/stream",
        ])
        .current_dir(&workspace)
        .env("E_HOME", fixture.home())
        .env("CAP_PROMPT", "stream")
        .env("CAP_WAIT_FOR", "PTY_STREAM_FINISHED")
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
    let served = server.join();
    let raw = std::fs::read(&capture).unwrap();
    assert!(
        served.is_ok(),
        "mock was not reached; capture: {}",
        String::from_utf8_lossy(&raw)
    );
    assert!(
        output.status.success(),
        "pty capture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        raw.windows(b"PTY_STREAM_FINISHED".len())
            .any(|window| window == b"PTY_STREAM_FINISHED"),
        "the terminal never received the stream tail"
    );
    assert!(
        raw.windows(b"\x1b[?2004l".len())
            .any(|window| window == b"\x1b[?2004l"),
        "bracketed paste was not disabled on exit"
    );
    assert!(
        raw.windows(b"\x1b[?25h".len())
            .any(|window| window == b"\x1b[?25h"),
        "the cursor was not restored on exit"
    );
}
