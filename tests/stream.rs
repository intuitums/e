//! The agent session stream: provider events fold into one `SessionEvent`
//! channel, failures still end the turn exactly once, Esc aborts a stall,
//! and a retryable campaign either recovers or gives up. Dialect parsing
//! lives in `providers.rs`.

mod common;

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
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

// A resumed session or a mid-session model switch can carry image-bearing
// history from an earlier, image-capable model forward to one that isn't.
// Unstripped, the whole turn gets rejected by a backend that doesn't
// understand image content — this pins that the agent turn loop actually
// calls the strip (not just that the pure function works in isolation).
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn historical_images_are_stripped_for_a_model_that_cannot_accept_them() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, server) = serve_sse(&[body]);
    let _home = mock_home();

    let model = test_model("mock", port, Api::Completions); // image_input: false
    let (mut agent, mut rx) = Agent::new(model);
    agent.load_history(vec![e::core::providers::ChatMessage::user_with_images(
        "look at this",
        vec![e::core::providers::ImageInput {
            media_type: "image/png".into(),
            data: std::sync::Arc::from("aGVsbG8="),
        }],
    )]);
    agent.submit("continue".into(), "sys".into());

    while let Some(event) = rx.recv().await {
        if matches!(event, SessionEvent::TurnEnd { .. }) {
            break;
        }
    }

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0].contains("aGVsbG8="),
        "a historical image must not reach a model that can't accept it"
    );
    assert!(
        requests[0].contains("is not declared image-capable"),
        "history should note that an image was omitted, not just silently drop it"
    );
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
    // A blank success earns one quiet re-request before the error, so the
    // mock must answer twice: blank, then blank again.
    let (port, _server) = serve_sse(&[body, body]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut saw_retry = false;
    let mut saw_error = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::Retry { reason, .. } => {
                saw_retry = true;
                assert!(reason.contains("empty"), "unexpected retry: {reason}");
            }
            SessionEvent::Error(message) => {
                saw_error = true;
                assert!(message.contains("empty"), "unexpected error: {message}");
            }
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(saw_retry, "the first blank success gets one re-request");
    assert!(saw_error, "a second blank success must surface an error");
}

/// A zero attempt budget means the unavoidable initial request is the only
/// request: neither ordinary failures nor blank successes get a follow-up.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn zero_attempt_budget_disables_every_retry_path() {
    let _lock = env_lock();
    let fail = "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    let blank = concat!(
        "data: {\"choices\":[{\"delta\":{}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (failure_port, failure_server) = serve_raw(vec![fail.into()]);
    let (blank_port, blank_server) = serve_sse(&[blank]);
    let home = mock_home();
    home.write("settings.json", r#"{"retry_max_attempts":0}"#);

    for port in [failure_port, blank_port] {
        let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
        agent.submit("hi".into(), "sys".into());
        let mut retries = 0;
        let mut error = None;
        while let Some(event) = rx.recv().await {
            match event {
                SessionEvent::Retry { .. } => retries += 1,
                SessionEvent::Error(message) => error = Some(message),
                SessionEvent::TurnEnd { .. } => break,
                _ => {}
            }
        }
        assert_eq!(retries, 0, "zero budget must not emit a retry");
        let error = error.expect("the initial failure must still be surfaced");
        assert!(
            !error.contains("/0 attempts"),
            "invalid attempt count: {error}"
        );
    }

    assert_eq!(failure_server.join().unwrap().len(), 1);
    assert_eq!(blank_server.join().unwrap().len(), 1);
}

/// The queued-prompt review is a keyed edit surface over a queue the turn
/// keeps steering: in-place replacement and removal are exact on idle
/// keys, an edit to a key the turn already drained lands as a fresh
/// steering message, and the turn ends on its own — nothing holds.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn queue_review_edits_survive_concurrent_drain() {
    let _lock = env_lock();
    let reply = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, _server) = serve_sse(&[reply, reply]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));

    // The keyed semantics, deterministic on an idle agent: a stale key
    // submits fresh under a new key; a live key edits in place; removal
    // drops the entry.
    agent.update_queued(vec![(7, "a".into())], vec![]);
    let entries = agent.queue_snapshot();
    assert_eq!(entries.len(), 1);
    let (key, ref text) = entries[0];
    assert_eq!(text, "a");
    assert_ne!(key, 7, "a stale key submits fresh, under its own key");
    agent.update_queued(vec![(key, "b".into())], vec![]);
    assert_eq!(agent.queue_snapshot(), vec![(key, "b".to_string())]);
    agent.update_queued(vec![], vec![key]);
    assert!(agent.queue_snapshot().is_empty());

    // A queue seeded before the turn starts steers in order, once.
    agent.update_queued(vec![(101, "edited".into()), (102, "late".into())], vec![]);
    agent.submit("hi".into(), "sys".into());
    let mut steered = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::Steered(text) => steered.push(text),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert_eq!(steered, vec!["edited".to_string(), "late".to_string()]);
    assert_eq!(agent.on_turn_end(), Vec::<String>::new());

    // An edit keyed to an entry the turn already drained submits fresh —
    // the user's edited intent, not a resurrection of the original.
    agent.submit("second".into(), "sys".into());
    agent.update_queued(vec![(u64::MAX, "fresh".into())], vec![]);
    let mut steered = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::Steered(text) => steered.push(text),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(steered.contains(&"fresh".to_string()));
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

/// One scripted behavior per POST the agent makes.
enum Act {
    /// Send one text delta, hold the socket, then drop it mid-stream.
    Hold,
    /// Close before any bytes — a loss with nothing produced.
    Close,
    /// Answer with a complete, successful SSE stream.
    Answer(&'static str),
}

// A raw server scripting each POST the agent makes (GET /models gets the
// empty catalog). Holds and closes reproduce suspend-killed sockets: one
// delta then EOF, or silence before headers.
fn sleepy_server(script: Vec<Act>) -> u16 {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = Arc::new(Mutex::new(script.into_iter()));
    std::thread::spawn(move || loop {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        let act = script.lock().unwrap_or_else(|e| e.into_inner()).next();
        let Some(act) = act else { return };
        let mut buf = vec![0u8; 262144];
        let n = sock.read(&mut buf).unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).into_owned();
        if request.starts_with("GET") {
            sock.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"object\":\"list\",\"data\":[]}",
            )
            .unwrap();
            continue;
        }
        match act {
            Act::Hold => {
                let body = "data: {\"choices\":[{\"delta\":{\"content\":\"half a reply\"}}]}\n\n";
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
                );
                sock.write_all(response.as_bytes()).unwrap();
                sock.flush().unwrap();
                std::thread::sleep(Duration::from_millis(600));
                // Dropping the socket closes it mid-stream.
            }
            Act::Close => {
                // Hold briefly so tests can inject a gap while the request
                // is in flight, then drop with no response — a loss with
                // nothing produced.
                std::thread::sleep(Duration::from_millis(400));
            }
            Act::Answer(text) => {
                let body = format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n\
                     data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":5,\"completion_tokens\":1}}}}\n\n\
                     data: [DONE]\n\n"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
                );
                sock.write_all(response.as_bytes()).unwrap();
            }
        }
    });
    port
}

// The device slept mid-reply and woke inside the window: the partial reply
// is committed, a continuation turn finishes the sentence, and the events
// say so — Slept, the continuation as a user turn, no error.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn a_sleep_under_the_window_resumes_over_the_committed_partial() {
    let _lock = env_lock();
    let port = sleepy_server(vec![Act::Hold, Act::Answer("and now it is finished")]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut saw_text = false;
    let mut saw_slept = false;
    let mut saw_steered = false;
    let mut final_text = String::new();
    let mut errored = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::TextDelta(delta) => {
                if !saw_text {
                    // The attempt is in flight; the machine "wakes" now.
                    agent.inject_sleep_gap(Duration::from_secs(60));
                    saw_text = true;
                }
                final_text.push_str(&delta);
            }
            SessionEvent::Slept { duration_secs } => {
                assert_eq!(duration_secs, 60);
                saw_slept = true;
            }
            SessionEvent::Steered(text) => {
                assert!(text.contains("Continue from exactly where it stopped"));
                saw_steered = true;
            }
            SessionEvent::Error(_) => errored = true,
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(saw_text && saw_slept && saw_steered);
    assert!(!errored, "a resumed turn is not a failed turn");
    assert!(
        final_text.contains("half a reply") && final_text.contains("finished"),
        "the continuation completes the sentence: {final_text}"
    );
    // History is honest: partial reply, continuation message, completion.
    let history = agent.history_snapshot();
    let user_messages = history.iter().filter(|m| m.role == "user").count();
    let partial = history
        .iter()
        .any(|m| m.role == "assistant" && m.content.contains("half a reply"));
    assert!(partial, "the watched partial stays in history");
    assert!(user_messages >= 2, "the continuation is a real user turn");
}

// Asleep past the resume window: the turn stops with the system line and
// an aborted end — a stop, not an error.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn a_sleep_past_the_window_stops_the_turn() {
    let _lock = env_lock();
    let port = sleepy_server(vec![Act::Hold]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut saw_text = false;
    let mut saw_stop = false;
    let mut saw_warning = false;
    let mut aborted = false;
    let mut errored = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::TextDelta(_) => {
                if !saw_text {
                    agent.inject_sleep_gap(Duration::from_secs(600));
                    saw_text = true;
                }
            }
            SessionEvent::SleepStopped { duration_secs } => {
                assert_eq!(duration_secs, 600);
                saw_stop = true;
            }
            SessionEvent::Warning(message) => saw_warning = message.contains("run stopped"),
            SessionEvent::Error(_) => errored = true,
            SessionEvent::TurnEnd { aborted: a } => {
                aborted = a;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_text && saw_stop && saw_warning && aborted);
    assert!(!errored, "the sleep stop is a stop, not an error");
}

// Asleep before anything streamed: the replay is immediate and invisible —
// no continuation turn, no error, just the fresh attempt finishing.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn a_sleep_before_any_output_replays_invisibly() {
    let _lock = env_lock();
    let port = sleepy_server(vec![Act::Close, Act::Answer("recovered")]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());
    // Give the request time to be in flight, then wake the machine.
    tokio::time::sleep(Duration::from_millis(150)).await;
    agent.inject_sleep_gap(Duration::from_secs(30));

    let mut saw_slept = false;
    let mut saw_text = false;
    let mut saw_steered = false;
    let mut errored = false;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::Slept { duration_secs } => {
                assert_eq!(duration_secs, 30);
                saw_slept = true;
            }
            SessionEvent::TextDelta(_) => saw_text = true,
            SessionEvent::Steered(_) => saw_steered = true,
            SessionEvent::Error(_) => errored = true,
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert!(saw_slept && saw_text);
    assert!(!saw_steered, "nothing streamed, so no continuation turn");
    assert!(!errored);
}

// Reaching the continuation cap is a stop, not a silent recovery: after the
// last allowed continuation the next mid-reply sleep must stop the turn in the
// cancel family (SleepStopped + "continuation cap" warning + aborted end) and
// must NOT commit the truncated reply as a normal turn or run its tool calls.
// Regression for the cap branch omitting `errored`, which let execution fall
// through to the success path.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn reaching_the_continuation_cap_stops_the_turn() {
    let _lock = env_lock();
    // Four mid-reply losses: three continuations (cap is 3), the fourth caps.
    let port = sleepy_server(vec![Act::Hold, Act::Hold, Act::Hold, Act::Hold]);
    let _home = mock_home();

    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("hi".into(), "sys".into());

    let mut slept = 0;
    let mut stopped = false;
    let mut saw_cap_warning = false;
    let mut aborted = false;
    let mut errored = false;
    while let Some(event) = rx.recv().await {
        match event {
            // Each attempt streams one delta before the socket drops; wake the
            // machine while that attempt is in flight so every loss is a
            // mid-reply sleep under the window.
            SessionEvent::TextDelta(_) => agent.inject_sleep_gap(Duration::from_secs(60)),
            SessionEvent::Slept { .. } => slept += 1,
            SessionEvent::SleepStopped { .. } => stopped = true,
            SessionEvent::Warning(message) => {
                if message.contains("continuation cap") {
                    saw_cap_warning = true;
                }
            }
            SessionEvent::Error(_) => errored = true,
            SessionEvent::TurnEnd { aborted: a } => {
                aborted = a;
                break;
            }
            _ => {}
        }
    }
    assert_eq!(slept, 3, "exactly the three allowed continuations resumed");
    assert!(stopped && saw_cap_warning, "the cap stop is announced");
    assert!(aborted, "the turn ends aborted, not as a normal completion");
    assert!(!errored, "the cap stop is a stop, not an error");
}
