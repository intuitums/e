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
mod turn;
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
use crate::core::session::SessionLog;
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
    home: PathBuf,
    history: Arc<Mutex<Vec<ChatMessage>>>,
    session: Arc<Mutex<Option<SessionLog>>>,
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
        crate::core::config::home::with_home(self.home.clone(), || self.append_inner(message))
    }

    fn append_inner(&self, message: ChatMessage) -> std::io::Result<()> {
        // Keep the history/session commit together relative to checkpoint swaps.
        let mut history = self.history.lock().unwrap_or_else(|e| e.into_inner());
        history.push(message.clone());
        if !self.save_session {
            return Ok(());
        }
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let mut created = SessionLog::create(&self.cwd, &slug(&self.model))?;
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
    /// Installation requires the exact history supplied in `expected`.
    fn load_compacted(
        &self,
        summary: &str,
        kept: Vec<ChatMessage>,
        cancel: &AtomicBool,
        expected: &[ChatMessage],
    ) -> bool {
        crate::core::config::home::with_home(self.home.clone(), || {
            self.install_compacted(summary, kept, cancel, expected)
        })
    }

    /// Prepare the new log before checking cancellation at the commit boundary.
    fn install_compacted(
        &self,
        summary: &str,
        kept: Vec<ChatMessage>,
        cancel: &AtomicBool,
        expected: &[ChatMessage],
    ) -> bool {
        let seed_message = ChatMessage::user(crate::core::agent::compact::seed(summary));
        let mut fresh_history = Vec::with_capacity(kept.len() + 1);
        fresh_history.push(seed_message.clone());
        fresh_history.extend(kept);

        // Same lock order as `commit`: history before session.
        let mut history_guard = self.history.lock().unwrap_or_else(|e| e.into_inner());
        // A summary only describes its snapshot. Concurrent commits must survive.
        if cancel.load(Ordering::SeqCst) || expected != history_guard.as_slice() {
            return false;
        }
        if !self.save_session {
            *history_guard = fresh_history;
            return true;
        }
        let mut guard = self.session.lock().unwrap_or_else(|e| e.into_inner());
        let result = match SessionLog::create(&self.cwd, &slug(&self.model)) {
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
                if cancel.load(Ordering::SeqCst) {
                    let path = created.path().to_path_buf();
                    drop(created);
                    let _ = std::fs::remove_file(path);
                    return false;
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
        session_id: r.session_id.clone(),
        tools: r.tools.clone(),
    }
}

/// Build and install a checkpoint without exposing a partial history swap.
/// Cancellation stops the provider request; failed summaries leave the log intact.
async fn compact_log(
    log: &TurnLog,
    system: &str,
    cancel: &Arc<AtomicBool>,
) -> Result<bool, String> {
    let history = log
        .history
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let (older, kept) = compact::split(&history, log.model.context_window);
    if older.is_empty() {
        return Ok(false);
    }
    tokio::select! {
        biased;
        _ = wait_cancelled(cancel) => return Err("compaction cancelled; history was preserved".into()),
        _ = log.events.send(SessionEvent::Compacting) => {}
    }
    // Reserve completion capacity before doing work or changing history.
    // Once installed, the checkpoint can then be published without an await.
    let completion = tokio::select! {
        biased;
        _ = wait_cancelled(cancel) => return Err("compaction cancelled; history was preserved".into()),
        result = log.events.reserve() => result.map_err(|_| "session event receiver closed; history was preserved".to_string())?,
    };
    let session_id = log
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .map(|session| session.id().to_string())
        .unwrap_or_default();
    let summary = tokio::select! {
        result = compact::summarize(log.model.clone(), &older, session_id) => result?,
        _ = wait_cancelled(cancel) => return Err("compaction cancelled; history was preserved".into()),
    };
    let mut projected = vec![ChatMessage::user(compact::seed(&summary))];
    projected.extend(kept.iter().cloned());
    let tokens = compact::estimate_request_tokens(system, &projected);
    if tokens >= compact::estimate_request_tokens(system, &history)
        || compact::should_compact(tokens, log.model.context_window)
    {
        return Err("compaction did not reduce context enough; history was preserved".into());
    }
    if cancel.load(Ordering::SeqCst) {
        return Err("compaction cancelled; history was preserved".into());
    }
    let writer = log.clone();
    let checkpoint = summary.clone();
    let installation_cancel = cancel.clone();
    let installed = tokio::task::spawn_blocking(move || {
        writer.load_compacted(&checkpoint, kept, &installation_cancel, &history)
    })
    .await
    .map_err(|error| format!("compaction commit failed: {error}"))?;
    if !installed {
        if cancel.load(Ordering::SeqCst) {
            return Err("compaction cancelled; history was preserved".into());
        }
        return Err("compaction could not be installed; history changed or could not be saved; history was preserved".into());
    }
    completion.send(SessionEvent::Compacted {
        summary,
        context_tokens: tokens,
    });
    Ok(true)
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

/// Own completion and the submission race. A prompt arriving before the
/// terminal event is published is consumed by another worker in this run.
async fn supervise_turn<F>(
    mut spawn_worker: F,
    events: mpsc::Sender<SessionEvent>,
    pending: Arc<Mutex<PendingQueue>>,
    compact_requested: Arc<AtomicBool>,
) -> bool
where
    F: FnMut() -> tokio::task::JoinHandle<turn::Outcome>,
{
    let _ = events.send(SessionEvent::TurnStart).await;
    loop {
        let (aborted, failed) = match spawn_worker().await {
            Ok(outcome) => (
                outcome == turn::Outcome::Cancelled,
                outcome == turn::Outcome::Failed,
            ),
            Err(error) => {
                let failure = if error.is_panic() {
                    format!("turn worker panicked: {error}")
                } else {
                    format!("turn worker stopped unexpectedly: {error}")
                };
                let _ = events.send(SessionEvent::Error(failure)).await;
                (false, true)
            }
        };
        // Reserve before locking: publishing completion and becoming idle are
        // one transaction with submit, without blocking a Tokio worker.
        let permits = events.reserve_many(2).await;
        let mut queue = pending.lock().unwrap_or_else(|error| error.into_inner());
        if !aborted
            && !failed
            && (!queue.items.is_empty() || compact_requested.load(Ordering::SeqCst))
        {
            continue;
        }
        let discarded: Vec<String> = queue.items.drain(..).map(|(_, text)| text).collect();
        compact_requested.store(false, Ordering::SeqCst);
        queue.running = false;
        if let Ok(mut permits) = permits {
            if !discarded.is_empty() {
                if let Some(permit) = permits.next() {
                    permit.send(SessionEvent::Discarded(discarded));
                }
            }
            if let Some(permit) = permits.next() {
                permit.send(SessionEvent::TurnEnd { aborted });
            }
        }
        return aborted;
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
    /// Queued prompts that could not run after cancellation or worker failure.
    Discarded(Vec<String>),
    /// Context maintenance belongs to the core, including headless runs.
    Compacting,
    Compacted {
        summary: String,
        context_tokens: u64,
    },
    TextDelta(String),
    ReasoningDelta(String),
    /// All calls from one assistant message, known before concurrent execution.
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
    /// Explicit workspace and configuration paths for in-process callers.
    pub cwd: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub save_session: bool,
    pub tool_mode: ToolMode,
    pub effort_override: Option<String>,
    /// A positive built-in tool allowlist for this run. `None` is the full
    /// built-in and extension set. It composes under `tool_mode`, so no-tools
    /// mode always wins.
    pub allowed_tools: Option<Vec<String>>,
}

impl Default for AgentOptions {
    fn default() -> Self {
        AgentOptions {
            cwd: None,
            home: None,
            save_session: true,
            tool_mode: ToolMode::All,
            effort_override: None,
            allowed_tools: None,
        }
    }
}

#[derive(Default)]
struct PendingQueue {
    running: bool,
    next_id: u64,
    items: Vec<(u64, String)>,
}

pub struct Agent {
    home: PathBuf,
    tools: Arc<tools::ToolRuntime>,
    pub model: Model,
    /// The extension host; None means built-in tools only.
    host: Option<std::sync::Arc<crate::core::extensions::ExtensionHost>>,
    cwd: PathBuf,
    history: Arc<Mutex<Vec<ChatMessage>>>,
    events: mpsc::Sender<SessionEvent>,
    /// Messages typed while a turn runs. Entries are keyed so the frontend
    /// can edit or drop a specific one after snapshotting: the turn loop
    /// drains concurrently, and a key it already took is gone — the review
    /// commits such an entry as a fresh prompt instead of resurrecting it.
    pending: Arc<Mutex<PendingQueue>>,
    cancel: Arc<AtomicBool>,
    compact_requested: Arc<AtomicBool>,
    /// The supervisor owns the worker's terminal event. Keeping its handle
    /// prevents the turn from becoming unobserved background work.
    turn_task: Option<tokio::task::JoinHandle<()>>,
    /// The session log; every committed message is appended.
    session: Arc<Mutex<Option<SessionLog>>>,
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
        let process_cwd = std::env::current_dir().unwrap_or_default();
        let cwd = options.cwd.clone().unwrap_or_else(|| process_cwd.clone());
        let home = options
            .home
            .clone()
            .unwrap_or_else(crate::core::config::home::home);
        let cwd = if cwd.is_absolute() {
            cwd
        } else {
            process_cwd.join(cwd)
        };
        let home = if home.is_absolute() {
            home
        } else {
            process_cwd.join(home)
        };
        let agent = Agent {
            home,
            tools: Arc::new(tools::ToolRuntime::default()),
            model,
            host: None,
            cwd,
            history: Arc::new(Mutex::new(Vec::new())),
            events,
            pending: Arc::new(Mutex::new(PendingQueue::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            compact_requested: Arc::new(AtomicBool::new(false)),
            turn_task: None,
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
    pub fn set_host(&mut self, host: std::sync::Arc<crate::core::extensions::ExtensionHost>) {
        self.host = Some(host);
    }

    pub fn is_streaming(&self) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .running
    }
    pub fn model_slug(&self) -> String {
        slug(&self.model)
    }
    /// The model's declared effort levels, in order; empty when it has no
    /// reasoning knob.
    pub fn effort_levels(&self) -> Vec<String> {
        self.model.effort.clone()
    }
    /// The effort for the next request: the saved setting when this model
    /// supports it, else the model's strong default (`high` when declared,
    /// otherwise its first level).
    pub fn effort(&self) -> Option<String> {
        let saved = self.options.effort_override.clone().or_else(|| {
            crate::core::config::home::with_home(self.home.clone(), || {
                crate::core::config::settings::get_string("effort")
            })
        });
        effort(&self.model.effort, saved.as_deref())
    }
    /// Select one of the current model's declared effort levels and persist it.
    /// Returns false when this model does not accept the requested value.
    pub fn set_effort(&mut self, effort: &str) -> Result<bool, std::io::Error> {
        if !self.model.effort.iter().any(|level| level == effort) {
            return Ok(false);
        }
        crate::core::config::home::with_home(self.home.clone(), || {
            crate::core::config::settings::set_string("effort", effort)
        })?;
        // Keep the running agent in sync too. In particular, this replaces a
        // launch-time override so /effort and shift+tab take effect now rather
        // than only after e restarts.
        self.options.effort_override = Some(effort.to_string());
        Ok(true)
    }

    /// The settings panel wrote the persisted effort directly. Stop applying
    /// an older launch/runtime override so the new value takes effect now.
    pub fn use_saved_effort(&mut self) {
        self.options.effort_override = None;
    }

    /// Advance to the model's next effort level and persist it. None when
    /// the model has no reasoning knob.
    pub fn cycle_effort(&mut self) -> Result<Option<String>, std::io::Error> {
        let levels = self.effort_levels();
        if levels.is_empty() {
            return Ok(None);
        }
        let Some(current) = self.effort() else {
            return Ok(None);
        };
        let next = next_effort(&levels, current.as_str());
        if !self.set_effort(&next)? {
            return Ok(None);
        }
        Ok(Some(next))
    }
    pub fn history_snapshot(&self) -> Vec<ChatMessage> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub fn load_history(&mut self, messages: Vec<ChatMessage>) {
        self.tools = Arc::new(tools::ToolRuntime::default());
        *self.history.lock().unwrap_or_else(|e| e.into_inner()) = messages;
    }
    pub fn clear(&mut self) {
        self.tools = Arc::new(tools::ToolRuntime::default());
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// The commit handle over this agent's history, session, and warning
    /// latch — the one way messages enter the record.
    fn log(&self) -> TurnLog {
        TurnLog {
            home: self.home.clone(),
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
        let expected = self.history_snapshot();
        tokio::task::spawn_blocking(move || {
            log.load_compacted(&summary, kept, &AtomicBool::new(false), &expected)
        })
        .await
        .unwrap_or(false)
    }

    /// Attach a session log; created lazily on the first message when None.
    pub fn set_session(&self, session: Option<SessionLog>) {
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
        // Poison-contained like every other lock here: a panicked holder
        // must not cascade into panics on every later reader.
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.path().to_path_buf())
    }

    /// The active conversation's stable session id, or None before the first
    /// message creates the log. Sent to gateways that ask for a per-
    /// conversation handle (see `providers::with_attribution`).
    pub fn session_id(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.id().to_string())
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
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items
            .len()
    }

    /// Snapshot the queued prompts, oldest first, with the keys the review
    /// commit sends back. Purely a read: the turn keeps steering while the
    /// review is open.
    pub fn queue_snapshot(&self) -> Vec<(u64, String)> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items
            .clone()
    }

    /// Apply the queued-prompt review's edits. Each `(key, text)` replaces
    /// that entry in place when the turn has not drained it yet, or queues
    /// it fresh when it has — the user's edited intent, either way. Keys in
    /// `removed` drop their entry if it is still waiting. Unnamed entries
    /// are untouched.
    pub fn update_queued(&self, edits: Vec<(u64, String)>, removed: Vec<u64>) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        for (key, text) in edits {
            if let Some(entry) = pending.items.iter_mut().find(|(id, _)| *id == key) {
                entry.1 = text;
            } else {
                // Drained while the review held it: the edit is still the
                // user's intent — submit it as a fresh steering message.
                pending.next_id += 1;
                let key = pending.next_id;
                pending.items.push((key, text));
            }
        }
        for key in removed {
            pending.items.retain(|(id, _)| *id != key);
        }
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

    /// Assemble this agent's prompt using its own workspace and home.
    pub fn system_prompt(&self) -> String {
        crate::core::config::home::with_home(self.home.clone(), || {
            context::system_prompt(&self.cwd)
        })
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
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if pending.running {
            pending.next_id += 1;
            let key = pending.next_id;
            pending.items.push((key, message.content));
            drop(pending);
            return true;
        }
        pending.running = true;
        drop(pending);
        self.log().commit(message);
        self.start(system, false);
        false
    }

    /// Request a checkpoint at the next provider boundary, or immediately
    /// when idle. No frontend needs to summarize or replace history.
    pub fn request_compaction(&mut self, system: String) {
        let mut queue = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.compact_requested.store(true, Ordering::SeqCst);
        if queue.running {
            return;
        }
        queue.running = true;
        drop(queue);
        self.start(system, true);
    }

    fn start(&mut self, system: String, compact_only: bool) {
        // A detached task retains its turn's permanently cancelled token.
        self.cancel = Arc::new(AtomicBool::new(false));
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
        let compact_requested = self.compact_requested.clone();
        let tool_runtime = self.tools.clone();
        let tool_mode = if model.supports_tools {
            self.options.tool_mode
        } else {
            ToolMode::None
        };
        // The request's allowlist is shared across the turn's tool tasks.
        let allowed_tools = self.options.allowed_tools.clone().map(Arc::new);
        let system = crate::core::config::home::with_home(self.home.clone(), || {
            match (tool_mode, allowed_tools.as_deref()) {
                (ToolMode::None, _) => format!("{system}\n\n{}", context::no_tools_notice()),
                (ToolMode::All, Some(tools)) if tools.is_empty() => {
                    format!("{system}\n\n{}", context::no_tools_notice())
                }
                (ToolMode::All, Some(tools)) => {
                    format!("{system}\n\n{}", context::tool_allowlist_notice(tools))
                }
                (ToolMode::All, None) => system,
            }
        });

        // The heartbeat belongs to the supervisor too. If the turn worker
        // panics, it is stopped instead of leaking into later turns.
        let heartbeat_stop = Arc::new(AtomicBool::new(false));
        let heartbeat = tokio::spawn(wake::heartbeat(
            wake.clone(),
            heartbeat_stop.clone(),
            Duration::from_secs(1),
        ));
        let lifecycle_events = events.clone();
        let lifecycle_host = host.clone();
        let lifecycle_pending = pending.clone();
        let lifecycle_compact = compact_requested.clone();
        let context = turn::Context {
            log,
            events,
            history,
            cancel,
            model,
            cwd,
            effort,
            pending,
            host,
            tool_seq,
            wake,
            system,
            allowed_tools,
            compact_requested,
            tool_runtime,
            tool_mode,
        };
        let mut first_worker = true;
        let spawn_worker = move || {
            let compact_only = if first_worker {
                compact_only
            } else {
                context
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .items
                    .is_empty()
            };
            first_worker = false;
            tokio::spawn(crate::core::config::home::scope(
                context.log.home.clone(),
                turn::run(context.clone(), compact_only),
            ))
        };
        let turn_task = tokio::spawn(async move {
            let aborted = supervise_turn(
                spawn_worker,
                lifecycle_events,
                lifecycle_pending,
                lifecycle_compact,
            )
            .await;
            heartbeat_stop.store(true, Ordering::SeqCst);
            heartbeat.abort();
            let _ = heartbeat.await;
            if let Some(h) = &lifecycle_host {
                h.event("turn_end", serde_json::json!({"aborted": aborted}))
                    .await;
            }
        });
        self.turn_task = Some(turn_task);
    }

    pub fn interrupt(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// Dispatch one tool call: extension hooks may block it, an extension that
/// owns the name serves it, otherwise the built-in runs on a blocking thread.
struct ToolRunContext {
    tools: Arc<tools::ToolRuntime>,
    host: Option<std::sync::Arc<crate::core::extensions::ExtensionHost>>,
    tool_mode: ToolMode,
    allowed_tools: Option<Arc<Vec<String>>>,
    cwd: PathBuf,
    cancel: Arc<AtomicBool>,
    id: u64,
    events: mpsc::Sender<SessionEvent>,
}

async fn run_tool(context: ToolRunContext, name: &str, arguments: &str) -> tools::ToolOutput {
    let ToolRunContext {
        tools: tool_runtime,
        host,
        tool_mode,
        allowed_tools,
        cwd,
        cancel,
        id,
        events,
    } = context;
    if cancel.load(Ordering::SeqCst) {
        return tools::ToolOutput {
            content: "tool cancelled before execution".into(),
            outcome: tools::ToolOutcome::Cancelled,
            summary: "cancelled".into(),
            display: None,
        };
    }
    if !tool_mode.allows() {
        return tools::ToolOutput {
            content: format!("tool blocked by no-tools mode: {name}"),
            outcome: tools::ToolOutcome::Blocked,
            summary: "blocked".into(),
            display: None,
        };
    }
    // Enforce the request's list at execution too. The advertised schemas are
    // not a security boundary because a provider can still emit any tool name.
    if let Some(allowed) = &allowed_tools {
        if !allowed.iter().any(|a| a == name) {
            return tools::ToolOutput {
                content: format!("tool blocked by the request's tool allowlist: {name}"),
                outcome: tools::ToolOutcome::Blocked,
                summary: "blocked".into(),
                display: None,
            };
        }
    }
    // Hooks still guard allowlisted built-ins, but an extension cannot replace
    // one by claiming the same name.
    let builtins_only = allowed_tools.is_some();
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
        if !builtins_only && h.owns_tool(name) {
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
        tool_runtime.run_streaming(&name, &arguments, &cwd, &cancel, |stream, chunk| {
            let chunk = tools::sanitize_display(chunk);
            if !chunk.is_empty() {
                // Live output is a preview. A slow consumer must not prevent
                // the command from checking its timeout or cancellation.
                // ToolEnd carries the retained output even if preview chunks drop.
                let _ = events.try_send(SessionEvent::ToolOutput { id, stream, chunk });
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
    update: crate::core::extensions::ToolProgress,
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

    #[tokio::test]
    async fn compaction_cancels_when_either_event_slot_is_backpressured() {
        for start_blocked in [false, true] {
            let (mut agent, _events) = Agent::with_options(
                crate::core::providers::catalog::builtin_catalog().remove(0),
                AgentOptions {
                    save_session: false,
                    ..AgentOptions::default()
                },
            );
            agent.load_history(vec![
                ChatMessage::user("x".repeat(10_000)),
                ChatMessage::user("recent"),
            ]);
            let mut log = agent.log();
            log.model.context_window = 8192;
            // Capacity must be reserved before any provider request can start.
            log.model.provider = "review-unconfigured".into();
            log.model.base_url = "http://127.0.0.1:0".into();
            let (events, _receiver) = mpsc::channel(1);
            if start_blocked {
                events.try_send(SessionEvent::TurnStart).unwrap();
            }
            log.events = events;
            let cancel = Arc::new(AtomicBool::new(false));
            let compaction = compact_log(&log, "system", &cancel);
            tokio::pin!(compaction);
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut compaction)
                    .await
                    .is_err()
            );
            cancel.store(true, Ordering::SeqCst);
            let result = tokio::time::timeout(Duration::from_secs(1), compaction)
                .await
                .unwrap();
            assert_eq!(
                result.unwrap_err(),
                "compaction cancelled; history was preserved"
            );
            assert_eq!(agent.history_snapshot().len(), 2);
        }
    }

    #[test]
    fn checkpoint_installation_rejects_a_snapshot_missing_a_concurrent_commit() {
        let root = std::env::temp_dir().join(format!("e-compact-stale-{}", uuid::Uuid::new_v4()));
        for save_session in [false, true] {
            let home = root.join(save_session.to_string());
            let (agent, _events) = Agent::with_options(
                crate::core::providers::catalog::builtin_catalog().remove(0),
                AgentOptions {
                    home: Some(home.clone()),
                    cwd: Some(home),
                    save_session,
                    ..AgentOptions::default()
                },
            );
            let log = agent.log();
            log.append(ChatMessage::user("original")).unwrap();
            let snapshot = agent.history_snapshot();
            let original_path = agent.session_path();
            agent.record_user("shell output committed during summary".into());
            assert!(!log.load_compacted(
                "stale summary",
                vec![],
                &AtomicBool::new(false),
                &snapshot
            ));
            assert_eq!(
                agent.history_snapshot()[1].content,
                "shell output committed during summary"
            );
            assert_eq!(agent.session_path(), original_path);
            if let Some(path) = original_path {
                assert_eq!(
                    SessionLog::load(&path).unwrap()[1].content,
                    "shell output committed during summary"
                );
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_during_checkpoint_preparation_keeps_the_original_session() {
        let home = std::env::temp_dir().join(format!("e-compact-cancel-{}", uuid::Uuid::new_v4()));
        let (agent, _events) = Agent::with_options(
            crate::core::providers::catalog::builtin_catalog().remove(0),
            AgentOptions {
                home: Some(home.clone()),
                cwd: Some(home.clone()),
                ..AgentOptions::default()
            },
        );
        let log = agent.log();
        log.append(ChatMessage::user("original")).unwrap();
        let original = agent.session_path().unwrap();
        let expected = agent.history_snapshot();
        let directory = original.parent().unwrap();
        // Hold preparation after the new log is created, before it can commit.
        let name = log.session_name.lock().unwrap();
        let worker = log.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let task = std::thread::spawn(move || {
            worker.load_compacted("summary", vec![], &worker_cancel, &expected)
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let logs = std::fs::read_dir(directory)
                .unwrap()
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
                .count();
            if logs == 2 {
                break;
            }
            assert!(Instant::now() < deadline, "checkpoint was not staged");
            std::thread::sleep(Duration::from_millis(5));
        }
        cancel.store(true, Ordering::SeqCst);
        drop(name);
        assert!(!task.join().unwrap());
        assert_eq!(agent.session_path().unwrap(), original);
        assert_eq!(agent.history_snapshot()[0].content, "original");
        assert_eq!(
            std::fs::read_dir(directory)
                .unwrap()
                .flatten()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
                .count(),
            1
        );
        drop(agent);
        drop(log);
        std::fs::remove_dir_all(home).unwrap();
    }

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
    async fn a_panicking_turn_still_reports_one_terminal_event() {
        let (events, mut rx) = mpsc::channel(8);
        let worker = || {
            tokio::spawn(async {
                panic!("test turn panic");
            })
        };
        let aborted = supervise_turn(
            worker,
            events,
            Arc::new(Mutex::new(PendingQueue::default())),
            Arc::new(AtomicBool::new(false)),
        )
        .await;

        assert!(!aborted);
        assert!(matches!(rx.recv().await, Some(SessionEvent::TurnStart)));
        let error = rx.recv().await.expect("panic must be reported");
        assert!(
            matches!(error, SessionEvent::Error(message) if message.contains("test turn panic"))
        );
        assert!(matches!(
            rx.recv().await,
            Some(SessionEvent::TurnEnd { aborted: false })
        ));
        assert!(rx.try_recv().is_err(), "TurnEnd must be emitted once");
    }

    #[tokio::test]
    async fn a_prompt_in_the_completion_gap_is_consumed_before_turn_end() {
        let (events, mut rx) = mpsc::channel(8);
        let queue = Arc::new(Mutex::new(PendingQueue {
            running: true,
            ..PendingQueue::default()
        }));
        let pending = queue.clone();
        let calls = Arc::new(AtomicU64::new(0));
        let count = calls.clone();
        let factory = move || {
            let pending = pending.clone();
            let count = count.clone();
            tokio::spawn(async move {
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                let mut pending = pending.lock().unwrap();
                if attempt == 0 {
                    // The worker has decided to stop, but completion has not
                    // been published. A concurrent submit still belongs here.
                    pending.items.push((1, "late prompt".into()));
                } else {
                    assert_eq!(pending.items.remove(0).1, "late prompt");
                }
                turn::Outcome::Complete
            })
        };
        supervise_turn(
            factory,
            events,
            queue.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(!queue.lock().unwrap().running);
        assert!(matches!(rx.recv().await, Some(SessionEvent::TurnStart)));
        assert!(matches!(
            rx.recv().await,
            Some(SessionEvent::TurnEnd { aborted: false })
        ));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn execution_policy_blocks_a_disallowed_call_even_if_requested() {
        let (events, _rx) = mpsc::channel(1);
        let output = run_tool(
            ToolRunContext {
                tools: Arc::new(tools::ToolRuntime::default()),
                host: None,
                tool_mode: ToolMode::None,
                allowed_tools: None,
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
        assert!(output.content.contains("no-tools"));
    }

    #[tokio::test]
    async fn request_allowlist_is_enforced_at_execution() {
        let (events, _rx) = mpsc::channel(1);
        let output = run_tool(
            ToolRunContext {
                tools: Arc::new(tools::ToolRuntime::default()),
                host: None,
                tool_mode: ToolMode::All,
                allowed_tools: Some(Arc::new(vec!["read".into(), "grep".into()])),
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
        assert!(output.content.contains("tool allowlist"));
    }
}
