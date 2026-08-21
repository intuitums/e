//! The provider seam: one request contract, one normalized event stream.
//!
//! Everything above this module sees `Event`s; everything below is a wire
//! dialect. Two dialects ship: the chat-completions family and the
//! Responses-API family (see `completions.rs` / `responses.rs`).
//! SSE framing is handled here — one small splitter, tested, shared.

pub mod anthropic;
pub mod catalog;
pub mod completions;
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

#[derive(Clone, Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String, // "user" | "assistant" | "tool" | "system"
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        ChatMessage {
            role: "user".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn assistant(content: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        ChatMessage {
            role: "assistant".into(),
            content: content.into(),
            tool_calls,
            tool_call_id: None,
        }
    }
    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        ChatMessage {
            role: "tool".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// Why a request failed — the retry decision hangs off this, never off
/// matching the message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Credentials are missing or were rejected. Retrying cannot help; the
    /// user must sign in.
    Auth,
    /// The request never left: connection or setup failure. Safe to retry.
    Transient,
    /// The provider saw the request and failed (HTTP status, broken stream,
    /// an error frame). Retrying could double-bill or double-run tools.
    Delivered,
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
    /// `kind` says whether the failure is retryable — see ErrorKind.
    Error {
        message: String,
        kind: ErrorKind,
    },
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
            Err((message, kind)) => {
                let _ = tx.send(Event::Error { message, kind }).await;
            }
        }
    });
    (rx, handle)
}

/// Incremental SSE splitter: feed raw bytes, get complete `data:` payloads.
/// Handles CRLF, multi-line data fields, and the `[DONE]` sentinel (returned
/// as a payload; dialects decide what it means).
pub struct SseSplitter {
    buffer: String,
}

impl Default for SseSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl SseSplitter {
    pub fn new() -> Self {
        SseSplitter {
            buffer: String::new(),
        }
    }

    pub fn feed(&mut self, chunk: &str) -> Vec<String> {
        self.buffer.push_str(chunk);
        let mut events = Vec::new();
        // Events are separated by a blank line.
        while let Some(pos) = find_event_end(&self.buffer) {
            let (raw, rest_at) = (
                self.buffer[..pos].to_string(),
                skip_separator(&self.buffer, pos),
            );
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

fn find_event_end(buf: &str) -> Option<usize> {
    let lf = buf.find("\n\n");
    let crlf = buf.find("\r\n\r\n");
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn skip_separator(buf: &str, pos: usize) -> usize {
    if buf[pos..].starts_with("\r\n\r\n") {
        pos + 4
    } else {
        pos + 2
    }
}

/// The shared client: one pool for every request, so connections (and their
/// TLS handshakes) are reused across turns and tool steps.
pub fn http() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(format!("e/{}", crate::VERSION))
            .build()
            .expect("http client")
    })
}
