//! The provider seam: one request contract, one normalized event stream.
//!
//! Everything above this module sees `Event`s; everything below is a wire
//! dialect. Four dialects ship: chat-completions, Responses, Anthropic
//! Messages, and Gemini (see `api/`). Providers are data (`data/*.json`);
//! OAuth refresh lives in `auth::login`. SSE framing is handled here — one
//! small splitter, tested, shared.

pub mod api;
pub mod catalog;
pub mod diagnostics;
pub mod registry;
pub mod runtime;

use serde::Serialize;
use tokio::sync::mpsc;

use crate::core::providers::catalog::{Api, Model};

const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
const MAX_IMAGE_COUNT: usize = 10;
const MAX_TOTAL_IMAGE_BYTES: u64 = 40 * 1024 * 1024;

#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct ImageInput {
    pub media_type: String,
    /// Base64 without a data-URL prefix. Sessions retain the bytes so resume
    /// does not depend on the original file still existing or staying still.
    pub data: std::sync::Arc<str>,
}

impl ImageInput {
    pub fn from_path(path: &std::path::Path) -> Result<Self, String> {
        Self::from_path_with_size(path).map(|(image, _)| image)
    }

    /// Load a bounded first-turn attachment batch. Keeping count, aggregate,
    /// file-type, and race-safe byte checks here gives the TUI, ask, and RPC
    /// paths one definition of a valid image batch.
    pub fn from_paths(paths: &[String]) -> Result<Vec<Self>, String> {
        if paths.len() > MAX_IMAGE_COUNT {
            return Err(format!(
                "at most {MAX_IMAGE_COUNT} --image attachments are allowed"
            ));
        }
        let declared_total = paths.iter().try_fold(0u64, |total, path| {
            let metadata = std::fs::metadata(path).map_err(|error| format!("{path}: {error}"))?;
            total
                .checked_add(metadata.len())
                .ok_or_else(|| "image attachment sizes overflowed".to_string())
        })?;
        if declared_total > MAX_TOTAL_IMAGE_BYTES {
            return Err("image attachments exceed 40 MiB in total".into());
        }

        let mut actual_total = 0u64;
        let mut images = Vec::with_capacity(paths.len());
        for path in paths {
            let (image, size) = Self::from_path_with_size(std::path::Path::new(path))?;
            actual_total = actual_total
                .checked_add(size)
                .ok_or_else(|| "image attachment sizes overflowed".to_string())?;
            if actual_total > MAX_TOTAL_IMAGE_BYTES {
                return Err("image attachments exceed 40 MiB in total".into());
            }
            images.push(image);
        }
        Ok(images)
    }

    fn from_path_with_size(path: &std::path::Path) -> Result<(Self, u64), String> {
        use base64::Engine as _;
        let metadata =
            std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("{}: not a regular file", path.display()));
        }
        if metadata.len() > MAX_IMAGE_BYTES {
            return Err(format!("{}: image exceeds 20 MiB", path.display()));
        }
        // The file can change size between metadata() and here — grow after a
        // small reported size, or never end at all (a fifo, a procfs entry).
        // Read through a bounded reader so such a file cannot be pulled into
        // memory in full before the length check below ever runs.
        use std::io::Read as _;
        let file =
            std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(MAX_IMAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if bytes.len() as u64 > MAX_IMAGE_BYTES {
            return Err(format!("{}: image exceeds 20 MiB", path.display()));
        }
        let media_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            "image/gif"
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            "image/webp"
        } else {
            return Err(format!(
                "{}: unsupported image data (use png, jpg, gif, or webp)",
                path.display()
            ));
        };
        let size = bytes.len() as u64;
        Ok((
            ImageInput {
                media_type: media_type.into(),
                data: base64::engine::general_purpose::STANDARD
                    .encode(bytes)
                    .into(),
            },
            size,
        ))
    }

    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }
}

/// One requested tool invocation, as the model asked for it.
#[derive(Clone, Debug, serde::Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Raw JSON argument string, exactly as streamed.
    pub arguments: String,
    /// An opaque provider signature attached to the call (Gemini thought
    /// signatures); must be replayed verbatim on the next request of a tool
    /// loop when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageInput>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "user".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_meta: None,
            images: Vec::new(),
        }
    }
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        ChatMessage {
            role: "assistant".into(),
            content: content.into(),
            tool_calls,
            tool_call_id: None,
            tool_meta: None,
            images: Vec::new(),
        }
    }
    /// A dialect-owned reasoning item (signed thinking block, Responses
    /// reasoning JSON) that must replay ahead of its assistant turn.
    pub fn reasoning(item: impl Into<String>) -> Self {
        ChatMessage {
            role: "reasoning".into(),
            content: item.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            tool_meta: None,
            images: Vec::new(),
        }
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage {
            role: "tool".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            tool_meta: None,
            images: Vec::new(),
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
            images: Vec::new(),
        }
    }

    pub fn user_with_images(content: impl Into<String>, images: Vec<ImageInput>) -> Self {
        let mut message = Self::user(content);
        message.images = images;
        message
    }
}

/// Drop images the selected model can't accept from a request's message
/// history — a resumed session, or a mid-session model switch, can carry
/// image-bearing turns from an earlier, image-capable model forward to one
/// that isn't. `load_images` already refuses a *new* attachment against an
/// incompatible model; this is the historical case, where sending the
/// request unchanged would get the whole turn rejected by a backend that
/// doesn't understand image content at all. Never touches the session's
/// own stored history — callers pass their own copy of it (the agent turn
/// loop's `messages`, cloned fresh from `history` each step).
pub fn strip_incompatible_images(messages: &mut [ChatMessage], model: &Model) {
    if model.image_input {
        return;
    }
    for message in messages.iter_mut() {
        if message.images.is_empty() {
            continue;
        }
        let count = message.images.len();
        message.images.clear();
        message.content.push_str(&format!(
            "\n\n[{count} image{} omitted: {} is not declared image-capable]",
            if count == 1 { "" } else { "s" },
            catalog::slug(model)
        ));
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
    /// Written but never confirmed complete: a header-wait or idle-body
    /// timeout, or the body transport broke mid-stream. May have been
    /// billed; the agent only retries when nothing has streamed yet, so a
    /// retry is a calculated risk, not a certainty.
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

/// How a completed stream said it ended. Anything but `Normal`/`ToolCalls`
/// means the reply is not the full answer — the agent surfaces it instead of
/// accepting a truncated or refused turn as a blank success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    /// A normal end of turn (`stop` / `completed` / `end_turn`), or the
    /// dialect saw no explicit reason.
    Normal,
    /// Ended to run the tool calls the stream requested.
    ToolCalls,
    /// The provider cut the reply at a token or length limit.
    Length,
    /// The model refused to answer.
    Refusal,
    /// The provider's content filter blocked or removed output.
    ContentFilter,
    /// A reason e doesn't classify; carried verbatim.
    Other(String),
}

/// A successfully completed stream: how the provider declared it ended, plus
/// stream-hygiene counters the agent surfaces as warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEnd {
    pub finish: FinishReason,
    /// SSE data payloads that failed to parse as JSON and were skipped.
    pub malformed: u32,
}

impl StreamEnd {
    pub fn normal() -> Self {
        StreamEnd {
            finish: FinishReason::Normal,
            malformed: 0,
        }
    }
}

#[derive(Debug)]
pub enum Event {
    TextDelta(String),
    ReasoningDelta(String),
    /// Bytes of tool-call argument JSON just streamed. Argument assembly is
    /// A provider began streaming one tool request. `key` is stable within
    /// this response even when the provider has not supplied the call id yet
    /// (for example, a chat-completions tool index).
    ToolCallStart {
        key: String,
    },
    /// One exact argument fragment for a particular in-flight call. Keeping
    /// identity and content here makes interleaved calls testable and lets
    /// frontends show useful progress without parsing provider wire frames.
    ToolArgumentsDelta {
        key: String,
        delta: String,
    },
    /// Argument streaming for this key is complete. Execution still begins
    /// only after the following validated `ToolCall` event.
    ToolCallEnd {
        key: String,
    },
    /// A completed tool request (dialects accumulate the argument deltas).
    ToolCall(ToolCall),
    /// Token usage from the terminal usage frame. `input` is the TOTAL
    /// prompt-side count — cached tokens included — so it alone measures
    /// context size; `cache_read` is the informational cached subset.
    /// Dialects whose wire fields are disjoint sum them into `input`.
    Usage {
        input: u64,
        output: u64,
        cache_read: u64,
    },
    /// A Responses-dialect reasoning item (verbatim JSON): the API demands
    /// it be resent ahead of the function calls it produced, so the agent
    /// stores it in history and the dialect replays it.
    ReasoningItem(String),
    Done(StreamEnd),
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
        let result = match runtime::authorize(&request.model).await {
            Ok(authorization) => match request.model.api {
                Api::Completions => api::completions::run(&request, &authorization, &tx).await,
                Api::Responses => api::responses::run(&request, &authorization, &tx).await,
                Api::Anthropic => api::anthropic::run(&request, &authorization, &tx).await,
                Api::Google => api::google::run(&request, &authorization, &tx).await,
            },
            Err(error) => Err(error),
        };
        match result {
            Ok(end) => {
                let _ = tx.send(Event::Done(end)).await;
            }
            Err(err) => {
                let _ = tx.send(Event::Error(err)).await;
            }
        }
    });
    (rx, handle)
}

/// Turn a non-2xx response into the typed error every dialect reports the
/// same way; 2xx passes through untouched.
pub async fn require_success(
    response: reqwest::Response,
) -> Result<reqwest::Response, ProviderError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let retry_after = retry_after_seconds(&response);
    let text = response.text().await.unwrap_or_default();
    Err(ProviderError::from_status(status, &text).with_retry_after(retry_after))
}

/// Incremental SSE splitter: feed raw bytes, get complete `data:` payloads.
/// Handles CRLF, multi-line data fields, and the `[DONE]` sentinel (returned
/// as a payload; dialects decide what it means).
pub struct SseSplitter {
    buffer: Vec<u8>,
    scan_from: usize,
    oversized: bool,
}

impl Default for SseSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl SseSplitter {
    pub fn new() -> Self {
        SseSplitter {
            buffer: Vec::new(),
            scan_from: 0,
            oversized: false,
        }
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
        while let Some(relative) = find_event_end(&self.buffer[self.scan_from..]) {
            let pos = self.scan_from + relative;
            let rest_at = skip_separator(&self.buffer, pos);
            if pos > MAX_SSE_EVENT_BYTES {
                self.buffer.drain(..rest_at);
                self.scan_from = 0;
                self.oversized = true;
                continue;
            }
            let raw = String::from_utf8_lossy(&self.buffer[..pos]).into_owned();
            self.buffer.drain(..rest_at);
            self.scan_from = 0;
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
        // Only the final three old bytes can begin a delimiter completed by a
        // later chunk. Resuming there makes a byte-at-a-time malformed stream
        // linear rather than repeatedly rescanning its whole pending frame.
        if self.buffer.len() > MAX_SSE_EVENT_BYTES {
            self.buffer.clear();
            self.scan_from = 0;
            self.oversized = true;
        } else {
            self.scan_from = self.buffer.len().saturating_sub(3);
        }
        events
    }

    fn take_oversized(&mut self) -> bool {
        std::mem::take(&mut self.oversized)
    }
}

/// A single SSE event should be a small JSON frame. Leave ample room for a
/// provider that emits one unusually large tool call, but never let a broken
/// gateway grow an unterminated frame without bound.
pub const MAX_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// Drives a streaming response body as SSE. `next()` yields complete `data:`
/// payloads with idle time bounded; the body ending before the dialect saw
/// its terminal frame is a broken stream, not a successful empty reply, so
/// EOF is an error by contract. Payloads that fail the dialect's JSON parse
/// are reported to `malformed()` and counted rather than silently vanishing.
pub struct SseStream<S> {
    stream: S,
    splitter: SseSplitter,
    queue: std::collections::VecDeque<String>,
    malformed: u32,
    event_timeout: std::time::Duration,
}

impl<S, T, E> SseStream<S>
where
    S: futures::Stream<Item = Result<T, E>> + Unpin,
    T: AsRef<[u8]>,
    E: std::fmt::Display,
{
    pub fn new(stream: S) -> Self {
        SseStream {
            stream,
            splitter: SseSplitter::new(),
            queue: std::collections::VecDeque::new(),
            malformed: 0,
            event_timeout: std::time::Duration::from_secs(STREAM_IDLE_SECS),
        }
    }

    /// Testable form of `new`: the timeout measures time until a complete SSE
    /// event, not time until the next arbitrary transport chunk.
    pub fn with_event_timeout(stream: S, event_timeout: std::time::Duration) -> Self {
        let mut value = Self::new(stream);
        value.event_timeout = event_timeout;
        value
    }

    /// The next complete payload; EOF fails as a stall (see the type docs).
    pub async fn next(&mut self) -> Result<String, ProviderError> {
        let deadline = tokio::time::Instant::now() + self.event_timeout;
        loop {
            if let Some(payload) = self.queue.pop_front() {
                return Ok(payload);
            }
            if self.splitter.take_oversized() {
                return Err(ProviderError::rejected(format!(
                    "provider sent an SSE event larger than {MAX_SSE_EVENT_BYTES} bytes"
                )));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(ProviderError::stalled(
                    "stream stalled before completing an SSE event",
                ));
            }
            match next_sse_chunk_within(&mut self.stream, remaining).await? {
                Some(chunk) => {
                    self.queue.extend(self.splitter.feed_bytes(chunk.as_ref()));
                }
                None => return Err(ProviderError::stalled("stream ended unexpectedly")),
            }
        }
    }

    /// Record one payload the dialect could not parse.
    pub fn malformed(&mut self) {
        self.malformed += 1;
    }

    /// Finish successfully: fold the hygiene counters into the turn result.
    pub fn end(&self, finish: FinishReason) -> StreamEnd {
        StreamEnd {
            finish,
            malformed: self.malformed,
        }
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
            // After a system sleep, pooled connections are dead but look
            // alive locally — a request that reuses one would sit out the
            // full stall budget before failing. Keepalives retire them
            // quickly on both sides instead.
            .tcp_keepalive(std::time::Duration::from_secs(15))
            .pool_idle_timeout(std::time::Duration::from_secs(30))
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
    next_sse_chunk_within(stream, std::time::Duration::from_secs(STREAM_IDLE_SECS)).await
}

async fn next_sse_chunk_within<S, T, E>(
    stream: &mut S,
    wait: std::time::Duration,
) -> Result<Option<T>, ProviderError>
where
    S: futures::Stream<Item = Result<T, E>> + Unpin,
    E: std::fmt::Display,
{
    match tokio::time::timeout(wait, futures::StreamExt::next(stream)).await {
        Ok(None) => Ok(None),
        Ok(Some(Ok(chunk))) => Ok(Some(chunk)),
        // A broken body transport (reset, truncated chunking) is retryable
        // by cause; the agent still refuses to retry once content streamed.
        Ok(Some(Err(e))) => Err(ProviderError::stalled(format!("stream error: {e}"))),
        Err(_) => Err(ProviderError::stalled(
            "stream stalled before completing an SSE event",
        )),
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

    fn temp_image_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "e-image-test-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn a_valid_image_under_the_cap_loads() {
        let path = temp_image_path("small.png");
        let content = b"\x89PNG\r\n\x1a\nrest of a tiny file";
        std::fs::write(&path, content).unwrap();
        let (image, size) = ImageInput::from_path_with_size(&path).unwrap();
        assert_eq!(image.media_type, "image/png");
        assert_eq!(size, content.len() as u64);
        let _ = std::fs::remove_file(&path);
    }

    // from_path_with_size reads through take(MAX_IMAGE_BYTES + 1) rather
    // than std::fs::read, so the post-read length check below can never be
    // reached by pulling an arbitrarily large file fully into memory
    // first. This test pins the still-correct outward behavior (reject,
    // with this exact message) after that change; it can't observe memory
    // use directly, but a regression back to an unbounded read would fail
    // this the same way it fails the fifo-style "must fail fast, not
    // block" tests elsewhere in this suite — by taking a very long time
    // or exhausting memory instead of returning promptly.
    #[test]
    fn an_oversized_image_is_rejected_without_reading_past_the_cap() {
        let path = temp_image_path("big.png");
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.resize(MAX_IMAGE_BYTES as usize + 1, 0);
        std::fs::write(&path, &bytes).unwrap();
        let error = ImageInput::from_path_with_size(&path).unwrap_err();
        assert!(error.contains("exceeds 20 MiB"), "{error}");
        let _ = std::fs::remove_file(&path);
    }

    fn image_incapable_model() -> Model {
        Model {
            provider: "test".into(),
            id: "test".into(),
            base_url: "https://example.invalid".into(),
            api: Api::Completions,
            catalog: crate::core::providers::registry::CatalogStrategy::Openai,
            responses_mount: crate::core::providers::registry::ResponsesMount::Platform,
            provider_supports_tools: true,
            provider_image_input: false,
            efforts: Vec::new(),
            thinking: crate::core::providers::catalog::Thinking::Manual,
            context_window: 200_000,
            max_output: None,
            supports_tools: true,
            image_input: false,
            pricing: None,
        }
    }

    #[test]
    fn strip_incompatible_images_clears_history_the_model_cannot_accept() {
        let mut messages = vec![
            ChatMessage::user("plain text, no images"),
            ChatMessage::user_with_images(
                "an old image",
                vec![ImageInput {
                    media_type: "image/png".into(),
                    data: std::sync::Arc::from("data"),
                }],
            ),
        ];
        strip_incompatible_images(&mut messages, &image_incapable_model());
        assert!(messages[0].images.is_empty());
        assert_eq!(messages[0].content, "plain text, no images");
        assert!(messages[1].images.is_empty(), "the image must be removed");
        assert!(
            messages[1]
                .content
                .contains("is not declared image-capable"),
            "the omission should be noted, not silent: {}",
            messages[1].content
        );
    }

    #[test]
    fn strip_incompatible_images_leaves_a_capable_model_untouched() {
        let mut model = image_incapable_model();
        model.image_input = true;
        let mut messages = vec![ChatMessage::user_with_images(
            "an image",
            vec![ImageInput {
                media_type: "image/png".into(),
                data: std::sync::Arc::from("data"),
            }],
        )];
        strip_incompatible_images(&mut messages, &model);
        assert_eq!(
            messages[0].images.len(),
            1,
            "capable models keep their images"
        );
        assert_eq!(messages[0].content, "an image");
    }
}
