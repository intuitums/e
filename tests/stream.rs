//! Wire-level test: a canned SSE server proves the completions client parses
//! deltas, reasoning, usage, and [DONE] correctly — no network, no keys.

use std::io::{Read, Write};
use std::net::TcpListener;

use e::core::provider::catalog::{Api, Model};
use e::core::provider::{stream, ChatMessage, Event, Request, SseSplitter};

#[test]
fn sse_splitter_handles_fragmentation_and_crlf() {
    let mut s = SseSplitter::new();
    assert!(s.feed("data: {\"a\":").is_empty());
    assert_eq!(s.feed("1}\n\n"), vec!["{\"a\":1}"]);
    assert_eq!(s.feed("data: x\r\n\r\ndata: y\n\n"), vec!["x", "y"]);
    // multi-line data field joins with newline
    assert_eq!(s.feed("data: a\ndata: b\n\n"), vec!["a\nb"]);
}

#[test]
fn sse_splitter_preserves_utf8_across_byte_chunks() {
    let event = "data: {\"text\":\"é\"}\n\n";
    let bytes = event.as_bytes();
    let split = bytes.iter().position(|byte| *byte == 0xc3).unwrap() + 1;
    let mut splitter = SseSplitter::new();
    assert!(splitter.feed_bytes(&bytes[..split]).is_empty());
    assert_eq!(
        splitter.feed_bytes(&bytes[split..]),
        vec![r#"{"text":"é"}"#]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn completions_stream_parses_deltas_and_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let home = std::env::temp_dir().join(format!("e-test-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,\
             \"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
            "data: [DONE]\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(response.as_bytes()).unwrap();
    });

    let request = Request {
        model: Model {
            provider: "mock".into(),
            id: "m".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            api: Api::Completions,
            efforts: Vec::new(),
            context_window: 200_000,
        },
        system: "sys".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: None,
        tools: Vec::new(),
    };
    let (mut rx, _handle) = stream(request);

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut usage = None;
    let mut done = false;
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta(d) => text.push_str(&d),
            Event::ReasoningDelta(d) => reasoning.push_str(&d),
            Event::Usage {
                input,
                output,
                cache_read,
            } => usage = Some((input, output, cache_read)),
            Event::Done => {
                done = true;
                break;
            }
            Event::Error(err) => panic!("stream error: {}", err.message),
            Event::ToolCall(_) | Event::ReasoningItem(_) => {}
        }
    }
    assert!(done);
    assert_eq!(text, "Hello");
    assert_eq!(reasoning, "hmm");
    assert_eq!(usage, Some((12, 3, 4)));
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_folds_provider_events_into_one_session_stream() {
    use e::core::agent::{Agent, SessionEvent};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-test-agent-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(response.as_bytes()).unwrap();
    });

    let model = Model {
        provider: "mock".into(),
        id: "m".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Completions,
        efforts: Vec::new(),
        context_window: 200_000,
    };
    let (mut agent, mut rx) = Agent::new(model);
    agent.submit("hi".into(), "sys".into());

    // The contract: TurnStart first, TurnEnd last, exactly once each.
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

#[tokio::test(flavor = "multi_thread")]
async fn agent_reports_errors_and_still_ends_the_turn_exactly_once() {
    use e::core::agent::{Agent, SessionEvent};

    // A 401 is an auth-class failure: never retried, so one request, one
    // deterministic ending.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-test-agent-err-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let response =
            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        sock.write_all(response.as_bytes()).unwrap();
    });

    let model = Model {
        provider: "mock".into(),
        id: "m".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Completions,
        efforts: Vec::new(),
        context_window: 200_000,
    };
    let (mut agent, mut rx) = Agent::new(model);
    agent.submit("hi".into(), "sys".into());

    // The contract under failure: the error surfaces, the turn still ends —
    // exactly once, not aborted — so the frontend can close it out visibly.
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

#[tokio::test(flavor = "multi_thread")]
async fn agent_interrupt_ends_a_stalled_stream() {
    use e::core::agent::{Agent, SessionEvent};
    use std::time::Duration;

    // Headers arrive, then the body never does — the pre-fix hang: Esc set
    // the cancel flag but the turn only checked it after the next SSE event.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-test-agent-stall-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let _ = sock.write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
        );
        // Keep the socket open so the client stays parked on the body.
        std::thread::sleep(Duration::from_secs(30));
    });

    let model = Model {
        provider: "mock".into(),
        id: "m".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Completions,
        efforts: Vec::new(),
        context_window: 200_000,
    };
    let (mut agent, mut rx) = Agent::new(model);
    agent.submit("hi".into(), "sys".into());

    // Wait until the turn has started, then interrupt while the body is idle.
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

#[tokio::test(flavor = "multi_thread")]
async fn unexpected_eof_is_an_error_not_a_silent_done() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-test-eof-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        // A partial stream that closes without [DONE].
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(response.as_bytes()).unwrap();
    });

    let request = Request {
        model: Model {
            provider: "mock".into(),
            id: "m".into(),
            base_url: format!("http://127.0.0.1:{port}"),
            api: Api::Completions,
            efforts: Vec::new(),
            context_window: 200_000,
        },
        system: "sys".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: None,
        tools: Vec::new(),
    };
    let (mut rx, _handle) = stream(request);

    let mut saw_text = false;
    let mut terminal = None;
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta(_) => saw_text = true,
            Event::Error(err) => {
                terminal = Some(err.message);
                break;
            }
            Event::Done => panic!("unexpected EOF must not finish as Done"),
            _ => {}
        }
    }
    assert!(saw_text);
    let message = terminal.expect("error terminal missing");
    assert!(
        message.contains("unexpected"),
        "unexpected EOF wording missing: {message}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unanswered_request_fails_instead_of_hanging() {
    use e::core::provider::{http, send_request_within, FailureCause};
    use std::time::Duration;

    // The gap between connect (client-bounded) and body reads (chunk-bounded):
    // the server accepts the request and never sends response headers.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        std::thread::sleep(Duration::from_secs(30));
    });

    let builder = http()
        .post(format!("http://127.0.0.1:{port}/v1/messages"))
        .body("{}");
    let err = send_request_within(builder, Duration::from_millis(300))
        .await
        .expect_err("headers never arrive — the send must time out");
    assert!(
        err.message.contains("no response"),
        "stall wording missing: {}",
        err.message
    );
    // Written but unanswered: may have been delivered, so it's a calculated
    // risk to retry, not a certainty — Stalled, same as an idle body.
    assert_eq!(err.cause, FailureCause::Stalled);
    assert!(err.cause.is_retryable());
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_interrupt_ends_a_turn_stuck_before_headers() {
    use e::core::agent::{Agent, SessionEvent};
    use std::time::Duration;

    // Same shape as the stalled-body pin, one await point earlier: the
    // provider task is parked inside send(), not the body stream. Esc must
    // end the turn just the same.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-test-agent-headers-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        std::thread::sleep(Duration::from_secs(30));
    });

    let model = Model {
        provider: "mock".into(),
        id: "m".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Completions,
        efforts: Vec::new(),
        context_window: 200_000,
    };
    let (mut agent, mut rx) = Agent::new(model);
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

#[tokio::test(flavor = "multi_thread")]
async fn retry_recovers_after_a_transient_failure() {
    use e::core::agent::{Agent, SessionEvent};
    use e::core::provider::FailureCause;

    // First attempt hits a 503; the retry succeeds. Exercises the whole
    // pipeline: status classification -> retry decision -> backoff -> a
    // second request -> Recovered once real content arrives.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-test-agent-retry-ok-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let response =
            "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        sock.write_all(response.as_bytes()).unwrap();
        drop(sock);

        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: [DONE]\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(response.as_bytes()).unwrap();
    });

    let model = Model {
        provider: "mock".into(),
        id: "m".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Completions,
        efforts: Vec::new(),
        context_window: 200_000,
    };
    let (mut agent, mut rx) = Agent::new(model);
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
                assert_eq!(attempt, 1);
                assert_eq!(limit, e::core::agent::retry::MAX_ATTEMPTS);
                assert_eq!(cause, FailureCause::ProviderUnavailable);
            }
            SessionEvent::Recovered { attempt, limit } => {
                saw_recovered = true;
                assert_eq!(attempt, 1);
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

#[tokio::test(flavor = "multi_thread")]
async fn retry_campaign_gives_up_after_max_attempts() {
    use e::core::agent::retry::MAX_ATTEMPTS;
    use e::core::agent::{Agent, SessionEvent};

    // Every attempt gets a 503 with Retry-After: 0 — near-instant, so the
    // whole ten-attempt campaign runs in well under a second — proving it
    // actually stops at the budget instead of retrying forever.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let home = std::env::temp_dir().join(format!("e-test-agent-retry-exhaust-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    std::thread::spawn(move || {
        // One initial request plus MAX_ATTEMPTS retries, all 503.
        for _ in 0..(MAX_ATTEMPTS + 1) {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 8192];
            let _ = sock.read(&mut buffer);
            let response = "HTTP/1.1 503 Service Unavailable\r\nretry-after: 0\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
            sock.write_all(response.as_bytes()).unwrap();
        }
    });

    let model = Model {
        provider: "mock".into(),
        id: "m".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Completions,
        efforts: Vec::new(),
        context_window: 200_000,
    };
    let (mut agent, mut rx) = Agent::new(model);
    agent.submit("hi".into(), "sys".into());

    let mut retries = Vec::new();
    let mut error_message = None;
    let aborted = tokio::time::timeout(std::time::Duration::from_secs(10), async {
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

    assert_eq!(retries, (1..=MAX_ATTEMPTS).collect::<Vec<_>>());
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

#[tokio::test(flavor = "multi_thread")]
async fn a_blank_successful_stream_surfaces_an_error_not_silence() {
    use e::core::agent::{Agent, SessionEvent};

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    let home = std::env::temp_dir().join(format!("e-test-blank-{port}"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), r#"{"mock":{"key":"k"}}"#).unwrap();
    std::env::set_var("E_HOME", &home);

    // A well-formed stream that carries nothing: usage only, then [DONE].
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 8192];
        let _ = sock.read(&mut buffer);
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{}}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":0}}\n\n",
            "data: [DONE]\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        sock.write_all(response.as_bytes()).unwrap();
    });

    let model = Model {
        provider: "mock".into(),
        id: "m".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api: Api::Completions,
        efforts: Vec::new(),
        context_window: 200_000,
    };
    let (mut agent, mut rx) = Agent::new(model);
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
