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
pub mod wake;

/// The continuation message committed after a sleep-caused mid-reply loss:
/// history already holds the truncated reply, so this is the whole prompt
/// the model needs to finish its own sentence.
const SLEEP_CONTINUATION: &str = "Your previous reply was cut off because the device slept. \
Continue from exactly where it stopped.";

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::core::cli::ToolMode;
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
    save_session: bool,
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

    /// Same as `commit`, but for the turn loop's own async task: `append`
    /// does synchronous session-file I/O, which would otherwise block that
    /// task's tokio worker thread on every committed message. Running it on
    /// the blocking pool gives it the same treatment `run_tool` already
    /// gives the built-in tools' blocking work, instead of the turn loop
    /// being the one place that skips it.
    async fn commit_async(&self, message: ChatMessage) {
        let log = self.clone();
        let result = match tokio::task::spawn_blocking(move || log.append(message)).await {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other("session append task panicked")),
        };
        note_persist(&self.persist_warned, result, &self.events);
    }

    fn append(&self, message: ChatMessage) -> std::io::Result<()> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(message.clone());
        if !self.save_session {
            return Ok(());
        }
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let mut created = Session::create(&self.cwd, &slug(&self.model))?;
            // A pending name applies before the first record. It is
            // best-effort: failing here must not discard the freshly created
            // log — dropping it would make the next commit open a different
            // file and strand every message already in memory outside any
            // session.
            if let Some(name) = self
                .session_name
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                let _ = created.set_name(&name);
            }
            *guard = Some(created);
        }
        match guard.as_mut() {
            Some(s) => s.append(&message),
            None => Ok(()),
        }
    }

    /// Replace history with the compaction seed plus the kept recent
    /// messages, writing them into a fresh session file so the compacted
    /// state is itself resumable; the old file stays untouched. The fresh
    /// log is created before the old one is detached: if creation fails,
    /// the old log stays attached and later turns append to it, so a crash
    /// resumes into the complete pre-compaction conversation instead of a
    /// new file holding only an unanchored tail. Blocking session I/O —
    /// callers run it off the async task (`Agent::load_compacted`).
    fn load_compacted(&self, summary: &str, kept: Vec<ChatMessage>) -> bool {
        let seed_message = ChatMessage::user(crate::core::agent::compact::seed(summary));
        let mut fresh_history = Vec::with_capacity(kept.len() + 1);
        fresh_history.push(seed_message.clone());
        fresh_history.extend(kept);

        if !self.save_session {
            *self.history.lock().unwrap_or_else(|e| e.into_inner()) = fresh_history;
            return true;
        }

        // Same lock order as `commit`: history before session.
        let mut history_guard = self.history.lock().unwrap_or_else(|e| e.into_inner());
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let result = match Session::create(&self.cwd, &slug(&self.model)) {
            Ok(mut created) => {
                // Same best-effort pending-name application as `commit`.
                if let Some(name) = self
                    .session_name
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                {
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
    /// The process was suspended and woke within the resume window; the
    /// turn is being continued automatically over the committed partial.
    Slept {
        duration_secs: u64,
    },
    /// The process was suspended longer than the resume window; the turn
    /// stopped. Partial work is committed; this is a stop, not an error.
    SleepStopped {
        duration_secs: u64,
    },
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

#[derive(Clone, Debug)]
pub struct AgentOptions {
    pub save_session: bool,
    pub tool_mode: ToolMode,
    pub effort_override: Option<String>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        AgentOptions {
            save_session: true,
            tool_mode: ToolMode::All,
            effort_override: None,
        }
    }
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
    /// The latest observed system-sleep gap. Written by the turn's
    /// heartbeat task; tests write it through [`Agent::inject_sleep_gap`].
    wake: wake::Shared,
    options: AgentOptions,
}

impl Agent {
    pub fn new(model: Model) -> (Self, mpsc::Receiver<SessionEvent>) {
        Self::with_options(model, AgentOptions::default())
    }

    pub fn with_options(
        model: Model,
        options: AgentOptions,
    ) -> (Self, mpsc::Receiver<SessionEvent>) {
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
            wake: wake::shared(),
            options,
        };
        (agent, rx)
    }

    /// Test seam: record a sleep gap as if the heartbeat had just observed
    /// the machine wake. The turn loop attributes stream losses to it when
    /// the attempt was in flight across the gap.
    pub fn inject_sleep_gap(&mut self, duration: std::time::Duration) {
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake::SleepGap {
            duration,
            woke_at: Instant::now(),
        });
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
        let saved = self
            .options
            .effort_override
            .clone()
            .or_else(|| crate::core::config::settings::get_string("effort"));
        effort(&self.model.efforts, saved.as_deref())
    }
    /// Advance to the model's next effort level and persist it. None when
    /// the model has no reasoning knob.
    pub fn cycle_effort(&self) -> Result<Option<String>, std::io::Error> {
        if self.options.effort_override.is_some() {
            return Ok(self.effort());
        }
        let levels = self.efforts();
        if levels.is_empty() {
            return Ok(None);
        }
        let Some(current) = self.effort() else {
            return Ok(None);
        };
        let next = next_effort(&levels, current.as_str());
        crate::core::config::settings::set_string("effort", &next)?;
        Ok(Some(next))
    }
    pub fn history_snapshot(&self) -> Vec<ChatMessage> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn load_history(&self, messages: Vec<ChatMessage>) {
        *self.history.lock().unwrap_or_else(|e| e.into_inner()) = messages;
    }
    pub fn clear(&self) {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
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
            save_session: self.options.save_session,
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
    ///
    /// Runs the file I/O on the blocking pool — this is called from the
    /// TUI's own async event loop, and a multi-message session write must
    /// not stall it any more than the turn loop's own commits are allowed
    /// to (see `TurnLog::commit_async`).
    pub async fn load_compacted(&self, summary: &str, kept: Vec<ChatMessage>) -> bool {
        let log = self.log();
        let summary = summary.to_string();
        tokio::task::spawn_blocking(move || log.load_compacted(&summary, kept))
            .await
            .unwrap_or(false)
    }

    /// Attach a session log; created lazily on the first message when None.
    pub fn set_session(&self, session: Option<Session>) {
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = if self.options.save_session {
            session
        } else {
            None
        };
    }

    /// The active session's file, if a log exists yet — None before the
    /// first message is committed. `/tree` reads this file directly rather
    /// than tracking the graph in memory.
    pub fn session_path(&self) -> Option<PathBuf> {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.path().to_path_buf())
    }

    /// Rewind: point the session at an earlier node (`/tree`'s choice) and
    /// mirror the path from root to that node into in-memory history. The
    /// file itself is untouched — the next commit attaches after `head`, so
    /// the abandoned tail survives as a sibling branch, not an overwrite.
    /// Same lock order as `commit`: history before session.
    pub fn rewind_to(&self, head: Option<String>, messages: Vec<ChatMessage>) {
        let mut history_guard = self.history.lock().unwrap_or_else(|e| e.into_inner());
        let mut session_guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(session) = session_guard.as_mut() {
            session.set_head(head);
        }
        *history_guard = messages;
    }

    /// Name this session: applies immediately when a log exists, otherwise
    /// when the log is created on the first message. Either way the name is
    /// idempotent — the last one wins.
    pub fn set_session_name(&self, name: String) {
        *self.session_name.lock().unwrap_or_else(|e| e.into_inner()) = Some(name);
        if !self.options.save_session {
            return;
        }
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = guard.as_mut() {
            if let Some(name) = self
                .session_name
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                let result = s.set_name(&name);
                drop(guard);
                note_persist(&self.persist_warned, result, &self.events);
            }
        }
    }

    /// Adopt the name a resumed session carries (or clear it for a fresh
    /// one). In-memory only — the log already holds its own name entries.
    pub fn adopt_session_name(&self, name: Option<String>) {
        *self.session_name.lock().unwrap_or_else(|e| e.into_inner()) = name;
    }

    /// Prompts waiting on the running turn (steering not yet drained).
    pub fn queued_count(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The extension-set session name, if any (the derived title still
    /// exists but the name overrides it in /resume).
    pub fn session_name(&self) -> Option<String> {
        self.session_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Drop the session name — a fresh session starts unnamed.
    pub fn clear_session_name(&self) {
        *self.session_name.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
        self.submit_message(ChatMessage::user(text), system)
    }

    /// Submit a normalized user message (used for image attachments).
    /// Steering remains text-only because a running request cannot safely
    /// acquire a new binary payload halfway through its provider stream.
    pub fn submit_message(&mut self, message: ChatMessage, system: String) -> bool {
        if self.running {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(message.content);
            return true;
        }
        self.log().commit(message);
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
        let wake = self.wake.clone();
        let tool_mode = if model.supports_tools {
            self.options.tool_mode
        } else {
            ToolMode::None
        };
        let system = match tool_mode {
            ToolMode::All => system,
            ToolMode::ReadOnly => format!("{system}\n\n{}", context::read_only_notice()),
            ToolMode::None => format!("{system}\n\n{}", context::no_tools_notice()),
        };

        tokio::spawn(async move {
            // Sleep/wake detection: a one-second heartbeat records wall-vs-
            // monotonic divergence; the stream-error path consults it to
            // attribute a loss to a suspension and pick resume vs stop.
            let heartbeat_stop = Arc::new(AtomicBool::new(false));
            tokio::spawn(wake::heartbeat(
                wake.clone(),
                heartbeat_stop.clone(),
                Duration::from_secs(1),
            ));
            let window_secs = wake::policy::window_secs();
            let max_continuations = wake::policy::max_continuations();
            let mut sleep_continuations = 0u32;
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
                let steered: Vec<String> = {
                    pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .drain(..)
                        .collect()
                };
                for message in steered {
                    let _ = events.send(SessionEvent::Steered(message.clone())).await;
                    log.commit_async(ChatMessage::user(message)).await;
                }

                let mut messages = { history.lock().unwrap_or_else(|e| e.into_inner()).clone() };
                // Some compatible gateways omit usage entirely. Keep a local,
                // conservative fallback so the mid-turn safety guard still
                // exists there; a real Usage frame replaces it below. Sized
                // against history before the image strip below, so it stays
                // the conservative side of what's actually sent.
                let mut last_context = compact::estimate_request_tokens(&system, &messages);
                // A resumed session, or a mid-session model switch, can carry
                // image-bearing turns forward from an earlier, image-capable
                // model to one that isn't — this is the one place every
                // frontend's outgoing request passes through, so it's the one
                // place that needs to know. This only edits the local copy
                // just cloned from `history` above; the session's own stored
                // record keeps its images regardless of what model sends the
                // next turn.
                providers::strip_incompatible_images(&mut messages, &model);
                let request = Request {
                    model: model.clone(),
                    system: system.clone(),
                    messages,
                    effort: effort.clone(),
                    tools: tools::filter_schemas(
                        match (&host, tool_mode) {
                            // Restricted modes trust only the built-in read
                            // surface. An extension overriding a tool named
                            // `read` must not smuggle writes into --read-only.
                            (Some(h), ToolMode::All) => h.merged_tool_schemas(),
                            _ => tools::schemas(),
                        },
                        tool_mode,
                    ),
                };

                // Total provider requests made for this step, including the
                // initial request. MAX_ATTEMPTS is a request budget, not a
                // retry budget.
                let mut attempt = 1u32;
                // When this attempt's stream opened. A sleep gap whose wake
                // came after this instant happened with the attempt in
                // flight, so a loss from it is attributable to the sleep.
                let mut attempt_started = Instant::now();
                let (mut rx, mut handle) = providers::stream(clone_request(&request));

                let mut text = String::new();
                let mut calls: Vec<ToolCall> = Vec::new();
                let mut reasoning_items: Vec<String> = Vec::new();
                // Set when a mid-reply loss should resume via a continuation
                // over the committed partial; when the window or the
                // continuation cap is exhausted, the stop flag is set instead.
                let mut sleep_resume: Option<Duration> = None;
                let mut sleep_stopped = false;
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
                        ProviderEvent::ToolArgumentsDelta { delta, .. } => {
                            assembly_bytes += delta.len() as u64;
                            let _ = events
                                .send(SessionEvent::ToolCallAssembly {
                                    bytes: assembly_bytes,
                                })
                                .await;
                        }
                        ProviderEvent::ToolCallStart { .. } | ProviderEvent::ToolCallEnd { .. } => {
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
                            // A suspension that outlived the resume window
                            // stops the run whatever the provider call died
                            // of: partial work is committed below, and the
                            // stop is reported as a stop, not an error.
                            if let Some(gap) = wake::gap_since(&wake, attempt_started)
                                .filter(|gap| gap.duration.as_secs() >= window_secs)
                            {
                                let _ = events
                                    .send(SessionEvent::SleepStopped {
                                        duration_secs: gap.duration.as_secs(),
                                    })
                                    .await;
                                let _ = events
                                    .send(SessionEvent::Warning(format!(
                                        "run stopped — the device was asleep for {} (window {window_secs}s)",
                                        gap.label()
                                    )))
                                    .await;
                                handle.abort();
                                errored = true;
                                sleep_stopped = true;
                                break 'stream;
                            }
                            let nothing_produced = text.is_empty()
                                && calls.is_empty()
                                && reasoning_items.is_empty()
                                && !reasoning_streamed;
                            // The attempt was in flight across a sleep that
                            // fits the window: the run keeps going. Nothing
                            // streamed means an immediate replay — not
                            // charged to the attempt budget, since the loss
                            // was the machine's, not the provider's.
                            let slept_through = wake::gap_since(&wake, attempt_started).is_some();
                            if nothing_produced && slept_through {
                                let duration = wake::gap_since(&wake, attempt_started)
                                    .map(|gap| gap.duration.as_secs())
                                    .unwrap_or(0);
                                let _ = events
                                    .send(SessionEvent::Slept {
                                        duration_secs: duration,
                                    })
                                    .await;
                                // No artificial backoff — the machine just
                                // woke; let the connect decide.
                                attempt_started = Instant::now();
                                let (nrx, nhandle) = providers::stream(clone_request(&request));
                                rx = nrx;
                                handle = nhandle;
                                assembly_bytes = 0;
                                continue 'stream;
                            }
                            // Safe to retry only when the cause itself is
                            // retryable (never Auth or Rejected) AND nothing
                            // has streamed yet this attempt: a delivered
                            // request that already produced output or ran
                            // tools cannot be replayed without risking a
                            // duplicate.
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
                                attempt_started = Instant::now();
                                let (nrx, nhandle) = providers::stream(clone_request(&request));
                                rx = nrx;
                                handle = nhandle;
                                // A fresh attempt streams its arguments from
                                // scratch; the liveness counter follows.
                                assembly_bytes = 0;
                                continue 'stream;
                            }
                            // A mid-reply loss under the window resumes
                            // differently: the partial reply is committed
                            // below, and a continuation request lets the
                            // model finish its own sentence — bounded by the
                            // continuation cap so lid-flapping cannot chain
                            // turns unattended.
                            if !nothing_produced {
                                if let Some(gap) = wake::gap_since(&wake, attempt_started) {
                                    if sleep_continuations < max_continuations {
                                        sleep_resume = Some(gap.duration);
                                        errored = true;
                                        break 'stream;
                                    }
                                    let _ = events
                                        .send(SessionEvent::SleepStopped {
                                            duration_secs: gap.duration.as_secs(),
                                        })
                                        .await;
                                    let _ = events
                                        .send(SessionEvent::Warning(format!(
                                            "run stopped — the device was asleep for {} (continuation cap {max_continuations} reached)",
                                            gap.label()
                                        )))
                                        .await;
                                    handle.abort();
                                    // Enter the cancel-family stop path (like
                                    // the past-window branch above): commit the
                                    // partial once, synthesize results for any
                                    // unrun calls, and end the turn. Without
                                    // `errored` the stop block is skipped and
                                    // the half-streamed reply is committed as a
                                    // normal turn — running its tool calls after
                                    // the run was already reported stopped.
                                    errored = true;
                                    sleep_stopped = true;
                                    break 'stream;
                                }
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
                            log.commit_async(ChatMessage::reasoning(item)).await;
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
                        log.commit_async(ChatMessage::assistant(std::mem::take(&mut text), calls))
                            .await;
                        for call in unrun {
                            log.commit_async(ChatMessage::tool_result_with_meta(
                                call.id, note, outcome, summary,
                            ))
                            .await;
                        }
                    }
                    if stream_cancelled {
                        break true;
                    }
                    // A sleep past the window (or the continuation cap) is a
                    // stop in the cancel family: the stop line already went
                    // out, so end aborted without a second row.
                    if sleep_stopped {
                        break true;
                    }
                    // A sleep under the window resumes: the continuation
                    // message is committed and shown, and the next step asks
                    // the model to finish its own sentence.
                    if let Some(duration) = sleep_resume {
                        sleep_continuations += 1;
                        let _ = events
                            .send(SessionEvent::Slept {
                                duration_secs: duration.as_secs(),
                            })
                            .await;
                        log.commit_async(ChatMessage::user(SLEEP_CONTINUATION.to_string()))
                            .await;
                        let _ = events
                            .send(SessionEvent::Steered(SLEEP_CONTINUATION.to_string()))
                            .await;
                        continue 'turn;
                    }
                    break false;
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
                    && pending.lock().unwrap_or_else(|e| e.into_inner()).is_empty()
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
                    log.commit_async(ChatMessage::reasoning(item)).await;
                }
                // Commit the assistant turn (text + any calls).
                log.commit_async(ChatMessage::assistant(text, calls.clone()))
                    .await;

                if calls.is_empty() {
                    // A plain reply would end the turn — but a message that
                    // landed mid-reply must still be delivered, so continue the
                    // turn to pick it up rather than stranding it.
                    if !pending.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
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
                                ToolRunContext {
                                    host,
                                    tool_mode,
                                    cwd,
                                    cancel,
                                    id,
                                    events: events.clone(),
                                },
                                &call.name,
                                &call.arguments,
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
                    log.commit_async(ChatMessage::tool_result_with_meta(
                        call.id,
                        output.content,
                        output.outcome,
                        output.summary,
                    ))
                    .await;
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
                    pending.lock().unwrap_or_else(|e| e.into_inner()).insert(
                        0,
                        "The turn was paused because the context ran low. Continue the task \
                         from where it left off."
                            .into(),
                    );
                    break 'turn false;
                }
            };
            let _ = events.send(SessionEvent::TurnEnd { aborted }).await;
            heartbeat_stop.store(true, Ordering::SeqCst);
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
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect()
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
struct ToolRunContext {
    host: Option<std::sync::Arc<crate::core::api::ExtensionHost>>,
    tool_mode: ToolMode,
    cwd: PathBuf,
    cancel: Arc<AtomicBool>,
    id: u64,
    events: mpsc::Sender<SessionEvent>,
}

async fn run_tool(context: ToolRunContext, name: &str, arguments: &str) -> tools::ToolOutput {
    let ToolRunContext {
        host,
        tool_mode,
        cwd,
        cancel,
        id,
        events,
    } = context;
    if !tool_mode.allows(name) {
        let mode = match tool_mode {
            ToolMode::ReadOnly => "read-only",
            ToolMode::None => "no-tools",
            ToolMode::All => "unavailable",
        };
        return tools::ToolOutput {
            content: format!("tool blocked by {mode} mode: {name}"),
            outcome: tools::ToolOutcome::Blocked,
            summary: "blocked".into(),
            display: None,
        };
    }
    // Restricted modes are a built-in-only surface: extension schemas,
    // overrides, tools, and hooks all stay out of the execution path.
    if let (ToolMode::All, Some(h)) = (tool_mode, &host) {
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
            let (progress, mut updates) = mpsc::channel(64);
            let call = h.call_tool_streaming(name, arguments, progress);
            tokio::pin!(call);
            let result = loop {
                tokio::select! {
                    result = &mut call => break result,
                    update = updates.recv() => {
                        if let Some(update) = update {
                            forward_extension_update(&events, id, update).await;
                        }
                    }
                    _ = wait_cancelled(&cancel) => {
                        return tools::ToolOutput {
                            content: "extension tool cancelled".into(),
                            outcome: tools::ToolOutcome::Cancelled,
                            summary: "cancelled".into(),
                            display: None,
                        };
                    }
                }
            };
            // The response and the last queued update can become ready in
            // the same select tick. Preserve wire order by draining every
            // update the host accepted before publishing the final result.
            while let Ok(update) = updates.try_recv() {
                forward_extension_update(&events, id, update).await;
            }
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

async fn forward_extension_update(
    events: &mpsc::Sender<SessionEvent>,
    id: u64,
    update: crate::core::api::ToolProgress,
) {
    let chunk = tools::sanitize_display(&update.chunk);
    if !chunk.is_empty() {
        let _ = events
            .send(SessionEvent::ToolOutput {
                id,
                stream: update.stream,
                chunk,
            })
            .await;
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;

    #[test]
    fn no_save_commits_to_memory_without_opening_a_session() {
        let (agent, _events) = Agent::with_options(
            crate::core::providers::catalog::default_model(),
            AgentOptions {
                save_session: false,
                ..AgentOptions::default()
            },
        );
        agent
            .log()
            .append(ChatMessage::user("kept in memory"))
            .unwrap();

        assert_eq!(agent.history_snapshot().len(), 1);
        assert!(agent.session.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn execution_policy_blocks_a_disallowed_call_even_if_requested() {
        let (events, _rx) = mpsc::channel(1);
        let output = run_tool(
            ToolRunContext {
                host: None,
                tool_mode: ToolMode::ReadOnly,
                cwd: std::path::PathBuf::from("."),
                cancel: Arc::new(AtomicBool::new(false)),
                id: 1,
                events,
            },
            "bash",
            r#"{"command":"touch should-not-exist"}"#,
        )
        .await;

        assert_eq!(output.outcome, tools::ToolOutcome::Blocked);
        assert!(output.content.contains("read-only"));
    }
}
