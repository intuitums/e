//! Regression coverage for terminal safety, trust layout, and stream failures.

mod common;

use e::tui::composer::{Editor, Key};

/// Pasted terminal controls must remain data, even before prompt submission.
#[test]
fn pasted_terminal_controls_are_inert() {
    let _lock = common::env_lock();
    let _home = common::Home::new("bounty-paste");
    let mut editor = Editor::new();
    let payload = "hello\x1b]0;BOUNTY_INJECTED\x07world\u{9b}2J";
    editor.insert_paste(payload);
    let theme = e::tui::theme::load_bundled(false).unwrap();
    for key in [Key::End, Key::Home, Key::SelectEnd] {
        editor.key(key);
        let frame = editor.render(&theme, 100, 30).join("\n");
        assert!(!frame.contains("\x1b]0;BOUNTY_INJECTED\x07"));
        assert!(!frame.contains('\u{9b}'));
        assert_eq!(
            editor.text(),
            payload,
            "display safety must not rewrite the draft"
        );
    }
}

/// Asking for trust must not execute terminal controls in the untrusted path.
#[test]
fn trust_question_treats_path_controls_as_data() {
    use e::tui::trustpanel::{render, TrustStage};
    let stage = TrustStage {
        selected: 0,
        scroll: Some(0),
        parent: Some("/tmp/parent\x1b]0;BOUNTY_INJECTED\x07".into()),
    };
    let theme = e::tui::theme::load_bundled(false).unwrap();
    let frame = render(
        &stage,
        &theme,
        100,
        "/tmp/workspace\x1b]0;BOUNTY_INJECTED\x07",
    )
    .join("\n");
    assert!(!frame.contains("\x1b]0;BOUNTY_INJECTED\x07"));
}

/// Up from two wide glyphs should stay at display column four, not char two.
#[test]
fn vertical_motion_preserves_display_column() {
    let _lock = common::env_lock();
    let _home = common::Home::new("bounty-cursor");
    let theme = e::tui::theme::load_bundled(false).unwrap();
    let mut editor = Editor::new();
    editor.set_text("abcd\n界界");
    editor.render(&theme, 100, 30);
    editor.key(Key::Up);
    assert_eq!(editor.cursor(), 4);
}

/// A terminal SSE error must fail the request even after visible output.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn completions_error_after_text_is_not_success() {
    use e::core::providers::{self, catalog::Api, ChatMessage, Event, Request};
    let _lock = common::env_lock();
    let home = common::Home::new("bounty-wire-error");
    home.auth(r#"{"mock":{"key":"synthetic"}}"#);
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"unrun\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: {\"error\":{\"message\":\"upstream quota exhausted\",\"type\":\"insufficient_quota\",\"code\":429}}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, server) = common::serve_sse(&[body]);
    let (mut events, task) = providers::stream(Request {
        model: common::test_model("mock", port, Api::Completions),
        system: "test".into(),
        messages: vec![ChatMessage::user("test")],
        effort: None,
        session_id: String::new(),
        tools: Vec::new(),
    });
    let mut failed = false;
    let mut done = false;
    while let Some(event) = events.recv().await {
        match event {
            Event::Error(error) => {
                assert_eq!(error.message, "upstream quota exhausted");
                assert_eq!(error.cause, providers::FailureCause::QuotaExhausted);
                assert!(!error.cause.is_retryable());
                failed = true;
            }
            Event::ToolCall(_) => panic!("error flushed an unrun tool call"),
            Event::Done(_) => {
                done = true;
                break;
            }
            _ => {}
        }
    }
    assert!(!done, "a failed stream must not emit Done");
    task.await.unwrap();
    server.join().unwrap();
    assert!(
        failed,
        "provider error was reported as a successful completion"
    );
}

/// Every wrapped trust row stays reachable in a short terminal, and choosing
/// an offscreen option reveals its label without recording a decision.
#[test]
fn narrow_trust_text_can_be_paged_without_losing_rows() {
    use e::tui::markdown::visible_width;
    use e::tui::trustpanel::{render, render_view, TrustStage};
    let _lock = common::env_lock();
    let _home = common::Home::new("bounty-trust-scroll");
    let theme = e::tui::theme::load_bundled(false).unwrap();
    let mut stage = TrustStage {
        selected: 0,
        scroll: Some(0),
        parent: Some("/tmp/long-parent/with-a-specific-scope".into()),
    };
    let dir = "/tmp/long-parent/with-a-specific-scope/workspace";
    let full = render(&stage, &theme, 32, dir);
    assert!(full.iter().all(|row| visible_width(row) <= 32));
    let mut seen = Vec::new();
    for _ in 0..full.len() {
        let rows = render_view(&mut stage, &theme, 32, 9, dir);
        assert!(rows.len() <= 9);
        assert!(rows.iter().all(|row| visible_width(row) <= 32));
        seen.extend(rows);
        stage.page(1, 32, 9);
    }
    // The last row is the non-scrolling hint. All actual content must appear
    // in some page with identical styling and no lost fragments.
    for row in &full[..full.len() - 1] {
        assert!(seen.contains(row), "unreachable trust row: {row:?}");
    }
    stage.step(1);
    let selected = render_view(&mut stage, &theme, 32, 9, dir).join("\n");
    let selected_text = e::core::tools::strip_ansi(&selected)
        .split_whitespace()
        .collect::<String>();
    assert!(
        selected_text.contains("›Trust/tmp/long-parent/with-a-specific-scope"),
        "{selected:?}"
    );
    assert_eq!(stage.choice(), (stage.parent.clone(), true));
    stage.page(-100, 32, 9);
    let first = render_view(&mut stage, &theme, 32, 9, dir).join("\n");
    let first_text = e::core::tools::strip_ansi(&first)
        .split_whitespace()
        .collect::<String>();
    assert!(
        first_text.contains(dir),
        "the question must show the complete workspace path: {first:?}"
    );
}

/// A truncated HTTP body produces reqwest's decoding headline even when all
/// delivered SSE JSON is valid. Preserve the transport cause for diagnosis.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn truncated_response_body_reports_the_underlying_transport_failure() {
    use e::core::providers::{self, catalog::Api, ChatMessage, Event, Request};
    let _lock = common::env_lock();
    let home = common::Home::new("bounty-body-error");
    home.auth(r#"{"mock":{"key":"synthetic"}}"#);
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"valid partial text\"}}]}\n\n";
    let (port, server) = common::serve_raw(vec![format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 500\r\nconnection: close\r\n\r\n{body}"
    )]);
    let (mut events, task) = providers::stream(Request {
        model: common::test_model("mock", port, Api::Completions),
        system: "test".into(),
        messages: vec![ChatMessage::user("test")],
        effort: None,
        session_id: String::new(),
        tools: Vec::new(),
    });
    let mut text = String::new();
    let mut failure = None;
    while let Some(event) = events.recv().await {
        match event {
            Event::TextDelta(delta) => text.push_str(&delta),
            Event::Error(error) => failure = Some(error),
            Event::Done(_) => panic!("truncated body reported success"),
            _ => {}
        }
    }
    task.await.unwrap();
    server.join().unwrap();
    assert_eq!(text, "valid partial text");
    let error = failure.expect("the body was truncated");
    assert!(error.message.starts_with("provider response interrupted:"));
    assert!(
        error
            .message
            .contains("end of file before message length reached"),
        "{}",
        error.message
    );
}

/// Error prefixes consume columns too. A wrapped transport diagnostic must
/// retain its first row's tail rather than losing it at the painter's clip.
#[test]
fn long_error_diagnostics_fit_without_losing_text() {
    use e::tui::markdown::visible_width;
    use e::tui::transcript::{Block, Kind};
    let theme = e::tui::theme::load_bundled(false).unwrap();
    let message = "provider response interrupted: error decoding response body: error reading a body from connection: end of file before message length reached";
    for width in [8, 32, 100] {
        let rows = Block::new(Kind::Error, message).lines_for_test(&theme, width);
        assert!(rows.iter().all(|row| visible_width(row) <= width));
        let text = e::core::tools::strip_ansi(&rows.join("\n"))
            .split_whitespace()
            .collect::<String>();
        assert_eq!(
            text,
            format!("●Error:{}", message.split_whitespace().collect::<String>())
        );
    }
}
