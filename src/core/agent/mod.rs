//! The agent: one session, one event stream, the tool loop.
//!
//! A turn is: request → stream text/reasoning/tool-call events → if the model
//! called tools, run them (yolo — no gate), append results, request again;
//! repeat until a reply arrives with no tool calls. Steering messages typed
//! mid-turn are drained between steps, before the next request. The whole turn
//! emits on one ordered channel and ends with exactly one `TurnEnd`.

pub mod compact;
pub mod context;
pub mod retry;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::core::providers::catalog::{slug, Model};
use crate::core::providers::{
    self, ChatMessage, Event as ProviderEvent, FailureCause, FinishReason, Request, ToolCall,
};
use crate::core::session::Session;
use crate::core::tools;

/// Steps (provider requests, tool batches between them) one turn may run
/// before it stops and asks to be continued. A backstop against a model
/// stuck calling tools forever — set far above any legitimate turn.
const MAX_STEPS: u32 = 256;

/// One handle for everything a commit needs — history, the session log, and
/// the persistence-warning latch — so the turn loop appends a message with
/// one call instead of threading six parameters through every site.
#[derive(Clone)]
struct TurnLog {
    history: Arc<Mutex<Vec<ChatMessage>>>,
    session: Arc<Mutex<Option<Session>>>,
    cwd: PathBuf,
    model: Model,
    session_name: Arc<Mutex<Option<String>>>,
    persist_warned: Arc<AtomicBool>,
    events: mpsc::Sender<SessionEvent>,
}

impl TurnLog {
    /// Append a message to history and to the session log, creating the log
    /// on the first message. The in-memory turn always proceeds; a
    /// persistence failure warns once per episode (see `note_persist`) —
    /// silently losing history is the one thing this must never do.
    fn commit(&self, message: ChatMessage) {
        let result = self.append(message);
        note_persist(&self.persist_warned, result, &self.events);
    }

    fn append(&self, message: ChatMessage) -> std::io::Result<()> {
        self.history.lock().unwrap().push(message.clone());
        let mut guard = self.session.lock().unwrap();
        if guard.is_none() {
            let mut created = Session::create(&self.cwd, &slug(&self.model))?;
            // A pending name applies before the first record. It is
            // best-effort: failing here must not discard the freshly created
            // log — dropping it would make the next commit open a different
            // file and strand every message already in memory outside any
            // session.
            if let Some(name) = self.session_name.lock().unwrap().clone() {
                let _ = created.set_name(&name);
            }
            *guard = Some(created);
        }
        match guard.as_mut() {
            Some(s) => s.append(&message),
            None => Ok(()),
        }
    }
}

/// Turn a commit result into at most one warning per failure episode: the
/// first failure warns, later ones stay quiet until a commit succeeds again.
/// The latch sets only when the warning was actually delivered — a full
/// channel at the first failure must not silently swallow the episode.
fn note_persist(
    warned: &AtomicBool,
    result: std::io::Result<()>,
    events: &mpsc::Sender<SessionEvent>,
) {
    match result {
        Ok(()) => warned.store(false, Ordering::SeqCst),
        Err(e) => {
            if !warned.load(Ordering::SeqCst)
                && events
                    .try_send(SessionEvent::Warning(format!(
                        "session not saved: {e} — the conversation continues in memory only"
                    )))
                    .is_ok()
            {
                warned.store(true, Ordering::SeqCst);
            }
        }
    }
}

fn clone_request(r: &Request) -> Request {
    Request {
        model: r.model.clone(),
        system: r.system.clone(),
        messages: r.messages.clone(),
        effort: r.effort.clone(),
        tools: r.tools.clone(),
    }
}

/// Resolve when Esc (or any interrupt) has been requested. Polled on a short
/// interval so a stalled provider stream — which never yields another event —
/// cannot strand the turn with `running` stuck true and Esc inert.
async fn wait_cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Sleep for `delay`, but give up early the moment Esc is pressed. A retry
/// backoff can run up to 30 seconds — a bare `sleep` would leave Esc inert
/// for the whole wait, the same stalled-spinner bug the stream loop already
/// guards against. Returns false when cancelled before the delay elapsed.
async fn sleep_cancellable(delay: Duration, cancel: &AtomicBool) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => true,
        _ = wait_cancelled(cancel) => false,
    }
}

/// Presentation contract for one call in a provider-issued tool batch.
#[derive(Clone, Debug)]
pub struct ToolCallPresentation {
    pub id: u64,
    pub category: String,
    pub running: String,
    pub completed: String,
    pub target: String,
}

#[derive(Debug)]
pub enum SessionEvent {
    TurnStart,
    TextDelta(String),
    ReasoningDelta(String),
    /// All calls from one assistant message, known before serial execution.
    ToolBatchStart {
        calls: Vec<ToolCallPresentation>,
    },
    /// One member of the batch now owns execution focus.
    ToolStart {
        id: u64,
    },
    /// A command pipe chunk observed before process completion.
    ToolOutput {
        id: u64,
        stream: tools::OutputStream,
        chunk: String,
    },
    /// A tool finished with a typed outcome and bounded full output.
    ToolEnd {
        id: u64,
        outcome: tools::ToolOutcome,
        summary: String,
        content: String,
    },
    /// Cumulative bytes of tool-call argument JSON streamed so far this
    /// step. Argument assembly is the one long stream phase with no other
    /// event — without this the UI freezes while the turn is alive.
    ToolCallAssembly {
        bytes: u64,
    },
    /// An extension tool named the session.
    Named(String),
    Usage {
        input: u64,
        output: u64,
        cache_read: u64,
    },
    Error(String),
    /// A non-fatal turn problem worth showing: a truncated or refused reply
    /// the provider delivered as success, or malformed stream frames that
    /// were skipped.
    Warning(String),
    /// A retryable failure is being backed off before another attempt.
    Retry {
        attempt: u32,
        limit: u32,
        delay_secs: u64,
        cause: FailureCause,
        reason: String,
    },
    /// The first attempt after one or more retries produced something —
    /// shown briefly before the row reverts to normal turn activity.
    Recovered {
        attempt: u32,
        limit: u32,
    },
    /// A steering message was accepted mid-turn (for display as a user block).
    Steered(String),
    TurnEnd {
        aborted: bool,
    },
}

/// The effort a model would use given its declared `levels` and the saved
/// setting: the saved value when this model supports it, else the model's
/// strong default — `high` when declared, otherwise its first level. None
/// when the model has no reasoning knob at all.
pub fn effort(levels: &[String], saved: Option<&str>) -> Option<String> {
    if levels.is_empty() {
        return None;
    }
    match saved {
        Some(v) if levels.iter().any(|l| l == v) => Some(v.to_string()),
        _ => Some(if levels.iter().any(|l| l == "high") {
            "high".to_string()
        } else {
            levels[0].clone()
        }),
    }
}

/// The next level after `current` in the model's cycle, wrapping around.
pub fn next_effort(levels: &[String], current: &str) -> String {
    let idx = levels.iter().position(|l| l == current).unwrap_or(0);
    levels[(idx + 1) % levels.len()].clone()
}

pub struct Agent {
    pub model: Model,
    /// The extension host; None means built-in tools only.
    host: Option<std::sync::Arc<crate::core::api::ExtensionHost>>,
    cwd: PathBuf,
    history: Arc<Mutex<Vec<ChatMessage>>>,
    events: mpsc::Sender<SessionEvent>,
    /// Messages typed while a turn runs (steered or queued per settings).
    pending: Arc<Mutex<Vec<String>>>,
    cancel: Arc<AtomicBool>,
    running: bool,
    /// The session log; every committed message is appended.
    session: Arc<Mutex<Option<Session>>>,
    /// An extension-set display name, applied when the log exists or when it
    /// is created on the first message.
    session_name: Arc<Mutex<Option<String>>>,
    /// Latch for the persistence-failure warning (see `note_persist`).
    persist_warned: Arc<AtomicBool>,
    /// Display ids for tool lifecycle events, unique across the whole
    /// session: an Esc-detached task from an earlier turn keeps a live
    /// events sender, and a per-turn counter would let its stale ToolEnd
    /// collide with (and corrupt) a later turn's row.
    tool_seq: Arc<AtomicU64>,
}

impl Agent {
    pub fn new(model: Model) -> (Self, mpsc::Receiver<SessionEvent>) {
        let (events, rx) = mpsc::channel(256);
        let agent = Agent {
            model,
            host: None,
            cwd: std::env::current_dir().unwrap_or_default(),
            history: Arc::new(Mutex::new(Vec::new())),
            events,
            pending: Arc::new(Mutex::new(Vec::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            running: false,
            session: Arc::new(Mutex::new(None)),
            session_name: Arc::new(Mutex::new(None)),
            persist_warned: Arc::new(AtomicBool::new(false)),
            tool_seq: Arc::new(AtomicU64::new(0)),
        };
        (agent, rx)
    }

    /// Attach the extension host: its tools join (and may override) the
    /// built-ins, and its hooks gate every tool call.
    pub fn set_host(&mut self, host: std::sync::Arc<crate::core::api::ExtensionHost>) {
        self.host = Some(host);
    }

    pub fn is_streaming(&self) -> bool {
        self.running
    }
    pub fn model_slug(&self) -> String {
        slug(&self.model)
    }
    /// The model's declared effort levels, in order; empty when it has no
    /// reasoning knob.
    pub fn efforts(&self) -> Vec<String> {
        self.model.efforts.clone()
    }
    /// The effort for the next request: the saved setting when this model
    /// supports it, else the model's strong default (`high` when declared,
    /// otherwise its first level).
    pub fn effort(&self) -> Option<String> {
        effort(
            &self.model.efforts,
            crate::core::config::settings::get_string("effort").as_deref(),
        )
    }
    /// Advance to the model's next effort level and persist it. None when
    /// the model has no reasoning knob.
    pub fn cycle_effort(&self) -> Option<String> {
        let levels = self.efforts();
        if levels.is_empty() {
            return None;
        }
        let next = next_effort(&levels, self.effort()?.as_str());
        crate::core::config::settings::set_string("effort", &next);
        Some(next)
    }
    pub fn history_snapshot(&self) -> Vec<ChatMessage> {
        self.history.lock().unwrap().clone()
    }
    pub fn load_history(&self, messages: Vec<ChatMessage>) {
        *self.history.lock().unwrap() = messages;
    }
    pub fn clear(&self) {
        self.history.lock().unwrap().clear();
    }

    /// The commit handle over this agent's history, session, and warning
    /// latch — the one way messages enter the record.
    fn log(&self) -> TurnLog {
        TurnLog {
            history: self.history.clone(),
            session: self.session.clone(),
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            session_name: self.session_name.clone(),
            persist_warned: self.persist_warned.clone(),
            events: self.events.clone(),
        }
    }

    /// Commit a user-visible fact into history and the session log without
    /// starting a turn — the `!` shell passthrough records its output this way
    /// so the model sees what the user ran.
    pub fn record_user(&self, text: String) {
        self.log().commit(ChatMessage::user(text));
    }

    /// Replace the history with the compaction seed plus the kept recent
    /// messages, committing everything into a fresh session file so the
    /// compacted state is itself resumable; the old file stays untouched.
    /// The fresh log is created before the old one is detached: if creation
    /// fails, the old log stays attached and later turns append to it, so a
    /// crash resumes into the complete pre-compaction conversation instead
    /// of a new file holding only an unanchored tail.
    pub fn load_compacted(&self, summary: &str, kept: Vec<ChatMessage>) -> bool {
        let seed_message = ChatMessage::user(crate::core::agent::compact::seed(summary));
        let mut fresh_history = Vec::with_capacity(kept.len() + 1);
        fresh_history.push(seed_message.clone());
        fresh_history.extend(kept);

        // Same lock order as `commit`: history before session.
        let mut history_guard = self.history.lock().unwrap();
        let mut guard = self.session.lock().unwrap();
        let result = match Session::create(&self.cwd, &slug(&self.model)) {
            Ok(mut created) => {
                // Same best-effort pending-name application as `commit`.
                if let Some(name) = self.session_name.lock().unwrap().clone() {
                    let _ = created.set_name(&name);
                }
                for message in &fresh_history {
                    if let Err(error) = created.append(message) {
                        // This file never became the active compacted log. Do
                        // not leave a plausible partial session in /resume.
                        let failed_path = created.path().to_path_buf();
                        drop(created);
                        let _ = std::fs::remove_file(failed_path);
                        note_persist(&self.persist_warned, Err(error), &self.events);
                        return false;
                    }
                }
                *guard = Some(created);
                *history_guard = fresh_history;
                Ok(())
            }
            Err(e) => Err(e),
        };
        let installed = result.is_ok();
        note_persist(&self.persist_warned, result, &self.events);
        installed
    }

    /// Attach a session log; created lazily on the first message when None.
    pub fn set_session(&self, session: Option<Session>) {
        *self.session.lock().unwrap() = session;
    }

    /// Name this session: applies immediately when a log exists, otherwise
    /// when the log is created on the first message. Either way the name is
    /// idempotent — the last one wins.
    pub fn set_session_name(&self, name: String) {
        *self.session_name.lock().unwrap() = Some(name);
        let mut guard = self.session.lock().unwrap();
        if let Some(s) = guard.as_mut() {
            if let Some(name) = self.session_name.lock().unwrap().clone() {
                let result = s.set_name(&name);
                drop(guard);
                note_persist(&self.persist_warned, result, &self.events);
            }
        }
    }

    /// Adopt the name a resumed session carries (or clear it for a fresh
    /// one). In-memory only — the log already holds its own name entries.
    pub fn adopt_session_name(&self, name: Option<String>) {
        *self.session_name.lock().unwrap() = name;
    }

    /// Prompts waiting on the running turn (steering not yet drained).
    pub fn queued_count(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// The extension-set session name, if any (the derived title still
    /// exists but the name overrides it in /resume).
    pub fn session_name(&self) -> Option<String> {
        self.session_name.lock().unwrap().clone()
    }

    /// Drop the session name — a fresh session starts unnamed.
    pub fn clear_session_name(&self) {
        *self.session_name.lock().unwrap() = None;
    }

    pub fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }

    /// Queue a message. If a turn is running it steers (drained next step);
    /// otherwise it starts a turn.
    /// A message typed while a turn runs never fires immediately: it is held
    /// and steered into the turn at the next step. Returns true if held,
    /// false if it began a fresh turn.
    pub fn submit(&mut self, text: String, system: String) -> bool {
        if self.running {
            self.pending.lock().unwrap().push(text);
            return true;
        }
        self.log().commit(ChatMessage::user(text));
        self.start(system);
        false
    }

    fn start(&mut self, system: String) {
        self.running = true;
        self.cancel.store(false, Ordering::SeqCst);
        let log = self.log();
        let events = self.events.clone();
        let history = self.history.clone();

        let cancel = self.cancel.clone();
        let model = self.model.clone();
        let cwd = self.cwd.clone();
        let effort = self.effort();
        let pending = self.pending.clone();
        let host = self.host.clone();
        let tool_seq = self.tool_seq.clone();

        tokio::spawn(async move {
            let _ = events.send(SessionEvent::TurnStart).await;
            // One free retry for a blank success per turn: an empty stream is
            // the most transient failure there is, and ending the turn on the
            // first one traded a 1s pause for a dead turn.
            let mut empty_retried = false;
            // Steps this turn has run (one request each). The cap is a
            // runaway backstop far above real work, not a working budget.
            let mut steps = 0u32;
            let aborted = 'turn: loop {
                if cancel.load(Ordering::SeqCst) {
                    break true;
                }
                steps += 1;
                if steps > MAX_STEPS {
                    let _ = events
                        .send(SessionEvent::Warning(format!(
                            "turn stopped after {MAX_STEPS} steps — send a message to continue"
                        )))
                        .await;
                    break false;
                }
                // Steer: fold any pending messages into this turn between steps.
                let steered: Vec<String> = { pending.lock().unwrap().drain(..).collect() };
                for message in steered {
                    let _ = events.send(SessionEvent::Steered(message.clone())).await;
                    log.commit(ChatMessage::user(message));
                }

                let messages = { history.lock().unwrap().clone() };
                // Some compatible gateways omit usage entirely. Keep a local,
                // conservative fallback so the mid-turn safety guard still
                // exists there; a real Usage frame replaces it below.
                let mut last_context = compact::estimate_request_tokens(&system, &messages);
                let request = Request {
                    model: model.clone(),
                    system: system.clone(),
                    messages,
                    effort: effort.clone(),
                    tools: match &host {
                        Some(h) => h.merged_tool_schemas(),
                        None => tools::schemas(),
                    },
                };

                // Total provider requests made for this step, including the
                // initial request. MAX_ATTEMPTS is a request budget, not a
                // retry budget.
                let mut attempt = 1u32;
                let (mut rx, mut handle) = providers::stream(clone_request(&request));

                let mut text = String::new();
                let mut calls: Vec<ToolCall> = Vec::new();
                let mut reasoning_items: Vec<String> = Vec::new();
                // A dialect can stream thought deltas without ever
                // committing a ReasoningItem (e.g. Gemini's empty-text
                // thought chunks), so a thinking-only stream must still
                // count as produced by this flag alone: otherwise a
                // retryable error after a long thinking phase would retry
                // (replaying the thoughts on screen) and a thinking-only
                // stream ending without text would be called an empty
                // response.
                let mut reasoning_streamed = false;
                // Latest usage frame this step; emitted once after the stream
                // ends. Dialects may report usage cumulatively mid-stream
                // (Gemini sends usageMetadata per chunk), so forwarding every
                // frame would let a consumer that sums per-step usage count
                // the same tokens more than once.
                let mut step_usage: Option<(u64, u64, u64)> = None;
                // Cumulative argument bytes this attempt, for the liveness
                // row. Deliberately not part of the retry-safety check: a
                // partial call never left the dialect, so replaying the
                // request commits nothing twice.
                let mut assembly_bytes = 0u64;
                let mut errored = false;
                // True once this attempt has streamed anything at all — the
                // signal both for "recovered" (first content after a retry)
                // and for whether a fresh failure is still safe to retry.
                let mut recovered_notified = false;
                // Cancel must win even when the provider yields nothing: a
                // prior `while let Some(event) = rx.recv()` only checked Esc
                // after the next byte arrived, so a stalled SSE left the
                // spinner running and Esc inert until the socket moved.
                // Esc mid-stream falls through to the partial-commit path
                // below instead of breaking the turn here: text the user
                // watched stream must reach history, or the next turn's model
                // has no memory of words the user is replying to.
                let mut stream_cancelled = false;
                'stream: loop {
                    let event = tokio::select! {
                        event = rx.recv() => event,
                        _ = wait_cancelled(&cancel) => {
                            handle.abort();
                            stream_cancelled = true;
                            break 'stream;
                        }
                    };
                    let Some(event) = event else {
                        break 'stream;
                    };
                    // The new stream (after one or more retries) just
                    // produced its first non-error event — the retry
                    // worked. An immediate second failure is not a recovery,
                    // so this excludes Error and lets that arm decide
                    // whether to retry again instead. `attempt` is 1-based:
                    // 1 is the first try, so only attempt 2+ ever recovered.
                    if attempt > 1
                        && !recovered_notified
                        && !matches!(event, ProviderEvent::Error(_))
                    {
                        recovered_notified = true;
                        let _ = events
                            .send(SessionEvent::Recovered {
                                attempt,
                                limit: retry::MAX_ATTEMPTS,
                            })
                            .await;
                    }
                    match event {
                        ProviderEvent::TextDelta(d) => {
                            text.push_str(&d);
                            let _ = events.send(SessionEvent::TextDelta(d)).await;
                        }
                        ProviderEvent::ReasoningDelta(d) => {
                            reasoning_streamed = true;
                            let _ = events.send(SessionEvent::ReasoningDelta(d)).await;
                        }
                        ProviderEvent::ToolCallDelta { bytes } => {
                            assembly_bytes += bytes;
                            let _ = events
                                .send(SessionEvent::ToolCallAssembly {
                                    bytes: assembly_bytes,
                                })
                                .await;
                        }
                        ProviderEvent::ToolCall(call) => calls.push(call),
                        ProviderEvent::ReasoningItem(item) => reasoning_items.push(item),
                        ProviderEvent::Usage {
                            input,
                            output,
                            cache_read,
                        } => {
                            step_usage = Some((input, output, cache_read));
                        }
                        ProviderEvent::Error(err) => {
                            // Safe to retry only when the cause itself is
                            // retryable (never Auth or Rejected) AND nothing
                            // has streamed yet this attempt: a delivered
                            // request that already produced output or ran
                            // tools cannot be replayed without risking a
                            // duplicate.
                            let nothing_produced = text.is_empty()
                                && calls.is_empty()
                                && reasoning_items.is_empty()
                                && !reasoning_streamed;
                            if err.cause.is_retryable()
                                && nothing_produced
                                && attempt < retry::MAX_ATTEMPTS
                            {
                                let retry_number = attempt;
                                attempt += 1;
                                let delay = retry::delay_for(retry_number, err.retry_after);
                                let _ = events
                                    .send(SessionEvent::Retry {
                                        attempt,
                                        limit: retry::MAX_ATTEMPTS,
                                        delay_secs: delay.as_secs(),
                                        cause: err.cause,
                                        reason: err.short.clone(),
                                    })
                                    .await;
                                if !sleep_cancellable(delay, &cancel).await {
                                    handle.abort();
                                    break 'turn true;
                                }
                                let (nrx, nhandle) = providers::stream(clone_request(&request));
                                rx = nrx;
                                handle = nhandle;
                                // A fresh attempt streams its arguments from
                                // scratch; the liveness counter follows.
                                assembly_bytes = 0;
                                continue 'stream;
                            }
                            // Distinguish genuine exhaustion (the cause was
                            // retryable and nothing had streamed, but the
                            // budget ran out) from a failure that simply
                            // can't be retried at all — partial content
                            // already produced this attempt, or a rejected
                            // cause. Only the former earns "gave up after
                            // N/M"; the latter would misreport why the
                            // attempt stopped.
                            let message = if err.cause.is_retryable() && nothing_produced {
                                format!(
                                    "{} — gave up after {attempt}/{} attempts: {}",
                                    err.cause.label(),
                                    retry::MAX_ATTEMPTS,
                                    err.message
                                )
                            } else {
                                err.message
                            };
                            let _ = events.send(SessionEvent::Error(message)).await;
                            errored = true;
                        }
                        ProviderEvent::Done(end) => {
                            // The stream completed, but not necessarily with
                            // the full answer — a truncated, refused, or
                            // filtered reply arrives as an HTTP success and
                            // must not pass silently.
                            let finish_warning = match &end.finish {
                                FinishReason::Normal | FinishReason::ToolCalls => None,
                                FinishReason::Length => Some(
                                    "reply truncated: the provider hit its output limit".into(),
                                ),
                                FinishReason::Refusal => {
                                    Some("the model refused to answer".to_string())
                                }
                                FinishReason::ContentFilter => Some(
                                    "output blocked by the provider's content filter".to_string(),
                                ),
                                FinishReason::Other(reason) => {
                                    Some(format!("turn ended abnormally: {reason}"))
                                }
                            };
                            if let Some(warning) = finish_warning {
                                let _ = events.send(SessionEvent::Warning(warning)).await;
                            }
                            if end.malformed > 0 {
                                let _ = events
                                    .send(SessionEvent::Warning(format!(
                                        "{} malformed stream event{} skipped",
                                        end.malformed,
                                        if end.malformed == 1 { "" } else { "s" }
                                    )))
                                    .await;
                            }
                        }
                    }
                }
                // One Usage per step, the stream's final frame: `input` is
                // this request's full context, `output` what this step alone
                // generated. Emitted even when the stream then errored — the
                // tokens were still consumed.
                if let Some((input, output, cache_read)) = step_usage {
                    last_context = input + output;
                    let _ = events
                        .send(SessionEvent::Usage {
                            input,
                            output,
                            cache_read,
                        })
                        .await;
                } else {
                    let provisional = ChatMessage::assistant(text.clone(), calls.clone());
                    last_context =
                        last_context.saturating_add(compact::estimate_message_tokens(&provisional));
                }
                // A cancelled or failed stream still commits what it already
                // produced: the user watched that text arrive, and a history
                // missing it would have the model contradict its own visible
                // words next turn. Calls that never ran get a synthetic
                // result — a dangling tool_use without its tool_result fails
                // the next request on every dialect.
                if stream_cancelled || errored {
                    if !text.is_empty() || !calls.is_empty() {
                        for item in reasoning_items.drain(..) {
                            log.commit(ChatMessage::reasoning(item));
                        }
                        let (note, outcome, summary) = if stream_cancelled {
                            (
                                "not executed — the turn was cancelled before this call ran",
                                tools::ToolOutcome::Cancelled,
                                "cancelled",
                            )
                        } else {
                            (
                                "not executed — the provider stream failed before this call ran",
                                tools::ToolOutcome::Failed,
                                "error",
                            )
                        };
                        let unrun = calls.clone();
                        log.commit(ChatMessage::assistant(std::mem::take(&mut text), calls));
                        for call in unrun {
                            log.commit(ChatMessage::tool_result_with_meta(
                                call.id, note, outcome, summary,
                            ));
                        }
                    }
                    break stream_cancelled;
                }

                // A stream that ends with no text, no calls, no reasoning,
                // and no error is a blank success — committing it would strand
                // the turn in silence. It is also the most transient failure
                // a provider produces, so it gets one quiet re-request per
                // turn before the error. Abnormal finishes and skipped frames
                // were already surfaced as warnings above; past the retry we
                // guarantee the user at least sees that something went wrong.
                if text.is_empty()
                    && calls.is_empty()
                    && reasoning_items.is_empty()
                    && !reasoning_streamed
                    && pending.lock().unwrap().is_empty()
                {
                    if !empty_retried {
                        empty_retried = true;
                        let _ = events
                            .send(SessionEvent::Retry {
                                attempt: 2,
                                limit: retry::MAX_ATTEMPTS,
                                delay_secs: 1,
                                cause: FailureCause::ProviderUnavailable,
                                reason: "empty response".into(),
                            })
                            .await;
                        if !sleep_cancellable(Duration::from_secs(1), &cancel).await {
                            break 'turn true;
                        }
                        continue 'turn;
                    }
                    let _ = events
                        .send(SessionEvent::Error(
                            "the model returned an empty response".into(),
                        ))
                        .await;
                    break false;
                }

                // Reasoning items commit first — the dialect that produced
                // them must replay them ahead of the assistant turn.
                for item in reasoning_items.drain(..) {
                    log.commit(ChatMessage::reasoning(item));
                }
                // Commit the assistant turn (text + any calls).
                log.commit(ChatMessage::assistant(text, calls.clone()));

                if calls.is_empty() {
                    // A plain reply would end the turn — but a message that
                    // landed mid-reply must still be delivered, so continue the
                    // turn to pick it up rather than stranding it.
                    if !pending.lock().unwrap().is_empty() {
                        continue;
                    }
                    break false;
                }

                // Resolve the complete batch before serial execution so the
                // transcript has one stable group from the first call.
                let mut batch = Vec::with_capacity(calls.len());
                for call in &calls {
                    let args: serde_json::Value =
                        serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);
                    let presentation = tools::present(&call.name, &args);
                    let id = tool_seq.fetch_add(1, Ordering::SeqCst) + 1;
                    batch.push((
                        id,
                        call.clone(),
                        ToolCallPresentation {
                            id,
                            category: presentation.category,
                            running: presentation.running,
                            completed: presentation.completed,
                            target: presentation.target,
                        },
                    ));
                }
                let _ = events
                    .send(SessionEvent::ToolBatchStart {
                        calls: batch.iter().map(|(_, _, shown)| shown.clone()).collect(),
                    })
                    .await;

                // Run the batch concurrently (the reference behavior): every
                // child streams its own lifecycle on the shared channel as it
                // progresses. Results commit in assistant source order.
                let mut handles = Vec::with_capacity(batch.len());
                for (id, call, _) in batch {
                    let host = host.clone();
                    let cancel = cancel.clone();
                    let events = events.clone();
                    let cwd = cwd.clone();
                    handles.push((
                        call.clone(),
                        tokio::spawn(async move {
                            let _ = events.send(SessionEvent::ToolStart { id }).await;
                            let output = run_tool(
                                &host,
                                &call.name,
                                &call.arguments,
                                &cwd,
                                cancel,
                                id,
                                events.clone(),
                            )
                            .await;
                            let _ = events
                                .send(SessionEvent::ToolEnd {
                                    id,
                                    outcome: output.outcome,
                                    summary: output.summary.clone(),
                                    // The viewer gets the rich detail (full
                                    // diffs); history keeps the lean content.
                                    content: output.display_text().to_string(),
                                })
                                .await;
                            output
                        }),
                    ));
                }

                for (call, handle) in handles {
                    // A blocked filesystem operation cannot be interrupted in
                    // place; on Esc, stop waiting, record the call as
                    // cancelled, and detach the task — the turn must end
                    // promptly even over a stalled FIFO or NFS mount.
                    let output = tokio::select! {
                        biased;
                        joined = handle => match joined {
                            Ok(output) => output,
                            Err(_) => tools::ToolOutput {
                                content: "tool panicked".into(),
                                outcome: tools::ToolOutcome::Failed,
                                summary: "error".into(),
                                display: None,
                            },
                        },
                        _ = wait_cancelled(&cancel) => tools::ToolOutput {
                            // Honest record: the blocked operation is only
                            // detached, so it may still complete after this.
                            content: "tool cancelled — the underlying operation \
                                      may still complete in the background"
                                .into(),
                            outcome: tools::ToolOutcome::Cancelled,
                            summary: "cancelled".into(),
                            display: None,
                        },
                    };
                    last_context = last_context
                        .saturating_add((output.content.chars().count() as u64).div_ceil(4));
                    log.commit(ChatMessage::tool_result_with_meta(
                        call.id,
                        output.content,
                        output.outcome,
                        output.summary,
                    ));
                }
                if cancel.load(Ordering::SeqCst) {
                    break 'turn true;
                }
                // Context guard: a long tool loop grows the context fastest
                // exactly where compaction (which runs between turns) can't
                // reach it, and the next request past the window dies on a
                // rejected 400 with the work half-done. When real usage says
                // the reserve is spent, end the turn cleanly instead — the
                // frontend compacts at TurnEnd — and queue a continuation so
                // the task resumes in the fresh context.
                if last_context > 0 && compact::should_compact(last_context, model.context_window) {
                    let _ = events
                        .send(SessionEvent::Warning(
                            "context nearly full — pausing to compact, then continuing".into(),
                        ))
                        .await;
                    // Worded so it stays true even if the compaction the
                    // frontend runs at TurnEnd fails — it must not claim a
                    // compaction that may not have happened.
                    pending.lock().unwrap().insert(
                        0,
                        "The turn was paused because the context ran low. Continue the task \
                         from where it left off."
                            .into(),
                    );
                    break 'turn false;
                }
            };
            let _ = events.send(SessionEvent::TurnEnd { aborted }).await;
            if let Some(h) = &host {
                h.event("turn_end", serde_json::json!({"aborted": aborted}))
                    .await;
            }
        });
    }

    /// Called by the frontend after each TurnEnd so a new turn may start.
    /// Returns any prompts stranded by the shutdown race: submitted after
    /// the worker's final empty-queue check but before this call, with no
    /// worker left to drain them. The frontend must resubmit them in order.
    pub fn on_turn_end(&mut self) -> Vec<String> {
        self.running = false;
        self.pending.lock().unwrap().drain(..).collect()
    }

    pub fn interrupt(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if !self.running {
            let _ = self
                .events
                .try_send(SessionEvent::TurnEnd { aborted: true });
        }
    }
}

/// Dispatch one tool call: extension hooks may block it, an extension that
/// owns the name serves it, otherwise the built-in runs on a blocking thread.
async fn run_tool(
    host: &Option<std::sync::Arc<crate::core::api::ExtensionHost>>,
    name: &str,
    arguments: &str,
    cwd: &std::path::Path,
    cancel: Arc<AtomicBool>,
    id: u64,
    events: mpsc::Sender<SessionEvent>,
) -> tools::ToolOutput {
    if let Some(h) = host {
        // The hook chain is bounded per extension, but Esc must not wait out
        // even one silent hook's timeout: race it against the cancel flag.
        // Dropping the hook future also drops its pending-map entry.
        let hook = h.hook_tool_call(name, arguments);
        tokio::pin!(hook);
        let blocked = tokio::select! {
            verdict = &mut hook => verdict,
            _ = wait_cancelled(&cancel) => {
                return tools::ToolOutput {
                    content: "tool cancelled".into(),
                    outcome: tools::ToolOutcome::Cancelled,
                    summary: "cancelled".into(),
                    display: None,
                };
            }
        };
        if let Some(reason) = blocked {
            return tools::ToolOutput {
                content: format!("Tool call blocked by extension: {reason}"),
                outcome: tools::ToolOutcome::Blocked,
                summary: "blocked".into(),
                display: None,
            };
        }
        if h.owns_tool(name) {
            // Same race as the hook above: the call against the cancel flag,
            // through the one shared helper — not a second hand-rolled poll.
            let result = tokio::select! {
                result = h.call_tool(name, arguments) => result,
                _ = wait_cancelled(&cancel) => {
                    return tools::ToolOutput {
                        content: "extension tool cancelled".into(),
                        outcome: tools::ToolOutcome::Cancelled,
                        summary: "cancelled".into(),
                        display: None,
                    };
                }
            };
            // An extension tool may name the session as a side effect; the
            // UI applies it on SessionEvent::Named.
            if let Some(new_name) = result.session_name.clone() {
                let _ = events.send(SessionEvent::Named(new_name)).await;
            }
            let outcome = if result.is_error {
                tools::ToolOutcome::Failed
            } else {
                tools::ToolOutcome::Completed
            };
            return tools::ToolOutput {
                content: result.content,
                outcome,
                summary: if outcome.is_error() {
                    "error".into()
                } else {
                    "done".into()
                },
                display: None,
            };
        }
    }
    let name = name.to_string();
    let arguments = arguments.to_string();
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        tools::run_streaming(&name, &arguments, &cwd, &cancel, |stream, chunk| {
            let chunk = tools::sanitize_display(chunk);
            if !chunk.is_empty() {
                // Blocks this pump thread when the channel is full, so the
                // SessionEvent receiver must never block on I/O — a stalled
                // consumer here stalls the tool it is reporting on.
                let _ = events.blocking_send(SessionEvent::ToolOutput { id, stream, chunk });
            }
        })
    })
    .await
    .unwrap_or(tools::ToolOutput {
        content: "tool panicked".into(),
        outcome: tools::ToolOutcome::Failed,
        summary: "error".into(),
        display: None,
    })
}
