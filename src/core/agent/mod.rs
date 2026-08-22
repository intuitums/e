//! The agent: one session, one event stream, the tool loop.
//!
//! A turn is: request → stream text/reasoning/tool-call events → if the model
//! called tools, run them (yolo — no gate), append results, request again;
//! repeat until a reply arrives with no tool calls. Steering messages typed
//! mid-turn are drained between steps, before the next request. The whole turn
//! emits on one ordered channel and ends with exactly one `TurnEnd`.

pub mod compact;
pub mod context;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::core::provider::catalog::{slug, Model};
use crate::core::provider::{
    self, ChatMessage, ErrorKind as ProviderErrorKind, Event as ProviderEvent, Request, ToolCall,
};
use crate::core::session::Session;
use crate::core::tools;

/// Append a message to history and to the session log, creating the log on
/// the first message.
fn commit(
    history: &Arc<Mutex<Vec<ChatMessage>>>,
    session: &Arc<Mutex<Option<Session>>>,
    cwd: &std::path::Path,
    model: &Model,
    message: ChatMessage,
) {
    history.lock().unwrap().push(message.clone());
    let mut guard = session.lock().unwrap();
    if guard.is_none() {
        *guard = Session::create(cwd, &slug(model)).ok();
    }
    if let Some(s) = guard.as_mut() {
        s.append(&message);
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
    Usage {
        input: u64,
        output: u64,
        cache_read: u64,
    },
    Error(String),
    /// A transient failure is being retried.
    Retry {
        attempt: u32,
        message: String,
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

    /// Commit a user-visible fact into history and the session log without
    /// starting a turn — the `!` shell passthrough records its output this way
    /// so the model sees what the user ran.
    pub fn record_user(&self, text: String) {
        commit(
            &self.history,
            &self.session,
            &self.cwd,
            &self.model,
            ChatMessage::user(text),
        );
    }

    /// Replace the history with the compaction seed plus the kept recent
    /// messages, detaching from the old session log — everything commits into
    /// a fresh session file, so the compacted state is itself resumable. The
    /// old file is untouched.
    pub fn load_compacted(&self, summary: &str, kept: Vec<ChatMessage>) {
        self.history.lock().unwrap().clear();
        *self.session.lock().unwrap() = None;
        commit(
            &self.history,
            &self.session,
            &self.cwd,
            &self.model,
            ChatMessage::user(crate::core::agent::compact::seed(summary)),
        );
        for message in kept {
            commit(
                &self.history,
                &self.session,
                &self.cwd,
                &self.model,
                message,
            );
        }
    }

    /// Attach a session log; created lazily on the first message when None.
    pub fn set_session(&self, session: Option<Session>) {
        *self.session.lock().unwrap() = session;
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
        commit(
            &self.history,
            &self.session,
            &self.cwd,
            &self.model,
            ChatMessage::user(text),
        );
        self.start(system);
        false
    }

    fn start(&mut self, system: String) {
        self.running = true;
        self.cancel.store(false, Ordering::SeqCst);
        let events = self.events.clone();
        let history = self.history.clone();

        let cancel = self.cancel.clone();
        let model = self.model.clone();
        let cwd = self.cwd.clone();
        let effort = self.effort();
        let session = self.session.clone();
        let pending = self.pending.clone();
        let host = self.host.clone();

        tokio::spawn(async move {
            let _ = events.send(SessionEvent::TurnStart).await;
            let mut tool_seq = 0u64;
            let aborted = 'turn: loop {
                if cancel.load(Ordering::SeqCst) {
                    break true;
                }
                // Steer: fold any pending messages into this turn between steps.
                let steered: Vec<String> = { pending.lock().unwrap().drain(..).collect() };
                for message in steered {
                    let _ = events.send(SessionEvent::Steered(message.clone())).await;
                    commit(&history, &session, &cwd, &model, ChatMessage::user(message));
                }

                let messages = { history.lock().unwrap().clone() };
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

                let mut attempt = 0u32;
                let (mut rx, mut handle) = provider::stream(clone_request(&request));

                let mut text = String::new();
                let mut calls: Vec<ToolCall> = Vec::new();
                let mut reasoning_items: Vec<String> = Vec::new();
                let mut errored = false;
                'stream: while let Some(event) = rx.recv().await {
                    if cancel.load(Ordering::SeqCst) {
                        handle.abort();
                        break 'turn true;
                    }
                    match event {
                        ProviderEvent::TextDelta(d) => {
                            text.push_str(&d);
                            let _ = events.send(SessionEvent::TextDelta(d)).await;
                        }
                        ProviderEvent::ReasoningDelta(d) => {
                            let _ = events.send(SessionEvent::ReasoningDelta(d)).await;
                        }
                        ProviderEvent::ToolCall(call) => calls.push(call),
                        ProviderEvent::ReasoningItem(item) => reasoning_items.push(item),
                        ProviderEvent::Usage {
                            input,
                            output,
                            cache_read,
                        } => {
                            let _ = events
                                .send(SessionEvent::Usage {
                                    input,
                                    output,
                                    cache_read,
                                })
                                .await;
                        }
                        ProviderEvent::Error { message, kind } => {
                            // Only a Transient (definitely-unsent) failure is
                            // safe to retry: a delivered request may have run
                            // tools or been billed; an auth failure needs a
                            // sign-in, not a retry.
                            if kind == ProviderErrorKind::Transient
                                && text.is_empty()
                                && calls.is_empty()
                                && reasoning_items.is_empty()
                                && attempt < 2
                            {
                                attempt += 1;
                                let backoff =
                                    std::time::Duration::from_millis(500 * attempt as u64);
                                let _ = events
                                    .send(SessionEvent::Retry {
                                        attempt,
                                        message: message.clone(),
                                    })
                                    .await;
                                tokio::time::sleep(backoff).await;
                                let (nrx, nhandle) = provider::stream(clone_request(&request));
                                rx = nrx;
                                handle = nhandle;
                                continue 'stream;
                            }
                            let _ = events.send(SessionEvent::Error(message)).await;
                            errored = true;
                        }
                        ProviderEvent::Done => {}
                    }
                }
                if errored {
                    break false;
                }

                // Reasoning items commit first — the dialect that produced
                // them must replay them ahead of the assistant turn.
                for item in reasoning_items.drain(..) {
                    commit(
                        &history,
                        &session,
                        &cwd,
                        &model,
                        ChatMessage {
                            role: "reasoning".into(),
                            content: item,
                            tool_calls: Vec::new(),
                            tool_call_id: None,
                            tool_meta: None,
                        },
                    );
                }
                // Commit the assistant turn (text + any calls).
                commit(
                    &history,
                    &session,
                    &cwd,
                    &model,
                    ChatMessage::assistant(text, calls.clone()),
                );

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
                    tool_seq += 1;
                    batch.push((
                        tool_seq,
                        call.clone(),
                        ToolCallPresentation {
                            id: tool_seq,
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
                                    content: output.content.clone(),
                                })
                                .await;
                            output
                        }),
                    ));
                }

                for (call, handle) in handles {
                    let output = match handle.await {
                        Ok(output) => output,
                        Err(_) => tools::ToolOutput {
                            content: "tool panicked".into(),
                            outcome: tools::ToolOutcome::Failed,
                            summary: "error".into(),
                        },
                    };
                    commit(
                        &history,
                        &session,
                        &cwd,
                        &model,
                        ChatMessage::tool_result_with_meta(
                            call.id,
                            output.content,
                            output.outcome,
                            output.summary,
                        ),
                    );
                }
                if cancel.load(Ordering::SeqCst) {
                    break 'turn true;
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
    pub fn on_turn_end(&mut self) {
        self.running = false;
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
        if let Some(reason) = h.hook_tool_call(name, arguments).await {
            return tools::ToolOutput {
                content: format!("Tool call blocked by extension: {reason}"),
                outcome: tools::ToolOutcome::Blocked,
                summary: "blocked".into(),
            };
        }
        if h.owns_tool(name) {
            let call = h.call_tool(name, arguments);
            tokio::pin!(call);
            loop {
                tokio::select! {
                    result = &mut call => {
                        let outcome = if result.is_error {
                            tools::ToolOutcome::Failed
                        } else {
                            tools::ToolOutcome::Completed
                        };
                        return tools::ToolOutput {
                            content: result.content,
                            outcome,
                            summary: if outcome.is_error() { "error".into() } else { "done".into() },
                        };
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                        if cancel.load(Ordering::SeqCst) {
                            return tools::ToolOutput {
                                content: "extension tool cancelled".into(),
                                outcome: tools::ToolOutcome::Cancelled,
                                summary: "cancelled".into(),
                            };
                        }
                    }
                }
            }
        }
    }
    let name = name.to_string();
    let arguments = arguments.to_string();
    let cwd = cwd.to_path_buf();
    tokio::task::spawn_blocking(move || {
        tools::run_streaming(&name, &arguments, &cwd, &cancel, |stream, chunk| {
            let chunk = tools::sanitize_display(chunk);
            if !chunk.is_empty() {
                let _ = events.blocking_send(SessionEvent::ToolOutput { id, stream, chunk });
            }
        })
    })
    .await
    .unwrap_or(tools::ToolOutput {
        content: "tool panicked".into(),
        outcome: tools::ToolOutcome::Failed,
        summary: "error".into(),
    })
}
