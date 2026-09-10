//! Backend failure records keep diagnostic facts outside model context.
mod common;

use common::{env_lock, serve_raw, test_model, Home};
use e::core::agent::{failure::RetryDecision, Agent, AgentOptions, SessionEvent};
use e::core::providers::{catalog::Api, FailureStage};

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn interrupted_body_records_context_and_retains_partial_work() {
    let _lock = env_lock();
    let home = Home::new("error-details");
    home.auth(r#"{"mock":{"key":"synthetic-key"}}"#);
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"partial reply\"}}]}\n\n";
    let (port, server) = serve_raw(vec![format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nx-request-id: diagnostic-123\r\ncontent-length: 500\r\nconnection: close\r\n\r\n{body}"
    )]);
    let (mut agent, mut rx) = Agent::new(test_model("mock", port, Api::Completions));
    agent.submit("test prompt".into(), "test system".into());
    let mut details = None;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::ErrorDetails(report) => details = Some(report),
            SessionEvent::Error(_) => assert!(details.is_some(), "details must precede the error"),
            SessionEvent::Retry { .. } => panic!("partial output must not be replayed"),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    assert_eq!(server.join().unwrap().len(), 1);
    let report = details.unwrap();
    assert_eq!(report.summary, "Provider response interrupted.");
    assert!(report
        .detail
        .contains("end of file before message length reached"));
    assert!(matches!(report.stage, FailureStage::Stream));
    assert_eq!(report.response.http_status, Some(200));
    assert_eq!(
        report.response.request_id.as_deref(),
        Some("diagnostic-123")
    );
    assert_eq!(report.retry_decision, RetryDecision::PartialOutput);
    assert_eq!(report.attempt, 1);
    assert_eq!(report.partial_text_bytes, "partial reply".len());
    assert_eq!(report.provider, "mock");
    assert_eq!(report.model, "test");
    let path = agent.session_path().unwrap();
    let records = std::fs::read_to_string(path.with_extension("errors.jsonl")).unwrap();
    let error: serde_json::Value = records
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .next()
        .unwrap();
    assert!(error["parent"].is_string());
    assert_eq!(error["details"]["response"]["request_id"], "diagnostic-123");
    let restored = e::core::session::SessionLog::load(&path).unwrap();
    assert!(restored
        .iter()
        .any(|message| message.content == "partial reply"));
    assert!(!restored
        .iter()
        .any(|message| message.content.contains("diagnostic-123")));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn no_save_failures_still_emit_redacted_backend_details() {
    let _lock = env_lock();
    let home = Home::new("error-no-save");
    home.auth(r#"{"mock":{"key":"synthetic-secret-key"}}"#);
    let body = "rejected synthetic-secret-key";
    let (port, server) = serve_raw(vec![format!(
        "HTTP/1.1 401 Unauthorized\r\nx-request-id: auth-123\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()
    )]);
    let (mut agent, mut rx) = Agent::with_options(
        test_model("mock", port, Api::Completions),
        AgentOptions {
            save_session: false,
            ..AgentOptions::default()
        },
    );
    agent.submit("test".into(), "test".into());
    let mut details = None;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::ErrorDetails(report) => details = Some(report),
            SessionEvent::Error(message) => assert!(!message.contains("synthetic-secret-key")),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    server.join().unwrap();
    let report = details.unwrap();
    assert_eq!(report.retry_decision, RetryDecision::NotRetryable);
    assert_eq!(report.response.http_status, Some(401));
    assert_eq!(report.response.request_id.as_deref(), Some("auth-123"));
    assert_eq!(report.detail, "rejected [redacted]");
    assert!(agent.session_path().is_none());
}

/// Both shared-classifier callers retain wire codes and response correlation IDs.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn shared_stream_error_classifier_keeps_diagnostics_in_both_dialects() {
    use e::core::providers::{self, ChatMessage, Event, FailureCause, Request};
    let _lock = env_lock();
    let home = Home::new("shared-error-diagnostics");
    home.auth(r#"{"mock":{"key":"synthetic-key"}}"#);
    for api in [Api::Completions, Api::Google] {
        let body = "data: {\"error\":{\"code\":\"server_error\",\"message\":\"Provider disconnected unexpectedly\"}}\n\n";
        let (port, server) = serve_raw(vec![format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nx-request-id: shared-123\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()
        )]);
        let (mut events, task) = providers::stream(Request {
            model: test_model("mock", port, api),
            system: "test".into(),
            messages: vec![ChatMessage::user("test")],
            effort: None,
            session_id: String::new(),
            tools: Vec::new(),
        });
        let mut failure = None;
        while let Some(event) = events.recv().await {
            match event {
                Event::Error(error) => failure = Some(error),
                Event::Done(_) => panic!("error frame reported success"),
                _ => {}
            }
        }
        task.await.unwrap();
        server.join().unwrap();
        let failure = failure.expect("missing provider error");
        assert_eq!(failure.cause, FailureCause::ProviderUnavailable);
        assert_eq!(failure.provider_code.as_deref(), Some("server_error"));
        assert_eq!(failure.response.request_id.as_deref(), Some("shared-123"));
        assert_eq!(failure.response.http_status, Some(200));
    }
}

/// Partial tool assembly counts as produced output even before a call is complete.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn partial_tool_arguments_prevent_retry_and_record_that_decision() {
    let _lock = env_lock();
    let home = Home::new("error-partial-arguments");
    home.auth(r#"{"mock":{"key":"synthetic-key"}}"#);
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"unrun\",\"function\":{\"name\":\"read\",\"arguments\":\"{\"}}]}}]}\n\n",
        "data: {\"error\":{\"code\":503,\"message\":\"unavailable\"}}\n\n"
    );
    let (port, server) = common::serve_sse(&[body]);
    let (mut agent, mut rx) = Agent::with_options(
        test_model("mock", port, Api::Completions),
        AgentOptions {
            save_session: false,
            ..AgentOptions::default()
        },
    );
    agent.submit("test".into(), "test".into());
    let mut details = None;
    while let Some(event) = rx.recv().await {
        match event {
            SessionEvent::ErrorDetails(report) => details = Some(report),
            SessionEvent::Retry { .. } => panic!("tool arguments must prevent retry"),
            SessionEvent::TurnEnd { .. } => break,
            _ => {}
        }
    }
    server.join().unwrap();
    let details = details.unwrap();
    assert_eq!(details.partial_tool_argument_bytes, 1);
    assert_eq!(details.retry_decision, RetryDecision::PartialOutput);
    assert_eq!(details.attempt, 1);
}

/// Blocking summary lookup retains the caller's scoped home rather than E_HOME.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn summary_override_loads_in_the_scoped_home() {
    let _lock = env_lock();
    let home = Home::new("summary-scope");
    home.write("settings.json", r#"{"error_stalled":"Wrong home."}"#);
    std::fs::create_dir(home.dir.join("scoped")).unwrap();
    home.write(
        "scoped/settings.json",
        r#"{"error_stalled":"Custom interruption."}"#,
    );
    e::core::config::home::scope(home.dir.join("scoped"), async {
        let error = e::core::providers::ProviderError::stalled("test");
        assert_eq!(
            e::core::agent::failure::ErrorDetails::summary(&error).await,
            "Custom interruption."
        );
    })
    .await;
}
