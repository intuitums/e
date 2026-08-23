//! The provider seam: one request contract, one normalized event stream.
//!
//! Everything above this module sees `Event`s; everything below is a wire
//! dialect. Three dialects ship: chat-completions, Responses-API, and
//! Anthropic Messages (see `completions.rs` / `responses.rs` / `anthropic.rs`).
//! Providers are data (`providers/*.json`); OAuth refresh lives in `auth::login`.
//! SSE framing is handled here — one small splitter, tested, shared.

pub mod anthropic;
pub mod catalog;
pub mod completions;
pub mod registry;
pub mod responses;

use serde::Serialize;
use tokio::sync::mpsc;

use crate::core::provider::catalog::{Api, Model};

/// One requested tool invocation, as the model asked for it.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON argument string, exactly as streamed.
    pub arguments: String,
}

/// Presentation metadata persisted beside a tool result, ignored by provider
/// dialects and used to reconstruct the transcript on resume.
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct ToolResultMeta {
    pub outcome: crate::core::tools::ToolOutcome,
    pub summary: String,
}

#[derive(Clone, Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant" | "tool" | "system"
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_meta: Option<ToolResultMeta>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "user".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_meta: None,
        }
    }
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        ChatMessage {
            role: "assistant".into(),
            content: content.into(),
            tool_calls,
            tool_call_id: None,
            tool_meta: None,
        }
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage {
            role: "tool".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            tool_meta: None,
        }
    }

    pub fn tool_result_with_meta(
        call_id: impl Into<String>,
        content: impl Into<String>,
        outcome: crate::core::tools::ToolOutcome,
        summary: impl Into<String>,
    ) -> Self {
        ChatMessage {
            role: "tool".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            tool_meta: Some(ToolResultMeta {
                outcome,
                summary: summary.into(),
            }),
        }
    }
}

/// Why a provider request failed, and what that implies for retrying it. The
/// retry decision hangs off this alone, never off matching message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
    /// Credentials are missing or were rejected locally, or the provider
    /// answered 401/403. Retrying cannot help; the user must sign in.
    Auth,
    /// Connection/DNS/TLS/setup failure — the request never left. Safe to
    /// retry.
    Network,
    /// Written but never confirmed received (a header-wait or idle-body
    /// timeout), with nothing produced yet. May have been billed, but
    /// nothing else can have been double-run — a retry is a calculated risk,
    /// not a certainty.
    Stalled,
    /// HTTP 429, or a provider error frame naming a rate limit. Retry,
    /// honoring `Retry-After` when the provider sent one.
    RateLimited,
    /// HTTP 408/500/502/503/504, or a provider error frame naming an outage
    /// — the provider is unwell right now, not that the request was bad.
    /// Retry.
    ProviderUnavailable,
    /// A rejected request (other 4xx), a provider error frame naming
    /// something else, or a stream that broke after content already
    /// arrived. Retrying would either fail identically or risk
    /// double-running something.
    Rejected,
}

impl FailureCause {
    /// Whether this cause alone permits a retry. Callers must still confirm
    /// nothing has streamed yet for the current attempt before acting on it.
    pub fn is_retryable(self) -> bool {
        matches!(
            self,
            FailureCause::Network
                | FailureCause::Stalled
                | FailureCause::RateLimited
                | FailureCause::ProviderUnavailable
        )
    }

    /// Classify an HTTP status the provider actually returned.
    fn from_status(status: reqwest::StatusCode) -> FailureCause {
        match status.as_u16() {
            401 | 403 => FailureCause::Auth,
            429 => FailureCause::RateLimited,
            408 | 500 | 502 | 503 | 504 => FailureCause::ProviderUnavailable,
            _ => FailureCause::Rejected,
        }
    }

    /// A short human name for the retry row and the exhausted-campaign error.
    pub fn label(self) -> &'static str {
        match self {
            FailureCause::Auth => "Authentication",
            FailureCause::Network => "Network interrupted",
            FailureCause::Stalled => "No response from provider",
            FailureCause::RateLimited => "Rate limited",
            FailureCause::ProviderUnavailable => "Provider unavailable",
            FailureCause::Rejected => "Request failed",
        }
    }
}

/// One provider-call failure: enough to drive the retry decision and to show
/// two different messages a caller needs — a short reason for the live retry
/// row, and the full detail for a terminal failure.
#[derive(Debug, Clone)]
pub struct ProviderError {
    /// Full detail; a terminal error block shows this.
    pub message: String,
    /// Squeezed to a line the activity row can hold, e.g. "504 Gateway
    /// Timeout". Equal to `message` when there is nothing shorter to say.
    pub short: String,
    pub cause: FailureCause,
    /// Seconds the provider asked us to wait (`Retry-After`), if it sent one.
    pub retry_after: Option<u64>,
}

impl ProviderError {
    pub fn auth(message: impl Into<String>) -> Self {
        let message = message.into();
        ProviderError {
            short: message.clone(),
            message,
            cause: FailureCause::Auth,
            retry_after: None,
        }
    }
    pub fn network(message: impl Into<String>) -> Self {
        let message = message.into();
        ProviderError {
            short: message.clone(),
            message,
            cause: FailureCause::Network,
            retry_after: None,
        }
    }
    pub fn stalled(message: impl Into<String>) -> Self {
        let message = message.into();
        ProviderError {
            short: message.clone(),
            message,
            cause: FailureCause::Stalled,
            retry_after: None,
        }
    }
    pub fn rejected(message: impl Into<String>) -> Self {
        let message = message.into();
        ProviderError {
            short: message.clone(),
            message,
            cause: FailureCause::Rejected,
            retry_after: None,
        }
    }
    /// A provider error frame delivered mid-stream, already classified by
    /// the dialect that parsed it (e.g. Anthropic's `overloaded_error`).
    pub fn frame(message: impl Into<String>, cause: FailureCause) -> Self {
        let message = message.into();
        ProviderError {
            short: message.clone(),
            message,
            cause,
            retry_after: None,
        }
    }
    /// Classify an HTTP status the provider actually returned; `body` is the
    /// response text the caller already read.
    pub fn from_status(status: reqwest::StatusCode, body: &str) -> Self {
        let cause = FailureCause::from_status(status);
        let short = format!(
            "{} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("error")
        );
        let snippet: String = body.chars().take(300).collect();
        let message = if snippet.is_empty() {
            short.clone()
        } else {
            format!("{short}: {snippet}")
        };
        ProviderError {
            message,
            short,
            cause,
            retry_after: None,
        }
    }
    pub fn with_retry_after(mut self, seconds: Option<u64>) -> Self {
        self.retry_after = seconds;
        self
    }
}

#[derive(Debug)]
pub enum Event {
    TextDelta(String),
    ReasoningDelta(String),
    /// A completed tool request (dialects accumulate the argument deltas).
    ToolCall(ToolCall),
    /// input, output, cache_read tokens from the terminal usage frame.
    Usage {
        input: u64,
        output: u64,
        cache_read: u64,
    },
    /// A Responses-dialect reasoning item (verbatim JSON): the API demands
    /// it be resent ahead of the function calls it produced, so the agent
    /// stores it in history and the dialect replays it.
    ReasoningItem(String),
    Done,
    /// The provider call failed; `err.cause` decides whether the agent may
    /// retry it — see FailureCause.
    Error(ProviderError),
}

pub struct Request {
    pub model: Model,
    pub system: String,
    pub messages: Vec<ChatMessage>,
    pub effort: Option<String>,
    /// Tool schemas to advertise (dialect-shaped by each implementation).
    pub tools: Vec<serde_json::Value>,
}

/// Start the request; events arrive on the returned channel. The task ends
/// with `Done` or `Error` — always exactly one terminal event. The handle
/// aborts the request (esc).
pub fn stream(request: Request) -> (mpsc::Receiver<Event>, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(64);
    let handle = tokio::spawn(async move {
        let result = match request.model.api {
            Api::Completions => crate::core::provider::completions::run(&request, &tx).await,
            Api::Responses => crate::core::provider::responses::run(&request, &tx).await,
            Api::Anthropic => crate::core::provider::anthropic::run(&request, &tx).await,
        };
        match result {
            Ok(()) => {
                let _ = tx.send(Event::Done).await;
            }
            Err(err) => {
                let _ = tx.send(Event::Error(err)).await;
            }
        }
    });
    (rx, handle)
}

/// Incremental SSE splitter: feed raw bytes, get complete `data:` payloads.
/// Handles CRLF, multi-line data fields, and the `[DONE]` sentinel (returned
/// as a payload; dialects decide what it means).
pub struct SseSplitter {
    buffer: Vec<u8>,
}

impl Default for SseSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl SseSplitter {
    pub fn new() -> Self {
        SseSplitter { buffer: Vec::new() }
    }

    /// Convenience entry point for tests and already-decoded sources.
    pub fn feed(&mut self, chunk: &str) -> Vec<String> {
        self.feed_bytes(chunk.as_bytes())
    }

    /// Preserve arbitrary HTTP body chunk boundaries as bytes. Decoding each
    /// chunk separately would replace a UTF-8 code point split across two
    /// chunks before the complete SSE event could be reassembled.
    pub fn feed_bytes(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        // Events are separated by a blank line.
        while let Some(pos) = find_event_end(&self.buffer) {
            let rest_at = skip_separator(&self.buffer, pos);
            let raw = String::from_utf8_lossy(&self.buffer[..pos]).into_owned();
            self.buffer.drain(..rest_at);
            let mut data = String::new();
            for line in raw.lines() {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if let Some(value) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value.strip_prefix(' ').unwrap_or(value));
                }
            }
            if !data.is_empty() {
                events.push(data);
            }
        }
        events
    }
}

fn find_event_end(buf: &[u8]) -> Option<usize> {
    let find = |needle: &[u8]| {
        buf.windows(needle.len())
            .position(|window| window == needle)
    };
    let lf = find(b"\n\n");
    let crlf = find(b"\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn skip_separator(buf: &[u8], pos: usize) -> usize {
    if buf[pos..].starts_with(b"\r\n\r\n") {
        pos + 4
    } else {
        pos + 2
    }
}

/// The shared client: one pool for every request, so connections (and their
/// TLS handshakes) are reused across turns and tool steps. Connect is bounded;
/// the overall request is not — a live SSE stream can run for minutes. The
/// wait for response headers is bounded in `send_request`, idle bodies
/// per-chunk in `next_sse_chunk`.
pub fn http() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(format!("e/{}", crate::VERSION))
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("http client")
    })
}

/// Seconds of provider silence — awaiting response headers or the next body
/// chunk — before the request is declared stalled.
pub const STREAM_IDLE_SECS: u64 = 180;

/// Send the request, bounding the wait for response headers. The client
/// bounds connect and `next_sse_chunk` bounds body reads; this closes the gap
/// between them — an accepted request the provider never answers — with the
/// same budget: silence is a stall wherever it happens.
pub async fn send_request(
    builder: reqwest::RequestBuilder,
) -> Result<reqwest::Response, ProviderError> {
    send_request_within(builder, std::time::Duration::from_secs(STREAM_IDLE_SECS)).await
}

/// `send_request` with an explicit bound (tests shrink it to milliseconds).
pub async fn send_request_within(
    builder: reqwest::RequestBuilder,
    wait: std::time::Duration,
) -> Result<reqwest::Response, ProviderError> {
    match tokio::time::timeout(wait, builder.send()).await {
        Ok(Ok(response)) => Ok(response),
        // Connection and setup failures: the request never left, retryable.
        Ok(Err(e)) => Err(ProviderError::network(format!("request failed: {e}"))),
        // The request was written but never answered — it may have been
        // delivered, so a retry is a calculated risk, not a certainty.
        Err(_) => Err(ProviderError::stalled(format!(
            "no response from provider for {}s",
            wait.as_secs()
        ))),
    }
}

/// Await the next SSE body chunk. An idle socket fails instead of hanging the
/// turn forever — the agent then ends with a visible error rather than a
/// spinner that Esc cannot clear.
pub async fn next_sse_chunk<S, T, E>(stream: &mut S) -> Result<Option<T>, ProviderError>
where
    S: futures::Stream<Item = Result<T, E>> + Unpin,
    E: std::fmt::Display,
{
    match tokio::time::timeout(
        std::time::Duration::from_secs(STREAM_IDLE_SECS),
        futures::StreamExt::next(stream),
    )
    .await
    {
        Ok(None) => Ok(None),
        Ok(Some(Ok(chunk))) => Ok(Some(chunk)),
        Ok(Some(Err(e))) => Err(ProviderError::rejected(format!("stream error: {e}"))),
        Err(_) => Err(ProviderError::stalled(format!(
            "stream stalled — no data for {STREAM_IDLE_SECS}s"
        ))),
    }
}

/// Parse `Retry-After` as whole seconds. Every provider we talk to sends the
/// numeric form; the HTTP-date form doesn't appear in practice here.
pub fn retry_after_seconds(response: &reqwest::Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_classification_matches_retry_policy() {
        use reqwest::StatusCode;
        let cause = |code: u16| FailureCause::from_status(StatusCode::from_u16(code).unwrap());
        assert_eq!(cause(401), FailureCause::Auth);
        assert_eq!(cause(403), FailureCause::Auth);
        assert_eq!(cause(429), FailureCause::RateLimited);
        assert_eq!(cause(408), FailureCause::ProviderUnavailable);
        assert_eq!(cause(500), FailureCause::ProviderUnavailable);
        assert_eq!(cause(502), FailureCause::ProviderUnavailable);
        assert_eq!(cause(503), FailureCause::ProviderUnavailable);
        assert_eq!(cause(504), FailureCause::ProviderUnavailable);
        assert_eq!(cause(400), FailureCause::Rejected);
        assert_eq!(cause(404), FailureCause::Rejected);
        assert_eq!(cause(422), FailureCause::Rejected);

        assert!(FailureCause::RateLimited.is_retryable());
        assert!(FailureCause::ProviderUnavailable.is_retryable());
        assert!(FailureCause::Network.is_retryable());
        assert!(FailureCause::Stalled.is_retryable());
        assert!(!FailureCause::Auth.is_retryable());
        assert!(!FailureCause::Rejected.is_retryable());
    }

    #[test]
    fn from_status_squeezes_a_long_body_to_a_short_reason() {
        let body = "x".repeat(2000);
        let err = ProviderError::from_status(reqwest::StatusCode::GATEWAY_TIMEOUT, &body);
        assert_eq!(err.short, "504 Gateway Timeout");
        assert!(err.message.starts_with("504 Gateway Timeout: xxx"));
        assert!(err.message.len() < body.len());
    }
}
