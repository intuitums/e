//! Security boundaries exercised with isolated homes and loopback servers.
mod common;

#[tokio::test]
async fn authenticated_http_never_follows_redirects() {
    for status in [301, 302, 303, 307, 308] {
        let destination = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let (port, server) = common::serve_raw(vec![format!(
            "HTTP/1.1 {status} Redirect\r\nLocation: http://{}/sink\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            destination.local_addr().unwrap()
        )]);
        let response = e::core::providers::http()
            .unwrap()
            .post(format!("http://127.0.0.1:{port}/request"))
            .header("x-api-key", "dummy")
            .header("x-goog-api-key", "dummy")
            .bearer_auth("dummy")
            .body("private dummy prompt")
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        assert!(e::core::providers::require_success(response).await.is_err());
        assert_eq!(
            destination.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        server.join().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn session_permissions_cover_new_and_legacy_state() {
    use std::os::unix::fs::PermissionsExt;
    let _lock = common::env_lock();
    let home = common::Home::new("private-sessions");
    std::fs::set_permissions(&home.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let legacy_dir = home.dir.join("sessions/legacy");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    let legacy = legacy_dir.join("old.jsonl");
    let legacy_data = include_str!("fixtures/sessions/v0.jsonl");
    std::fs::write(&legacy, legacy_data).unwrap();
    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o644)).unwrap();

    let log = e::core::session::SessionLog::create(&home.dir, "dummy/model").unwrap();
    for dir in [
        &home.dir,
        &home.dir.join("sessions"),
        log.path().parent().unwrap(),
    ] {
        assert_eq!(
            std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(
        std::fs::metadata(log.path()).unwrap().permissions().mode() & 0o077,
        0
    );
    let reopened = e::core::session::SessionLog::reopen(&legacy).unwrap();
    assert_eq!(
        std::fs::metadata(reopened.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(std::fs::read_to_string(&legacy).unwrap(), legacy_data);
}

#[test]
fn tool_labels_never_emit_terminal_controls() {
    use e::tui::transcript::{ToolChild, ToolState, Transcript};
    let payload = "\x1b]52;c;RFVNTVk=\x07\x1b[2J";
    let theme = e::tui::theme::load_bundled(false).unwrap();
    for (tool, key) in [("read", "path"), ("bash", "command"), ("grep", "pattern")] {
        let shown =
            e::core::tools::present(tool, &serde_json::json!({key: format!("file{payload}")}));
        assert!(!shown.target.contains('\x1b'));
    }
    let mut child = ToolChild::pending(
        1,
        format!("read{payload}"),
        format!("Reading{payload}"),
        format!("Read{payload}"),
        format!("file{payload}"),
    );
    child.state = ToolState::Completed;
    let mut transcript = Transcript::default();
    transcript.extend_tool_group(vec![child]);
    let frame = transcript.render(&theme, 180).join("\n");
    assert!(frame.contains("Read file"));
    assert!(!frame.contains("\x1b]52"));
    assert!(!frame.contains("\x1b[2J"));
    transcript.blocks[0].start_tool(1);
    let live = transcript.blocks[0].overlay_rows(&theme, 180);
    assert!(!live.join("\n").contains("\x1b]52"));
}
