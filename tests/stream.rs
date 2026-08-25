//! The agent session stream: provider events fold into one `SessionEvent`
//! channel, failures still end the turn exactly once, Esc aborts a stall,
//! and a retryable campaign either recovers or gives up. Dialect parsing
//! lives in `providers.rs`.

mod common;

use std::io::{Read, Write};
use std::time::Duration;

use common::{env_lock, serve_raw, serve_sse, sse_response, test_model, Home};

use e::core::agent::{Agent, SessionEvent};
use e::core::providers::catalog::Api;
use e::core::providers::FailureCause;

fn mock_home() -> Home {
    let home = Home::new("stream");
    home.auth(r#"{"mock":{"key":"k"}}"#);
    home
}

// The env lock is held across awaits: E_HOME must stay ours and each
// #[tokio::test] runs on its own runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn agent_folds_provider_events_into_one_session_stream() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, _server) = serve_sse(&[body]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut kinds = Vec::new();
    while let Some(event) = rx.recv().await {
        let done = matches!(event, SessionEvent::TurnEnd { .. });
        kinds.push(match event {
            SessionEvent::TurnStart => "start",
            SessionEvent::TextDelta(_) => "text",
            SessionEvent::Usage { .. } => "usage",
            SessionEvent::TurnEnd { .. } => "end",
            SessionEvent::ReasoningDelta(_) => "reasoning",
            SessionEvent::Error(_) => "error",
            _ => "other",
        });
        if done {
            break;
        }
    }
    assert_eq!(kinds.first(), Some(&"start"));
    assert_eq!(kinds.last(), Some(&"end"));
    assert!(kinds.contains(&"text") && kinds.contains(&"usage"));
    assert!(!kinds.contains(&"error"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn agent_reports_errors_and_still_ends_the_turn_exactly_once() {
    let _lock = env_lock();
    let (port, _server) = serve_raw(vec![
        "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
    ]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut saw_error = false;
    let mut endings = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::Error(_) => saw_error = true,
            SessionEvent::TurnEnd { aborted } => {
                endings.push(aborted);
                break;
            }
            _ => {}
        }
    }
    assert!(saw_error, "error event missing");
    assert_eq!(endings, vec![false]);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn agent_interrupt_ends_a_stalled_stream() {
    let _lock = env_lock();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let _home = mock_home();

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let _ = sock.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
        );
        std::thread::sleep(Duration::from_secs(30));
    });

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    while let Some(event) = rx.recv().await {
        if matches!(event, SessionEvent::TurnStart) {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    agent.interrupt();

    let aborted = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = rx.recv().await {
            if let SessionEvent::TurnEnd { aborted } = event {
                return aborted;
            }
        }
        panic!("turn ended without TurnEnd");
    })
    .await
    .expect("interrupt must end a stalled turn promptly");
    assert!(aborted, "stalled interrupt should abort the turn");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn agent_interrupt_ends_a_turn_stuck_before_headers() {
    let _lock = env_lock();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let _home = mock_home();

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        std::thread::sleep(Duration::from_secs(30));
    });

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    while let Some(event) = rx.recv().await {
        if matches!(event, SessionEvent::TurnStart) {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    agent.interrupt();

    let aborted = tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = rx.recv().await {
            if let SessionEvent::TurnEnd { aborted } = event {
                return aborted;
            }
        }
        panic!("turn ended without TurnEnd");
    })
    .await
    .expect("interrupt must end a pre-headers stall promptly");
    assert!(aborted, "pre-headers interrupt should abort the turn");
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn retry_recovers_after_a_transient_failure() {
    let _lock = env_lock();
    let ok = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, _server) = serve_raw(vec![
        "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
        sse_response(ok),
    ]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut saw_retry = false;
    let mut saw_recovered = false;
    let mut saw_error = false;
    let mut text = String::new();
    let mut aborted = None;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::Retry {
                attempt,
                limit,
                cause,
                ..
            } => {
                saw_retry = true;
                assert_eq!(attempt, 2);
                assert_eq!(limit, e::core::agent::retry::MAX_ATTEMPTS);
                assert_eq!(cause, FailureCause::ProviderUnavailable);
            }
            SessionEvent::Recovered { attempt, limit } => {
                saw_recovered = true;
                assert_eq!(attempt, 2);
                assert_eq!(limit, e::core::agent::retry::MAX_ATTEMPTS);
            }
            SessionEvent::TextDelta(d) => text.push_str(&d),
            SessionEvent::Error(_) => saw_error = true,
            SessionEvent::TurnEnd { aborted: a } => {
                aborted = Some(a);
                break;
            }
            _ => {}
        }
    }
    assert!(saw_retry, "Retry event missing");
    assert!(saw_recovered, "Recovered event missing");
    assert!(!saw_error, "should not surface an error after recovering");
    assert_eq!(text, "ok");
    assert_eq!(aborted, Some(false));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn retry_campaign_gives_up_after_max_attempts() {
    use e::core::agent::retry::MAX_ATTEMPTS;

    let _lock = env_lock();
    let fail =
        "HTTP/1.1 503 Service Unavailable\r\nretry-after: 0\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    let (port, _server) = serve_raw(vec![fail.to_string(); MAX_ATTEMPTS as usize]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut retries = Vec::new();
    let mut error_message = None;
    let aborted = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = rx.recv().await.expect("stream ended without TurnEnd");
            match event {
                SessionEvent::Retry { attempt, .. } => retries.push(attempt),
                SessionEvent::Error(message) => error_message = Some(message),
                SessionEvent::TurnEnd { aborted } => return aborted,
                _ => {}
            }
        }
    })
    .await
    .expect("the campaign must give up, not hang");

    assert_eq!(retries, (2..=MAX_ATTEMPTS).collect::<Vec<_>>());
    let message = error_message.expect("exhausted campaign must report an error");
    assert!(
        message.contains(&format!("{MAX_ATTEMPTS}/{MAX_ATTEMPTS}")),
        "attempt count missing: {message}"
    );
    assert!(
        message.contains("gave up"),
        "exhaustion wording missing: {message}"
    );
    assert!(!aborted);
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn a_blank_successful_stream_surfaces_an_error_not_silence() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":0}}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, _server) = serve_sse(&[body]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut saw_error = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::Error(message) => {
                saw_error = true;
                assert!(message.contains("empty"), "unexpected error: {message}");
            }
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(saw_error, "a blank success must surface an error");
}

/// Gemini sends thought text only as ReasoningDelta — never a committed
/// ReasoningItem — so a thinking-only stream that ends without text (here
/// MAX_TOKENS mid-thought) is a live model being truncated, not a blank
/// response. The turn must end with the truncation warning and no error.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn thinking_only_stream_is_not_an_empty_response() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"planning\",\"thought\":true}]}}]}\n\n",
        "data: {\"candidates\":[{\"finishReason\":\"MAX_TOKENS\"}]}\n\n",
    );
    let (port, _server) = serve_sse(&[body]);
    let home = Home::new("google-thinking");
    home.auth(r#"{"google":{"key":"g"}}"#);

    let (mut agent, mut rx) = Agent::new(test_model("google", port, Api::Google));
    agent.submit("hi".into(), "sys".into());

    let mut saw_reasoning = false;
    let mut saw_warning = false;
    let mut saw_error = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::ReasoningDelta(_) => saw_reasoning = true,
            SessionEvent::Warning(_) => saw_warning = true,
            SessionEvent::Error(_) => saw_error = true,
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(saw_reasoning, "thought deltas must stream");
    assert!(saw_warning, "truncation must surface as a warning");
    assert!(
        !saw_error,
        "a thinking-only stream is not an empty response"
    );
}

/// A 200 stream the provider cut at its output limit must not read as a
/// finished turn: the finish reason is mapped, and skipped malformed
/// payloads are counted instead of vanishing.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn truncation_and_malformed_payloads_surface_in_stream_end() {
    use e::core::providers::{stream, ChatMessage, Event, FinishReason, Request};

    let _lock = env_lock();
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        "data: {not json at all}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, _server) = serve_sse(&[body]);
    let _home = mock_home();

    let request = Request {
        model: test_model("mock", port, Api::Completions),
        system: "sys".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: None,
        tools: Vec::new(),
    };
    let (mut rx, _handle) = stream(request);

    let mut end = None;
    while let Some(event) = rx.recv().await {
        match event {
            Event::Done(e) => {
                end = Some(e);
                break;
            }
            Event::Error(err) => panic!("stream error: {}", err.message),
            _ => {}
        }
    }
    let end = end.unwrap();
    assert_eq!(end.finish, FinishReason::Length);
    assert_eq!(end.malformed, 1);
}

/// End to end: an abnormal finish becomes a visible turn warning, not a
/// silently accepted truncation.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn agent_warns_on_truncated_turn() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"cut off mid\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, _server) = serve_sse(&[body]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut warning = None;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::Warning(message) => warning = Some(message),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    let warning = warning.expect("a truncated turn must warn");
    assert!(
        warning.contains("truncated"),
        "unexpected warning: {warning}"
    );
}

/// A body transport failure after successful headers but before any output
/// is retryable — a 503 and an idle timeout already were, and this was the
/// inconsistent gap. The agent's nothing-produced guard still blocks replays
/// once content has streamed.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn body_transport_error_before_output_retries() {
    let _lock = env_lock();
    let ok = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, _server) = serve_raw(vec![
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 500\r\nconnection: close\r\n\r\ndata: {\"cho".into(),
        sse_response(ok),
    ]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut saw_retry = false;
    let mut saw_error = false;
    let mut text = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::Retry { .. } => saw_retry = true,
            SessionEvent::Error(_) => saw_error = true,
            SessionEvent::TextDelta(d) => text.push_str(&d),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(saw_retry, "a pre-output body failure must retry");
    assert!(!saw_error, "the retry should recover the turn");
    assert_eq!(text, "ok");
}
