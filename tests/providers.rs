//! The provider seam: one harness, every dialect, the catalog, the registry.
//!
//! Adding a dialect is a case in the table, not a new file. The mock server,
//! `E_HOME` lock, and request collector live in `common/` so the pins stay
//! about wire shape and event grammar — not fixture boilerplate.

mod common;

use common::{clear_env_keys, env_lock, read_tool, request_json, serve_sse, test_model, Home};

use e::core::providers::catalog::{self, Api, Model, Thinking};
use e::core::providers::{
    self, ChatMessage, Event, FailureCause, FinishReason, ImageInput, Request, SseSplitter,
    SseStream, ToolCall, MAX_SSE_EVENT_BYTES,
};

// ---------------------------------------------------------------------------
// Shared framing
// ---------------------------------------------------------------------------

#[test]
fn sse_splitter_handles_fragmentation_and_crlf() {
    let mut s = SseSplitter::new();
    assert!(s.feed("data: {\"a\":").is_empty());
    assert_eq!(s.feed("1}\n\n"), vec!["{\"a\":1}"]);
    assert_eq!(s.feed("data: x\r\n\r\ndata: y\n\n"), vec!["x", "y"]);
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

#[tokio::test]
async fn unterminated_sse_event_is_bounded() {
    let chunk = vec![b'x'; MAX_SSE_EVENT_BYTES + 1];
    let stream = futures::stream::iter(vec![Ok::<_, std::io::Error>(chunk)]);
    let mut sse = SseStream::new(stream);
    let err = sse.next().await.unwrap_err();
    assert_eq!(err.cause, FailureCause::Rejected);
    assert!(err.message.contains("SSE event larger"));
}

#[tokio::test]
async fn a_completed_oversized_sse_event_is_rejected_too() {
    let mut chunk = b"data: ".to_vec();
    chunk.extend(std::iter::repeat_n(b'x', MAX_SSE_EVENT_BYTES + 1));
    chunk.extend_from_slice(b"\n\n");
    let stream = futures::stream::iter(vec![Ok::<_, std::io::Error>(chunk)]);
    let mut sse = SseStream::new(stream);
    let err = sse.next().await.unwrap_err();
    assert_eq!(err.cause, FailureCause::Rejected);
}

#[tokio::test]
async fn periodic_bytes_do_not_reset_the_complete_event_deadline() {
    let stream = futures::stream::unfold(0usize, |n| async move {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        Some((Ok::<_, std::io::Error>(vec![b'x']), n + 1))
    });
    let mut sse =
        SseStream::with_event_timeout(Box::pin(stream), std::time::Duration::from_millis(25));
    let err = sse.next().await.unwrap_err();
    assert_eq!(err.cause, FailureCause::Stalled);
}

// ---------------------------------------------------------------------------
// Dialects — one table, one pin per wire contract
// ---------------------------------------------------------------------------

enum History {
    UserOnly,
    /// Anthropic reshapes tool results on the way out.
    AnthropicToolLoop,
    /// Gemini replays thought signatures on the prior function call.
    GoogleToolLoop,
}

enum ToolExpect {
    Exact {
        id: &'static str,
        args: &'static str,
    },
    /// Gemini synthesizes ids (`read-N`) and attaches thought signatures.
    Synthesized {
        name: &'static str,
        id_prefix: &'static str,
        args_json: &'static str,
        signature: Option<&'static str>,
    },
}

struct DialectCase {
    name: &'static str,
    provider: &'static str,
    auth: &'static str,
    api: Api,
    thinking: Thinking,
    effort: Option<&'static str>,
    history: History,
    sse: &'static str,
    text: &'static str,
    reasoning: &'static str,
    usage: Option<(u64, u64, u64)>,
    tool: ToolExpect,
    finish: Option<FinishReason>,
}

fn dialects() -> Vec<DialectCase> {
    vec![
        DialectCase {
            name: "completions",
            provider: "mock",
            auth: r#"{"mock":{"key":"k"}}"#,
            api: Api::Completions,
            thinking: Thinking::Manual,
            effort: None,
            history: History::UserOnly,
            sse: concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"hmm\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",",
                "\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}]}}]}\n\n",
                "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3,",
                "\"prompt_tokens_details\":{\"cached_tokens\":4}}}\n\n",
                "data: [DONE]\n\n",
            ),
            text: "Hello",
            reasoning: "hmm",
            usage: Some((12, 3, 4)),
            tool: ToolExpect::Exact {
                id: "c1",
                args: "{\"path\":\"a.txt\"}",
            },
            finish: None,
        },
        DialectCase {
            name: "anthropic",
            provider: "anthropic",
            auth: r#"{"anthropic":{"key":"sk-ant-test"}}"#,
            api: Api::Anthropic,
            thinking: Thinking::Manual,
            effort: Some("high"),
            history: History::AnthropicToolLoop,
            sse: concat!(
                "event: message_start\n",
                "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":40}}}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello \"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"world\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
                "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.txt\\\"}\"}}\n\n",
                "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
                "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":25}}\n\n",
                "data: {\"type\":\"message_stop\"}\n\n",
            ),
            text: "hello world",
            reasoning: "hmm",
            // Anthropic's prompt fields are disjoint: input is the inclusive
            // total (100 uncached + 40 cache-read), cache_read the subset.
            usage: Some((140, 25, 40)),
            tool: ToolExpect::Exact {
                id: "tu_1",
                args: "{\"path\":\"a.txt\"}",
            },
            finish: None,
        },
        DialectCase {
            name: "responses",
            provider: "openai",
            auth: r#"{"openai":{"key":"sk-test"}}"#,
            api: Api::Responses,
            thinking: Thinking::Manual,
            effort: Some("medium"),
            history: History::UserOnly,
            sse: concat!(
                "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n",
                "data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"hmm\"}\n\n",
                "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"c1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":0}}}}\n\n",
            ),
            text: "hi",
            reasoning: "hmm",
            usage: Some((10, 2, 0)),
            tool: ToolExpect::Exact {
                id: "c1",
                args: "{\"path\":\"a.txt\"}",
            },
            finish: None,
        },
        DialectCase {
            name: "google",
            provider: "google",
            auth: r#"{"google":{"key":"g-test"}}"#,
            api: Api::Google,
            thinking: Thinking::Manual,
            effort: Some("high"),
            history: History::GoogleToolLoop,
            sse: concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"planning\",\"thought\":true}]}}]}\n\n",
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello \"}]}}]}\n\n",
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"world\"},{\"functionCall\":{\"id\":\"g-call-1\",\"name\":\"read\",\"args\":{\"path\":\"a.txt\"}},\"thoughtSignature\":\"sig-1\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":90,\"candidatesTokenCount\":20,\"thoughtsTokenCount\":5,\"cachedContentTokenCount\":30}}\n\n",
            ),
            text: "hello world",
            reasoning: "planning",
            // Thought tokens count as output.
            usage: Some((90, 25, 30)),
            tool: ToolExpect::Synthesized {
                name: "read",
                id_prefix: "g-call-1",
                args_json: r#"{"path":"a.txt"}"#,
                signature: Some("sig-1"),
            },
            finish: Some(FinishReason::Normal),
        },
    ]
}

fn assert_wire(name: &str, sent: &str) {
    let body = request_json(sent);
    match name {
        "completions" => {
            assert!(
                body["tools"][0].get("function").is_some(),
                "completions must keep the nested function shape: {body}"
            );
            assert_eq!(body["tools"][0]["function"]["name"], "read");
        }
        "anthropic" => {
            assert!(sent.contains("x-api-key: sk-ant-test"));
            assert!(sent.contains("anthropic-version: 2023-06-01"));
            assert!(
                body["tools"][0]["input_schema"].is_object(),
                "tool schema not converted"
            );
            assert!(
                sent.contains("\"tool_use_id\":\"tu_0\""),
                "tool result not in anthropic shape"
            );
            assert!(
                sent.contains("\"budget_tokens\":24000"),
                "high effort budget missing"
            );
            assert!(body["max_tokens"].is_number());
            // Prompt caching: the system block anchors the prefix and the
            // last message carries the moving breakpoint — without it every
            // step of a tool loop re-bills the whole history uncached.
            assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
            let last = body["messages"].as_array().unwrap().last().unwrap();
            let blocks = last["content"].as_array().unwrap();
            assert_eq!(
                blocks.last().unwrap()["cache_control"]["type"],
                "ephemeral",
                "moving cache breakpoint missing on the last message"
            );
        }
        "responses" => {
            // The regression that 400'd the first live turn: tools[0].name
            // at top level, not nested under function.
            assert_eq!(body["tools"][0]["type"], "function");
            assert_eq!(body["tools"][0]["name"], "read");
            assert!(
                body["tools"][0].get("function").is_none(),
                "nested shape leaked"
            );
            assert!(body["tools"][0]["parameters"].is_object());
            assert!(body["parallel_tool_calls"].as_bool().unwrap());
            let request_line = sent.lines().next().unwrap();
            assert!(
                request_line.starts_with("POST /responses "),
                "wrong mount: {request_line}"
            );
            assert!(!sent.contains("chatgpt-account-id"));
            assert!(
                !sent.contains("prompt_cache_key"),
                "the codex-only cache key must not ride a plain-key request"
            );
            assert!(
                sent.contains("authorization: Bearer sk-test")
                    || sent.contains("Authorization: Bearer sk-test")
            );
        }
        "google" => {
            assert!(sent.contains("POST /models/test:streamGenerateContent?alt=sse"));
            assert!(sent.contains("x-goog-api-key: g-test"));
            assert_eq!(body["systemInstruction"]["parts"][0]["text"], "sys");
            assert_eq!(
                body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
                "high"
            );
            assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "read");
            let contents = body["contents"].as_array().unwrap();
            assert_eq!(contents[1]["role"], "model");
            assert_eq!(contents[1]["parts"][0]["functionCall"]["name"], "read");
            assert_eq!(contents[1]["parts"][0]["functionCall"]["id"], "call_abc123");
            assert_eq!(contents[1]["parts"][0]["thoughtSignature"], "sig-0");
            assert_eq!(contents[2]["parts"][0]["functionResponse"]["name"], "read");
            assert_eq!(
                contents[2]["parts"][0]["functionResponse"]["id"],
                "call_abc123"
            );
        }
        other => panic!("unknown dialect {other}"),
    }
}

fn history_messages(kind: &History) -> Vec<ChatMessage> {
    match kind {
        History::UserOnly => vec![ChatMessage::user("hello")],
        History::AnthropicToolLoop => vec![
            ChatMessage::user("read a.txt"),
            ChatMessage::assistant(
                "",
                vec![ToolCall {
                    id: "tu_0".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"old.txt\"}".into(),
                    signature: None,
                }],
            ),
            ChatMessage::tool_result("tu_0", "old contents"),
        ],
        History::GoogleToolLoop => vec![
            ChatMessage::user("read a.txt"),
            // A foreign-dialect id (an OpenAI-style call id from a
            // mid-session model switch): the functionResponse name must
            // come from the call it refers to, never from the id's spelling.
            ChatMessage::assistant(
                "",
                vec![ToolCall {
                    id: "call_abc123".into(),
                    name: "read".into(),
                    arguments: "{\"path\":\"old.txt\"}".into(),
                    signature: Some("sig-0".into()),
                }],
            ),
            ChatMessage::tool_result("call_abc123", "old contents"),
        ],
    }
}

fn assert_tool(name: &str, calls: &[ToolCall], expect: &ToolExpect) {
    match expect {
        ToolExpect::Exact { id, args } => {
            assert_eq!(calls.len(), 1, "{name}");
            assert_eq!(calls[0].id, *id, "{name}");
            assert_eq!(calls[0].arguments, *args, "{name}");
        }
        ToolExpect::Synthesized {
            name: tool_name,
            id_prefix,
            args_json,
            signature,
        } => {
            assert_eq!(calls.len(), 1, "{name}");
            assert_eq!(calls[0].name, *tool_name, "{name}");
            assert!(
                calls[0].id.starts_with(id_prefix),
                "{name}: id {}",
                calls[0].id
            );
            let got: serde_json::Value = serde_json::from_str(&calls[0].arguments).unwrap();
            let want: serde_json::Value = serde_json::from_str(args_json).unwrap();
            assert_eq!(got, want, "{name}");
            assert_eq!(calls[0].signature.as_deref(), *signature, "{name}");
        }
    }
}

async fn collect_stream(
    request: Request,
) -> (
    String,
    String,
    Vec<ToolCall>,
    Option<(u64, u64, u64)>,
    Option<FinishReason>,
) {
    let (mut rx, _handle) = providers::stream(request);
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = Vec::new();
    let mut usage = None;
    let mut finish = None;
    let mut starts = std::collections::BTreeSet::new();
    let mut ends = std::collections::BTreeSet::new();
    let mut deltas: std::collections::BTreeMap<String, String> = Default::default();
    let mut end_order = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta(d) => text.push_str(&d),
            Event::ReasoningDelta(d) => reasoning.push_str(&d),
            Event::ToolCall(c) => calls.push(c),
            Event::Usage {
                input,
                output,
                cache_read,
            } => usage = Some((input, output, cache_read)),
            Event::Error(err) => panic!("stream errored: {}", err.message),
            Event::Done(end) => {
                finish = Some(end.finish);
                break;
            }
            Event::ReasoningItem(_) => {}
            Event::ToolCallStart { key } => {
                assert!(starts.insert(key), "duplicate tool-call start");
            }
            Event::ToolArgumentsDelta { key, delta } => {
                assert!(starts.contains(&key), "arguments arrived before start");
                assert!(!ends.contains(&key), "arguments arrived after end");
                deltas.entry(key).or_default().push_str(&delta);
            }
            Event::ToolCallEnd { key } => {
                assert!(starts.contains(&key), "tool-call end arrived before start");
                assert!(ends.insert(key.clone()), "duplicate tool-call end");
                end_order.push(key);
            }
        }
    }
    assert_eq!(starts, ends, "every semantic tool-call start must end");
    assert_eq!(
        starts.len(),
        calls.len(),
        "one lifecycle per completed call"
    );
    for (key, call) in end_order.iter().zip(&calls) {
        assert_eq!(
            deltas.get(key).map(String::as_str).unwrap_or_default(),
            call.arguments,
            "semantic deltas must reconstruct completed arguments for {key}"
        );
    }
    (text, reasoning, calls, usage, finish)
}

async fn collect_error(request: Request) -> e::core::providers::ProviderError {
    let (mut rx, _handle) = providers::stream(request);
    while let Some(event) = rx.recv().await {
        match event {
            Event::Error(error) => return error,
            Event::Done(_) => panic!("stream completed instead of failing"),
            _ => {}
        }
    }
    panic!("stream ended without an error")
}

/// Recognized streaming rate-limit codes can still carry a hard quota wall in
/// their message; the more specific, non-retryable cause must win.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn streaming_rate_limit_codes_respect_quota_messages() {
    let _lock = env_lock();
    let anthropic = concat!(
        "data: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",",
        "\"message\":\"monthly usage limit reached\"}}\n\n",
    );
    let responses = concat!(
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{",
        "\"code\":\"rate_limit_exceeded\",\"message\":\"insufficient_quota\"}}}\n\n",
    );
    let (anthropic_port, _anthropic_server) = serve_sse(&[anthropic]);
    let (responses_port, _responses_server) = serve_sse(&[responses]);
    let home = Home::new("streaming-quota");
    home.auth(r#"{"anthropic":{"key":"k"},"openai":{"key":"k"}}"#);

    for (provider, port, api) in [
        ("anthropic", anthropic_port, Api::Anthropic),
        ("openai", responses_port, Api::Responses),
    ] {
        let error = collect_error(Request {
            model: test_model(provider, port, api),
            system: "sys".into(),
            messages: vec![ChatMessage::user("hi")],
            effort: None,
            tools: Vec::new(),
        })
        .await;
        assert_eq!(error.cause, FailureCause::QuotaExhausted, "{provider}");
        assert!(!error.cause.is_retryable(), "{provider}");
    }
}

/// Chat Completions may interleave fragments for parallel calls. Progress
/// must retain a stable per-call key; one anonymous byte counter cannot prove
/// that fragments were attributed or assembled correctly.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn semantic_tool_progress_preserves_interleaved_call_identity() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-a\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call-b\",\"function\":{\"name\":\"grep\",\"arguments\":\"{\\\"qu\"}}]}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"a\\\"}\"}},{\"index\":1,\"function\":{\"arguments\":\"ery\\\":\\\"b\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let (port, server) = serve_sse(&[body]);
    let home = Home::new("semantic-tool-progress");
    home.auth(r#"{"openai":{"key":"test"}}"#);
    let request = Request {
        model: test_model("openai", port, Api::Completions),
        system: "sys".into(),
        messages: vec![ChatMessage::user("use two tools")],
        effort: None,
        tools: vec![read_tool()],
    };
    let (mut rx, _handle) = providers::stream(request);
    let mut starts = Vec::new();
    let mut deltas: std::collections::BTreeMap<String, String> = Default::default();
    let mut ends = Vec::new();
    let mut calls = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            Event::ToolCallStart { key } => starts.push(key),
            Event::ToolArgumentsDelta { key, delta } => {
                deltas.entry(key).or_default().push_str(&delta)
            }
            Event::ToolCallEnd { key } => ends.push(key),
            Event::ToolCall(call) => calls.push(call),
            Event::Error(error) => panic!("stream errored: {}", error.message),
            Event::Done(_) => break,
            _ => {}
        }
    }
    assert_eq!(starts, vec!["0", "1"]);
    assert_eq!(ends, vec!["0", "1"]);
    assert_eq!(deltas["0"], r#"{"path":"a"}"#);
    assert_eq!(deltas["1"], r#"{"query":"b"}"#);
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].arguments, deltas["0"]);
    assert_eq!(calls[1].arguments, deltas["1"]);
    server.join().unwrap();
}

// The env lock is held across awaits: E_HOME must stay ours and each
// #[tokio::test] runs on its own runtime.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn each_dialect_streams_text_tools_and_usage() {
    let _lock = env_lock();
    for case in dialects() {
        let (port, server) = serve_sse(&[case.sse]);
        let home = Home::new(case.name);
        home.auth(case.auth);

        let mut model = test_model(case.provider, port, case.api);
        model.thinking = case.thinking;
        let request = Request {
            model,
            system: "sys".into(),
            messages: history_messages(&case.history),
            effort: case.effort.map(str::to_string),
            tools: vec![read_tool()],
        };

        let (text, reasoning, calls, usage, finish) = collect_stream(request).await;
        assert_eq!(text, case.text, "{}", case.name);
        assert_eq!(reasoning, case.reasoning, "{}", case.name);
        assert_tool(case.name, &calls, &case.tool);
        assert_eq!(usage, case.usage, "{}", case.name);
        if let Some(want) = case.finish {
            assert_eq!(finish, Some(want), "{}", case.name);
        }

        let sent = server.join().unwrap();
        assert_eq!(sent.len(), 1, "{}", case.name);
        assert_wire(case.name, &sent[0]);
    }
}

// A nameless tool_use block (name: "") must not open a tool-call lifecycle:
// Anthropic names the block up front, so the start can be refused there
// instead of leaving a dangling ToolCallStart with no matching ToolCallEnd.
// The consumer relies on start/end balance (see collect_stream's asserts),
// and a nameless call is unrunnable anyway.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn anthropic_nameless_tool_use_opens_no_lifecycle() {
    let _lock = env_lock();
    let sse = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":0}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_bad\",\"name\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"x\\\":1}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_good\",\"name\":\"read\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.txt\\\"}\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (port, server) = serve_sse(&[sse]);
    let home = Home::new("anthropic-nameless");
    home.auth(r#"{"anthropic":{"key":"sk-ant-test"}}"#);
    let request = Request {
        model: test_model("anthropic", port, Api::Anthropic),
        system: "sys".into(),
        messages: history_messages(&History::AnthropicToolLoop),
        effort: Some("high".into()),
        tools: vec![read_tool()],
    };
    let (_text, _reasoning, calls, _usage, _finish) = collect_stream(request).await;
    // collect_stream already panics if a start lacks an end; this pins that
    // the nameless block emitted nothing while the named one survived.
    assert_eq!(calls.len(), 1, "only the named tool_use should be emitted");
    assert_eq!(calls[0].id, "tu_good");
    server.join().unwrap();
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn each_dialect_translates_the_same_image_message() {
    let _lock = env_lock();
    for case in dialects() {
        let (port, server) = serve_sse(&[case.sse]);
        let home = Home::new(&format!("image-{}", case.name));
        home.auth(case.auth);
        let request = Request {
            model: test_model(case.provider, port, case.api),
            system: "sys".into(),
            messages: vec![ChatMessage::user_with_images(
                "describe",
                vec![ImageInput {
                    media_type: "image/png".into(),
                    data: "AA==".into(),
                }],
            )],
            effort: case.effort.map(str::to_string),
            tools: vec![read_tool()],
        };
        let _ = collect_stream(request).await;
        let sent = server.join().unwrap();
        let body = request_json(&sent[0]);
        match case.api {
            Api::Completions => {
                assert_eq!(body["messages"][1]["content"][1]["type"], "image_url");
                assert_eq!(
                    body["messages"][1]["content"][1]["image_url"]["url"],
                    "data:image/png;base64,AA=="
                );
            }
            Api::Responses => {
                assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
                assert_eq!(
                    body["input"][0]["content"][1]["image_url"],
                    "data:image/png;base64,AA=="
                );
            }
            Api::Anthropic => {
                assert_eq!(body["messages"][0]["content"][1]["type"], "image");
                assert_eq!(
                    body["messages"][0]["content"][1]["source"]["media_type"],
                    "image/png"
                );
                assert_eq!(body["messages"][0]["content"][1]["source"]["data"], "AA==");
            }
            Api::Google => {
                assert_eq!(
                    body["contents"][0]["parts"][1]["inlineData"]["mimeType"],
                    "image/png"
                );
                assert_eq!(
                    body["contents"][0]["parts"][1]["inlineData"]["data"],
                    "AA=="
                );
            }
        }
    }
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn completions_retries_without_rejected_stream_options() {
    let _lock = env_lock();
    let rejected = r#"{"error":{"message":"unknown field stream_options"}}"#;
    let first = format!(
        "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{rejected}",
        rejected.len()
    );
    let second = common::sse_response("data: [DONE]\n\n");
    let (port, server) = common::serve_raw(vec![first, second]);
    let home = Home::new("strict-completions");
    home.auth(r#"{"mock":{"key":"k"}}"#);
    let request = Request {
        model: test_model("mock", port, Api::Completions),
        system: "sys".into(),
        messages: vec![ChatMessage::user("hello")],
        effort: None,
        tools: Vec::new(),
    };

    collect_stream(request).await;
    let sent = server.join().unwrap();
    assert_eq!(sent.len(), 2);
    assert!(request_json(&sent[0]).get("stream_options").is_some());
    assert!(request_json(&sent[1]).get("stream_options").is_none());
}

// The plain-key mount must never see `prompt_cache_key` (pinned in
// `assert_wire` above); the codex OAuth mount must always send it — pin
// both ends so a refactor can't silently drop it from the OAuth branch.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn responses_codex_oauth_mount_sends_prompt_cache_key() {
    let _lock = env_lock();
    let case = dialects()
        .into_iter()
        .find(|c| c.name == "responses")
        .unwrap();
    let (port, server) = serve_sse(&[case.sse]);
    let home = Home::new("responses-oauth");
    home.auth(
        r#"{"openai":{"access":"acc-test","refresh":"ref-test","expires":9999999999999,"account_id":"acct-1"}}"#,
    );

    let mut model = test_model(case.provider, port, case.api);
    model.thinking = case.thinking;
    model.responses_mount = e::core::providers::registry::ResponsesMount::Codex;
    let request = Request {
        model,
        system: "sys".into(),
        messages: history_messages(&case.history),
        effort: case.effort.map(str::to_string),
        tools: vec![read_tool()],
    };

    collect_stream(request).await;

    let sent = server.join().unwrap();
    assert_eq!(sent.len(), 1);
    let request_line = sent[0].lines().next().unwrap();
    assert!(
        request_line.starts_with("POST /codex/responses "),
        "wrong mount: {request_line}"
    );
    assert!(sent[0].contains("chatgpt-account-id: acct-1"));
    let body = request_json(&sent[0]);
    assert!(
        body["prompt_cache_key"].is_string(),
        "the codex OAuth mount must send the cache key: {body}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn responses_platform_mount_is_not_inferred_from_oauth_credentials() {
    let _lock = env_lock();
    let case = dialects()
        .into_iter()
        .find(|case| case.name == "responses")
        .unwrap();
    let (port, server) = serve_sse(&[case.sse]);
    let home = Home::new("responses-platform-oauth");
    home.auth(
        r#"{"openai":{"access":"acc-test","refresh":"ref-test","expires":9999999999999,"account_id":"acct-1"}}"#,
    );
    let request = Request {
        model: test_model(case.provider, port, case.api),
        system: "sys".into(),
        messages: history_messages(&case.history),
        effort: case.effort.map(str::to_string),
        tools: vec![read_tool()],
    };

    collect_stream(request).await;
    let sent = server.join().unwrap();
    assert!(sent[0].starts_with("POST /responses "));
    assert!(!sent[0].contains("chatgpt-account-id"));
    assert!(request_json(&sent[0]).get("prompt_cache_key").is_none());
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn adaptive_models_take_effort_through_output_config() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":1}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (port, server) = serve_sse(&[body]);
    let home = Home::new("adaptive");
    home.auth(r#"{"anthropic":{"key":"sk-ant-test"}}"#);

    let mut model = test_model("anthropic", port, Api::Anthropic);
    model.thinking = Thinking::Adaptive;
    let request = Request {
        model,
        system: "be helpful".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: Some("high".into()),
        tools: Vec::new(),
    };
    let (_text, _reasoning, _calls, _usage, _finish) = collect_stream(request).await;

    let sent = server.join().unwrap().remove(0);
    assert!(
        sent.contains("\"thinking\":{\"type\":\"adaptive\"}"),
        "adaptive shape missing"
    );
    assert!(
        sent.contains("\"output_config\":{\"effort\":\"high\"}"),
        "effort not carried through output_config"
    );
    assert!(
        !sent.contains("budget_tokens"),
        "legacy budget_tokens must not reach an adaptive model"
    );
}

/// Signed thinking blocks round-trip a tool loop: the stream's thinking +
/// signature becomes a replayable reasoning item, and the follow-up request
/// carries it verbatim at the head of the assistant turn, before tool_use.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn signed_thinking_blocks_are_captured_and_replayed() {
    let _lock = env_lock();
    let first = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"let me look\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig-abc\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tu_1\",\"name\":\"read\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":5}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let second = concat!(
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"done\"}}\n\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (port, server) = serve_sse(&[first, second]);
    let home = Home::new("signed-think");
    home.auth(r#"{"anthropic":{"key":"sk-ant-test"}}"#);

    let mut model = test_model("anthropic", port, Api::Anthropic);
    model.thinking = Thinking::Adaptive;

    let request = Request {
        model: model.clone(),
        system: "sys".into(),
        messages: vec![ChatMessage::user("read a.txt")],
        effort: Some("high".into()),
        tools: Vec::new(),
    };
    let (mut rx, _handle) = providers::stream(request);
    let mut items = Vec::new();
    let mut calls = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            Event::ReasoningItem(item) => items.push(item),
            Event::ToolCall(c) => calls.push(c),
            Event::Error(err) => panic!("stream errored: {}", err.message),
            Event::Done(_) => break,
            _ => {}
        }
    }
    assert_eq!(items.len(), 1, "one signed thinking block captured");
    let block: serde_json::Value = serde_json::from_str(&items[0]).unwrap();
    assert_eq!(block["type"], "thinking");
    assert_eq!(block["thinking"], "let me look");
    assert_eq!(block["signature"], "sig-abc");
    assert_eq!(calls.len(), 1);

    let request = Request {
        model,
        system: "sys".into(),
        messages: vec![
            ChatMessage::user("read a.txt"),
            ChatMessage {
                role: "reasoning".into(),
                content: items.remove(0),
                tool_calls: Vec::new(),
                tool_call_id: None,
                tool_meta: None,
                images: Vec::new(),
                usage: None,
                internal: false,
            },
            ChatMessage::assistant("", calls.clone()),
            ChatMessage::tool_result("tu_1", "contents"),
        ],
        effort: Some("high".into()),
        tools: Vec::new(),
    };
    let (_text, _reasoning, _calls, _usage, _finish) = collect_stream(request).await;

    let sent = server.join().unwrap();
    assert_eq!(sent.len(), 2);
    let value = request_json(&sent[1]);
    let assistant = &value["messages"][1];
    assert_eq!(assistant["role"], "assistant");
    let content = assistant["content"].as_array().unwrap();
    assert_eq!(
        content[0]["type"], "thinking",
        "thinking must lead the assistant turn"
    );
    assert_eq!(content[0]["signature"], "sig-abc");
    assert_eq!(content[0]["thinking"], "let me look");
    assert_eq!(content[1]["type"], "tool_use");
}

/// Gemini verifies a function call's thoughtSignature against the thought
/// text that preceded it. A multi-step tool loop must replay that signed
/// thought text ahead of the function call it signs, not just the
/// signature — otherwise the follow-up request rejects.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn gemini_replays_signed_thought_text_ahead_of_its_function_call() {
    let _lock = env_lock();
    let first = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"let me look\",\"thought\":true}]}}]}\n\n",
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"id\":\"g-call-1\",\"name\":\"read\",\"args\":{\"path\":\"a.txt\"}},\"thoughtSignature\":\"sig-abc\"}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":10,\"candidatesTokenCount\":5}}\n\n",
    );
    let second =
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"done\"}]},\"finishReason\":\"STOP\"}]}\n\n";
    let (port, server) = serve_sse(&[first, second]);
    let home = Home::new("gemini-signed-thought");
    home.auth(r#"{"google":{"key":"g-test"}}"#);

    let model = test_model("google", port, Api::Google);
    let request = Request {
        model: model.clone(),
        system: "sys".into(),
        messages: vec![ChatMessage::user("read a.txt")],
        effort: None,
        tools: Vec::new(),
    };
    let (mut rx, _handle) = providers::stream(request);
    let mut items = Vec::new();
    let mut calls = Vec::new();
    while let Some(event) = rx.recv().await {
        match event {
            Event::ReasoningItem(item) => items.push(item),
            Event::ToolCall(c) => calls.push(c),
            Event::Error(err) => panic!("stream errored: {}", err.message),
            Event::Done(_) => break,
            _ => {}
        }
    }
    assert_eq!(items.len(), 1, "one signed thought captured");
    let item: serde_json::Value = serde_json::from_str(&items[0]).unwrap();
    assert_eq!(item["type"], "gemini_thought");
    assert_eq!(item["text"], "let me look");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].signature.as_deref(), Some("sig-abc"));

    let request = Request {
        model,
        system: "sys".into(),
        messages: vec![
            ChatMessage::user("read a.txt"),
            ChatMessage::reasoning(items.remove(0)),
            ChatMessage::assistant("", calls.clone()),
            ChatMessage::tool_result("g-call-1", "contents"),
        ],
        effort: None,
        tools: Vec::new(),
    };
    let (_text, _reasoning, _calls, _usage, _finish) = collect_stream(request).await;

    let sent = server.join().unwrap();
    assert_eq!(sent.len(), 2);
    let value = request_json(&sent[1]);
    let model_turn = &value["contents"][1];
    assert_eq!(model_turn["role"], "model");
    let parts = model_turn["parts"].as_array().unwrap();
    assert_eq!(
        parts[0]["text"], "let me look",
        "the signed thought text must lead the turn it signs: {value}"
    );
    assert_eq!(parts[0]["thought"], true);
    assert_eq!(parts[1]["functionCall"]["name"], "read");
    assert_eq!(parts[1]["thoughtSignature"], "sig-abc");
}

/// A small declared context window (a user-defined custom model, or a
/// mistyped override) can shrink `max_tokens` below the room a manual
/// thinking budget needs — the request must degrade by dropping the
/// `thinking` block, not by emitting `budget_tokens >= max_tokens`, which
/// the API rejects outright.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn small_context_window_drops_thinking_instead_of_an_invalid_budget() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":1}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (port, server) = serve_sse(&[body]);
    let home = Home::new("small-window");
    home.auth(r#"{"anthropic":{"key":"sk-ant-test"}}"#);

    let mut model = test_model("anthropic", port, Api::Anthropic);
    model.thinking = Thinking::Manual;
    model.context_window = 2_000; // max_tokens clamps to 1_000, under the 2_048 floor
    let request = Request {
        model,
        system: "be helpful".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: Some("high".into()),
        tools: Vec::new(),
    };
    let (_text, _reasoning, _calls, _usage, _finish) = collect_stream(request).await;

    let sent = server.join().unwrap().remove(0);
    assert!(
        !sent.contains("\"thinking\""),
        "a max_tokens too small for any valid budget must drop thinking entirely: {sent}"
    );
}

/// A model whose real output ceiling is below the Anthropic dialect's 32k
/// default (declared via `max_output`, e.g. claude-haiku-4-5's ~8k) must
/// have `max_tokens` clamped to that ceiling — otherwise the request 400s
/// with a max_tokens-exceeds-limit error before generation.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn small_max_output_clamps_max_tokens_below_the_dialect_default() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":9}}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "data: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":1}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (port, server) = serve_sse(&[body]);
    let home = Home::new("small-max-output");
    home.auth(r#"{"anthropic":{"key":"sk-ant-test"}}"#);

    let mut model = test_model("anthropic", port, Api::Anthropic);
    model.thinking = Thinking::Manual;
    model.context_window = 200_000; // half the window (100k) would otherwise clamp nothing
    model.max_output = Some(8_192);
    let request = Request {
        model,
        system: "be helpful".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: None,
        tools: Vec::new(),
    };
    let (_text, _reasoning, _calls, _usage, _finish) = collect_stream(request).await;

    let sent = server.join().unwrap().remove(0);
    assert_eq!(
        request_json(&sent)["max_tokens"],
        8_192,
        "max_tokens must respect the model's own max_output, not the dialect's 32k default: {sent}"
    );
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn responses_replays_reasoning_items_ahead_of_their_calls() {
    use e::core::agent::{Agent, SessionEvent};

    let _lock = env_lock();
    let first = concat!(
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"id\":\"rs_1\",\"encrypted_content\":\"SECRETBLOB\",\"summary\":[]}}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"f.txt\\\"}\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
    );
    let second = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
    );
    let (port, server) = serve_sse(&[first, second]);
    let home = Home::new("reasoning");
    home.auth(r#"{"openai":{"key":"k"}}"#);

    let ws = home.dir.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("f.txt"), "data\n").unwrap();
    std::env::set_current_dir(ws.canonicalize().unwrap()).unwrap();

    let (mut agent, mut rx) = Agent::new(test_model("openai", port, Api::Responses));
    agent.submit("read f.txt".into(), "sys".into());
    let mut ended = false;
    while let Some(event) = rx.recv().await {
        if let SessionEvent::TurnEnd { .. } = event {
            ended = true;
            break;
        }
    }
    assert!(ended);

    let sent = server.join().unwrap();
    assert_eq!(sent.len(), 2);
    let body = request_json(&sent[1]);
    let input = body["input"].as_array().unwrap();
    let kinds: Vec<&str> = input.iter().filter_map(|i| i["type"].as_str()).collect();
    let reasoning_pos = kinds.iter().position(|k| *k == "reasoning");
    let call_pos = kinds.iter().position(|k| *k == "function_call");
    assert!(
        reasoning_pos.is_some(),
        "reasoning item not replayed: {kinds:?}"
    );
    assert!(
        reasoning_pos.unwrap() < call_pos.expect("function_call replayed"),
        "reasoning must precede its call: {kinds:?}"
    );
    let item = &input[reasoning_pos.unwrap()];
    assert_eq!(item["encrypted_content"], "SECRETBLOB", "item not verbatim");
    assert_eq!(item["id"], "rs_1");
}

/// MAX_TOKENS from Gemini is a truncated reply delivered as HTTP success;
/// the dialect must name it instead of finishing normally.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn google_max_tokens_maps_to_length() {
    let _lock = env_lock();
    let body = concat!(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"cut\"}]}}]}\n\n",
        "data: {\"candidates\":[{\"finishReason\":\"MAX_TOKENS\"}]}\n\n",
    );
    let (port, _server) = serve_sse(&[body]);
    let home = Home::new("google-len");
    home.auth(r#"{"google":{"key":"g-test"}}"#);

    let request = Request {
        model: test_model("google", port, Api::Google),
        system: "sys".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: None,
        tools: Vec::new(),
    };
    let (_text, _reasoning, _calls, _usage, finish) = collect_stream(request).await;
    assert_eq!(finish, Some(FinishReason::Length));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn unexpected_eof_is_an_error_not_a_silent_done() {
    let _lock = env_lock();
    // A partial stream that closes without [DONE].
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
    let (port, _server) = serve_sse(&[body]);
    let home = Home::new("eof");
    home.auth(r#"{"mock":{"key":"k"}}"#);

    let request = Request {
        model: test_model("mock", port, Api::Completions),
        system: "sys".into(),
        messages: vec![ChatMessage::user("hi")],
        effort: None,
        tools: Vec::new(),
    };
    let (mut rx, _handle) = providers::stream(request);
    let mut saw_text = false;
    let mut terminal = None;
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta(_) => saw_text = true,
            Event::Error(err) => {
                terminal = Some(err.message);
                break;
            }
            Event::Done(_) => panic!("unexpected EOF must not finish as Done"),
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
    use e::core::providers::{http, send_request_within, FailureCause};
    use std::time::Duration;

    // The gap between connect (client-bounded) and body reads (chunk-bounded):
    // the server accepts the request and never sends response headers.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::Read;
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
    assert_eq!(err.cause, FailureCause::Stalled);
    assert!(err.cause.is_retryable());
}

// ---------------------------------------------------------------------------
// Catalog + registry — walk the data, pin the seams that have drifted
// ---------------------------------------------------------------------------

fn catalog_with_models_json(json: &str) -> Vec<Model> {
    let home = Home::new("models");
    home.write("models.json", json);
    catalog::catalog()
}

#[test]
fn registry_and_catalog_match_the_embedded_data() {
    let _lock = env_lock();
    clear_env_keys();
    let _home = Home::new("registry");

    let all = e::core::providers::registry::all();
    assert!(all.len() >= 17);
    let catalog = catalog::catalog();

    for provider in all {
        assert!(
            !provider.display.is_empty(),
            "{} has no display name",
            provider.name
        );
        // Remote backends speak TLS; only keyless local backends may not.
        assert!(
            provider.base_url.starts_with("https://")
                || (provider.auth.none && provider.base_url.starts_with("http://localhost")),
            "{}: suspicious base_url {}",
            provider.name,
            provider.base_url
        );
        assert!(
            provider.auth.oauth.is_some() || provider.auth.key || provider.auth.none,
            "{} has no way to sign in",
            provider.name
        );
        if provider.auth.key {
            assert!(
                provider.auth.key_env.is_some(),
                "{} key without env var",
                provider.name
            );
        }
        // Local backends discover their models live; everyone else ships seeds.
        assert!(
            provider.auth.none || !provider.models.is_empty(),
            "{} has no seed models",
            provider.name
        );
        assert_eq!(catalog::display_name(&provider.name), provider.display);

        for decl in &provider.models {
            let model = catalog
                .iter()
                .find(|m| m.provider == provider.name && m.id == decl.id)
                .unwrap_or_else(|| {
                    panic!("{} / {} missing from the catalog", provider.name, decl.id)
                });
            assert_eq!(model.base_url, provider.base_url, "{}", decl.id);
            assert_eq!(model.api, provider.api(), "{}", decl.id);
            assert_eq!(model.context_window, decl.context_window, "{}", decl.id);
            assert_eq!(model.max_output, decl.max_output, "{}", decl.id);
            assert_eq!(model.efforts, decl.efforts, "{}", decl.id);
            let thinking = match decl.thinking.as_deref() {
                Some("adaptive") => Thinking::Adaptive,
                _ => Thinking::Manual,
            };
            assert_eq!(model.thinking, thinking, "{}", decl.id);
        }
    }

    // Panel contents, from data: two account flows, fourteen key providers
    // (the keyless locals appear in neither panel).
    assert_eq!(e::core::providers::registry::oauth_providers().len(), 2);
    assert_eq!(e::core::providers::registry::key_providers().len(), 14);

    let vercel = e::core::providers::registry::find("vercel").expect("vercel is a built-in");
    assert_eq!(vercel.auth.key_env.as_deref(), Some("AI_GATEWAY_API_KEY"));
    assert!(vercel.auth.oauth.is_none(), "gateway is API-key only");

    let google = e::core::providers::registry::find("google").expect("google is a built-in");
    assert_eq!(google.api(), Api::Google);
    assert_eq!(google.auth.key_env.as_deref(), Some("GEMINI_API_KEY"));

    let ollama = e::core::providers::registry::find("ollama").expect("ollama is a built-in");
    assert!(ollama.auth.none && !ollama.auth.key);

    assert!(
        !catalog.iter().any(|m| m.id == "grok-build-0.1"),
        "grok-build-0.1 was culled from the built-ins"
    );
    let sonnet = catalog
        .iter()
        .find(|m| m.provider == "vercel" && m.id == "anthropic/claude-sonnet-5")
        .unwrap();
    assert_eq!(catalog::slug(sonnet), "vercel/anthropic/claude-sonnet-5");

    let go = catalog
        .iter()
        .find(|m| m.provider == "opencode-go")
        .unwrap();
    let zen = catalog
        .iter()
        .find(|m| m.provider == "opencode-zen")
        .unwrap();
    assert_ne!(go.base_url, zen.base_url, "Go and Zen must stay distinct");

    let grok43 = catalog
        .iter()
        .find(|m| m.provider == "xai" && m.id == "grok-4.3")
        .unwrap();
    assert_eq!(grok43.context_window, 1_000_000);
    let grok46 = catalog
        .iter()
        .find(|m| m.provider == "xai" && m.id == "grok-4.6")
        .unwrap();
    assert_eq!(grok46.context_window, 500_000);

    let thinking = |id: &str| {
        catalog
            .iter()
            .find(|m| m.provider == "anthropic" && m.id == id)
            .unwrap_or_else(|| panic!("{id} missing from the built-in catalog"))
            .thinking
    };
    for id in [
        "claude-fable-5",
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-opus-4-8",
    ] {
        assert_eq!(thinking(id), Thinking::Adaptive, "{id} must be adaptive");
    }
    assert_eq!(thinking("claude-haiku-4-5"), Thinking::Manual);

    let haiku = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-haiku-4-5")
        .unwrap();
    assert_eq!(
        haiku.max_output,
        Some(8_192),
        "haiku's real output ceiling is well under the Anthropic dialect's 32k default"
    );
}

#[test]
fn native_support_tier_is_explicit_and_narrow() {
    use e::core::providers::registry::SupportTier;
    let native = e::core::providers::registry::all()
        .iter()
        .filter(|provider| provider.tier == SupportTier::Native)
        .map(|provider| provider.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        native,
        ["anthropic", "google", "openai", "openai-codex"]
            .into_iter()
            .collect()
    );
}

#[test]
fn image_input_uses_magic_bytes_not_a_trusting_extension() {
    let root = std::env::temp_dir().join(format!(
        "e-image-input-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let image_path = root.join("image.data");
    std::fs::write(&image_path, b"\x89PNG\r\n\x1a\nrest").unwrap();
    let image = ImageInput::from_path(&image_path).unwrap();
    let cloned = image.clone();
    assert_eq!(image.media_type, "image/png");
    assert!(image.data_url().starts_with("data:image/png;base64,"));
    assert!(std::sync::Arc::ptr_eq(&image.data, &cloned.data));

    let fake = root.join("not-really.png");
    std::fs::write(&fake, b"plain text").unwrap();
    assert!(ImageInput::from_path(&fake)
        .unwrap_err()
        .contains("unsupported image data"));

    let too_many = vec!["unused".to_string(); 11];
    assert!(ImageInput::from_paths(&too_many)
        .unwrap_err()
        .contains("at most 10"));

    let mut aggregate = Vec::new();
    for index in 0..3 {
        let path = root.join(format!("large-{index}.png"));
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(14 * 1024 * 1024)
            .unwrap();
        aggregate.push(path.display().to_string());
    }
    assert!(ImageInput::from_paths(&aggregate)
        .unwrap_err()
        .contains("40 MiB"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn models_json_windows_and_overrides() {
    let _lock = env_lock();
    let catalog = catalog_with_models_json(
        r#"{"providers":{"local":{"base_url":"http://localhost:9999","context_window":64000,
            "models":["small", {"id":"big","context_window":1000000}]}}}"#,
    );
    let find = |id: &str| {
        catalog
            .iter()
            .find(|m| m.provider == "local" && m.id == id)
            .unwrap()
    };
    assert_eq!(
        find("small").context_window,
        64_000,
        "provider default applies"
    );
    assert_eq!(
        find("big").context_window,
        1_000_000,
        "per-model wins over provider default"
    );

    let catalog = catalog_with_models_json(
        r#"{"providers":{"anthropic":{"models":[
            {"id":"claude-haiku-4-5","max_output":4096}
        ]}}}"#,
    );
    let haiku = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-haiku-4-5")
        .unwrap();
    assert_eq!(
        haiku.max_output,
        Some(4_096),
        "a models.json override wins over the built-in max_output"
    );

    let catalog = catalog_with_models_json(
        r#"{"providers":{"opencode-go":{"models":[{"id":"kimi-k3","context_window":131072}]}}}"#,
    );
    let matches: Vec<_> = catalog
        .iter()
        .filter(|m| m.provider == "opencode-go" && m.id == "kimi-k3")
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "the file entry replaces the built-in, not duplicates it"
    );
    assert_eq!(matches[0].context_window, 131_072);
}

#[test]
fn partial_override_inherits_the_builtin() {
    let _lock = env_lock();
    // One field corrected; transport, window, efforts, and thinking stay
    // with the built-in. Three former tests, one reproduction.
    let catalog = catalog_with_models_json(
        r#"{"providers":{"anthropic":{"models":[
            {"id":"claude-opus-5","context_window":123456},
            "claude-sonnet-5"
        ]}}}"#,
    );
    let opus = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-opus-5")
        .unwrap();
    assert_eq!(opus.base_url, "https://api.anthropic.com");
    assert_eq!(opus.api, Api::Anthropic);
    assert_eq!(opus.context_window, 123_456);
    assert_eq!(opus.thinking, Thinking::Adaptive);

    let sonnet = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-sonnet-5")
        .unwrap();
    assert_eq!(sonnet.context_window, 1_000_000);
    assert_eq!(
        sonnet.efforts,
        vec!["low".to_string(), "medium".to_string(), "high".to_string()]
    );
    assert_eq!(sonnet.thinking, Thinking::Adaptive);
}

#[test]
fn a_custom_provider_without_base_url_is_rejected_with_a_warning() {
    let _lock = env_lock();
    let home = Home::new("base-required");
    home.write(
        "models.json",
        r#"{"providers":{"custom":{"models":["secret-target"]}}}"#,
    );

    let catalog = catalog::catalog();
    assert!(
        !catalog
            .iter()
            .any(|model| model.provider == "custom" && model.id == "secret-target"),
        "a provider with no endpoint must not inherit an unrelated host"
    );
    assert_eq!(
        catalog::config_warnings(),
        vec!["models.json: provider custom requires an explicit base_url"]
    );
}

#[test]
fn only_signed_in_providers_are_available() {
    let _lock = env_lock();
    clear_env_keys();
    let home = Home::new("avail");

    assert!(catalog::available().is_empty());
    assert!(catalog::resolve("grok-4.6").is_none());

    home.auth(r#"{"anthropic":{"key":"k"}}"#);
    let available = catalog::available();
    assert!(available.iter().all(|m| m.provider == "anthropic"));
    assert!(catalog::resolve("claude-fable-5").is_some());
    assert!(catalog::resolve("grok-4.6").is_none());

    home.write(
        "settings.json",
        r#"{"model":"opencode-go/deepseek-v4-flash"}"#,
    );
    assert_eq!(catalog::default_model().provider, "anthropic");
}

#[test]
fn cycle_pool_follows_the_scope() {
    let _lock = env_lock();
    clear_env_keys();
    let home = Home::new("scope");
    home.auth(r#"{"anthropic":{"key":"k"},"xai":{"key":"k"}}"#);

    assert_eq!(catalog::cycle_pool().len(), catalog::available().len());

    catalog::set_scope(&[
        "anthropic/claude-fable-5".into(),
        "xai/grok-4.6".into(),
        "openai/gpt-5.5".into(),
    ])
    .unwrap();
    let pool: Vec<String> = catalog::cycle_pool().iter().map(catalog::slug).collect();
    assert_eq!(pool, vec!["xai/grok-4.6", "anthropic/claude-fable-5"]);
}

#[test]
fn malformed_models_configuration_is_reported_without_panicking() {
    let _lock = env_lock();
    clear_env_keys();
    let home = Home::new("invalid-models-config");
    home.write(
        "models.json",
        r#"{"providers":{"anthropic":{"api":"not-a-dialect"},"custom":{"base_url":"https://example.invalid","models":[{"id":"test","thinking":"mystery"}]}}}"#,
    );

    let catalog = catalog::catalog();
    assert!(catalog.iter().any(|model| model.provider == "anthropic"));
    let warnings = catalog::config_warnings().join("\n");
    assert!(warnings.contains("unknown api dialect `not-a-dialect`"));
    assert!(warnings.contains("unknown thinking mode `mystery`"));
}

#[test]
fn env_keys_sign_providers_in() {
    let _lock = env_lock();
    let home = Home::new("envkey");
    clear_env_keys();

    assert!(catalog::available().is_empty());

    // Every key_env the registry declares is a real sign-in, not just
    // Anthropic's. Walk them so a new provider is covered by this test.
    for provider in e::core::providers::registry::all() {
        let Some(env) = &provider.auth.key_env else {
            continue;
        };
        std::env::set_var(env, "sk-from-env");
        let available = catalog::available();
        assert!(
            available.iter().any(|m| m.provider == provider.name),
            "{env} did not sign {} in",
            provider.name
        );
        if provider.name == "vercel" {
            assert!(catalog::resolve("anthropic/claude-sonnet-5").is_some());
            assert!(catalog::resolve("vercel/anthropic/claude-sonnet-5").is_some());
        }
        std::env::remove_var(env);
        clear_env_keys();
    }

    // auth.json still wins over the environment.
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-env");
    home.auth(r#"{"anthropic":{"key":"sk-file"}}"#);
    match e::core::auth::load().get("anthropic").unwrap() {
        e::core::auth::Credential::ApiKey { key } => assert_eq!(key, "sk-file"),
        _ => panic!("wrong credential kind"),
    }
    std::env::remove_var("ANTHROPIC_API_KEY");
}

/// Keyless local backends (Ollama, LM Studio) count as signed in with no
/// stored credential and no env var — but never as phantom `auth::load()`
/// entries, so first-run onboarding still sees an empty credential file.
/// Their models come solely from the live /models refresh, so with no
/// server running they contribute nothing to the catalog.
#[test]
fn keyless_local_providers_are_signed_in_without_credentials() {
    let _lock = env_lock();
    let _home = Home::new("keyless");
    clear_env_keys();

    let auth = e::core::auth::load();
    assert!(auth.is_empty(), "no phantom credentials for keyless locals");
    assert!(e::core::auth::signed_in(&auth, "ollama"));
    assert!(e::core::auth::signed_in(&auth, "lmstudio"));
    assert!(!e::core::auth::signed_in(&auth, "anthropic"));
    assert!(catalog::available().is_empty());
}

#[test]
fn legacy_opencode_auth_keys_still_sign_in() {
    let _lock = env_lock();
    let home = Home::new("legacy-auth");
    clear_env_keys();

    home.auth(r#"{"opencode":{"key":"sk-old"}}"#);
    let auth = e::core::auth::load();
    assert!(
        matches!(auth.get("opencode-zen"), Some(e::core::auth::Credential::ApiKey { key }) if key == "sk-old"),
        "legacy key not honored"
    );
    assert!(catalog::available()
        .iter()
        .any(|m| m.provider == "opencode-zen"));
}

#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn provider_reported_models_appear_without_a_release() {
    use std::io::{Read, Write};
    let _lock = env_lock();
    clear_env_keys();
    let home = Home::new("live");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = r#"{"data":[
            {"id":"brand-new-model","context_length":64000},
            {"id":"small","context_length":123456},
            {"id":"text-embedding-large"},
            {"id":"brand-new-model-20260101"},
            {"id":"fine-looking-embed","type":"embedding","context_length":8192},
            {"id":"typed-chat","type":"language","context_window":8000}
        ]}"#;
        let _ = a.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        sent
    });

    home.auth(r#"{"mock":{"key":"sk-live"}}"#);
    home.write(
        "models.json",
        format!(
            r#"{{"providers":{{"mock":{{"base_url":"http://127.0.0.1:{port}","api":"anthropic","supports_tools":false,"image_input":true,"models":["small"]}}}}}}"#
        ),
    );

    catalog::refresh_remote().await;
    let sent = server.join().unwrap();
    assert!(sent.contains("GET /models"));
    assert!(sent.contains("Bearer sk-live") || sent.contains("bearer sk-live"));

    let catalog = catalog::catalog();
    let fresh = catalog
        .iter()
        .find(|m| m.provider == "mock" && m.id == "brand-new-model")
        .expect("gateway model appears");
    assert_eq!(fresh.context_window, 64_000);
    assert_eq!(fresh.api, Api::Anthropic);
    assert!(!fresh.supports_tools);
    assert!(fresh.image_input);
    let known = catalog
        .iter()
        .find(|m| m.provider == "mock" && m.id == "small")
        .expect("declared model stays listed");
    assert_eq!(known.context_window, 123_456);
    assert!(!catalog.iter().any(|m| m.id == "text-embedding-large"));
    assert!(!catalog.iter().any(|m| m.id == "brand-new-model-20260101"));
    assert!(
        !catalog.iter().any(|m| m.id == "fine-looking-embed"),
        "a non-language type is dropped even when the id looks like chat"
    );
    let typed = catalog
        .iter()
        .find(|m| m.provider == "mock" && m.id == "typed-chat")
        .expect("language type is kept");
    assert_eq!(typed.context_window, 8_000);
    assert!(catalog::available()
        .iter()
        .any(|m| m.id == "brand-new-model"));

    catalog::refresh_remote().await;
}

// Google's live list is its own dialect: same `/models` path, but
// `x-goog-api-key` auth and a `models[].name` payload. The overlay must speak
// it — otherwise signed-in Google users only ever see the seed ids.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn google_model_refresh_speaks_the_gemini_dialect() {
    use std::io::{Read, Write};
    let _lock = env_lock();
    clear_env_keys();
    let home = Home::new("google-live");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = r#"{"models":[
            {"name":"models/gemini-fresh-pro","supportedGenerationMethods":["generateContent"],"inputTokenLimit":1048576},
            {"name":"models/gemini-text-embedding","supportedGenerationMethods":["embedContent"]},
            {"name":"models/veo-video","supportedGenerationMethods":["predictLongRunning"]}
        ]}"#;
        let _ = a.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        sent
    });

    home.auth(r#"{"google":{"key":"AIza-test"}}"#);
    // Every google id rides the mock: refresh_remote dedupes to one base_url
    // per provider — the first catalog entry's — so leaving any builtin on
    // the real endpoint would send the probe there instead of here.
    home.write(
        "models.json",
        format!(
            r#"{{"providers":{{"google":{{"base_url":"http://127.0.0.1:{port}","api":"google-generative-ai","models":["gemini-3-pro","gemini-3-flash","gemini-fresh-pro"]}}}}}}"#
        ),
    );

    catalog::refresh_remote().await;
    let sent = server.join().unwrap();
    assert!(sent.contains("GET /models"));
    assert!(
        sent.contains("x-goog-api-key: AIza-test"),
        "Google's list endpoint takes the key header, not a bearer token"
    );

    let catalog = catalog::catalog();
    let fresh = catalog
        .iter()
        .find(|m| m.provider == "google" && m.id == "gemini-fresh-pro")
        .expect("the `models[].name` payload applies to the catalog");
    assert_eq!(fresh.context_window, 1_048_576);
    assert_eq!(fresh.api, Api::Google);
    assert!(!catalog
        .iter()
        .any(|m| m.provider == "google" && (m.id.contains("embedding") || m.id.contains("veo"))));
}

// Anthropic declares the bare host as its base — `/v1` lives in the dialect's
// paths, so its list endpoint is `{base}/v1/models`, not `{base}/models`. The
// overlay must speak that path — otherwise every refresh 404s silently and
// Anthropic never goes live.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn anthropic_model_refresh_speaks_the_messages_dialect() {
    use std::io::{Read, Write};
    let _lock = env_lock();
    clear_env_keys();
    let home = Home::new("anthropic-live");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut a, _) = listener.accept().unwrap();
        let mut buf = [0u8; 8192];
        let n = a.read(&mut buf).unwrap();
        let sent = String::from_utf8_lossy(&buf[..n]).to_string();
        let body = r#"{"data":[
            {"id":"claude-fresh-large","type":"language","context_length":1000000},
            {"id":"claude-fresh","type":"language","context_length":200000},
            {"id":"claude-embed-fresh","type":"embedding","context_length":1000}
        ]}"#;
        let _ = a.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        sent
    });

    home.auth(r#"{"anthropic":{"key":"sk-ant-test"}}"#);
    // Every anthropic id rides the mock: refresh_remote probes the first
    // catalog entry per provider, so any builtin left on the real endpoint
    // would send the request to api.anthropic.com instead of here.
    home.write(
        "models.json",
        format!(
            r#"{{"providers":{{"anthropic":{{"base_url":"http://127.0.0.1:{port}","api":"anthropic-messages","models":["claude-fable-5","claude-opus-5","claude-sonnet-5","claude-opus-4-8","claude-haiku-4-5"]}}}}}}"#
        ),
    );

    catalog::refresh_remote().await;
    let sent = server.join().unwrap();
    assert!(
        sent.contains("GET /v1/models"),
        "anthropic lists under /v1, not /models: {sent}"
    );
    assert!(
        sent.contains("x-api-key: sk-ant-test"),
        "anthropic's list endpoint takes the key header, not a bearer token"
    );

    let catalog = catalog::catalog();
    let fresh = catalog
        .iter()
        .find(|m| m.provider == "anthropic" && m.id == "claude-fresh")
        .expect("the `data[].id` payload applies to the catalog");
    assert_eq!(fresh.context_window, 200_000);
    assert_eq!(fresh.api, Api::Anthropic);
    assert!(!catalog
        .iter()
        .any(|m| m.provider == "anthropic" && m.id.contains("embed")));
}

/// The completions dialect carries the model's effort knob on the
/// OpenAI-standard `reasoning_effort` field — sent whenever the agent
/// resolved one, absent when there is none or the knob is `off` (which has
/// no wire encoding here). This was the one dialect silently dropping it.
#[allow(clippy::await_holding_lock)]
#[tokio::test(flavor = "multi_thread")]
async fn completions_send_reasoning_effort_when_the_model_has_a_knob() {
    let _lock = env_lock();
    let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
    let stream = |effort: Option<&'static str>| async move {
        let (port, server) = serve_sse(&[sse]);
        let home = Home::new(&format!("effort-{}", effort.unwrap_or("none")));
        home.auth(r#"{"mock":{"key":"k"}}"#);
        let model = test_model("mock", port, Api::Completions);
        let request = Request {
            model,
            system: "sys".into(),
            messages: vec![ChatMessage::user("hi")],
            effort: effort.map(str::to_string),
            tools: Vec::new(),
        };
        collect_stream(request).await;
        server.join().unwrap().remove(0)
    };

    let sent = stream(Some("low")).await;
    assert_eq!(
        request_json(&sent)["reasoning_effort"],
        "low",
        "resolved effort must reach the completions wire: {sent}"
    );

    let sent = stream(None).await;
    assert!(
        request_json(&sent).get("reasoning_effort").is_none(),
        "a model with no knob must not send the field: {sent}"
    );

    let sent = stream(Some("off")).await;
    assert!(
        request_json(&sent).get("reasoning_effort").is_none(),
        "`off` has no completions encoding — absence is the closest thing: {sent}"
    );
}
