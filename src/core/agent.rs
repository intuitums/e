//! The agent: one session, one event stream, the tool loop.
//!
//! A turn is: request → stream text/reasoning/tool-call events → if the model
//! called tools, run them (yolo — no gate), append results, request again;
//! repeat until a reply arrives with no tool calls. Steering messages typed
//! mid-turn are drained between steps, before the next request. The whole turn
//! emits on one ordered channel and ends with exactly one `TurnEnd`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::core::model::{slug, Model};
use crate::core::provider::{self, ChatMessage, Event as ProviderEvent, Request, ToolCall};
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

#[derive(Debug)]
pub enum SessionEvent {
    TurnStart,
    TextDelta(String),
    ReasoningDelta(String),
    /// A tool is about to run: display verb + target.
    ToolStart { id: u64, verb: String, target: String },
    /// A tool finished: its one-line summary and whether it errored.
    ToolEnd { id: u64, summary: String, is_error: bool },
    Usage { input: u64, output: u64, cache_read: u64 },
    Error(String),
    /// A transient failure is being retried.
    Retry { attempt: u32, message: String },
    /// A steering message was accepted mid-turn (for display as a user block).
    Steered(String),
    TurnEnd { aborted: bool },
}

pub struct Agent {
    pub model: Model,
    cwd: PathBuf,
    history: Arc<Mutex<Vec<ChatMessage>>>,
    events: mpsc::Sender<SessionEvent>,
    /// Steering messages queued while a turn runs.
    steer: Arc<Mutex<Vec<String>>>,
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
            cwd: std::env::current_dir().unwrap_or_default(),
            history: Arc::new(Mutex::new(Vec::new())),
            events,
            steer: Arc::new(Mutex::new(Vec::new())),
            cancel: Arc::new(AtomicBool::new(false)),
            running: false,
            session: Arc::new(Mutex::new(None)),
        };
        (agent, rx)
    }

    pub fn is_streaming(&self) -> bool {
        self.running
    }
    pub fn model_slug(&self) -> String {
        slug(&self.model)
    }
    pub fn effort(&self) -> Option<String> {
        if self.model.efforts.is_empty() { None } else { Some("high".into()) }
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

    /// Attach a session log; created lazily on the first message when None.
    pub fn set_session(&self, session: Option<Session>) {
        *self.session.lock().unwrap() = session;
    }
    pub fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }

    /// Queue a message. If a turn is running it steers (drained next step);
    /// otherwise it starts a turn.
    pub fn submit(&mut self, text: String, system: String) {
        if self.running {
            self.steer.lock().unwrap().push(text);
            return;
        }
        commit(&self.history, &self.session, &self.cwd, &self.model, ChatMessage::user(text));
        self.start(system);
    }

    fn start(&mut self, system: String) {
        self.running = true;
        self.cancel.store(false, Ordering::SeqCst);
        let events = self.events.clone();
        let history = self.history.clone();
        let steer = self.steer.clone();
        let cancel = self.cancel.clone();
        let model = self.model.clone();
        let cwd = self.cwd.clone();
        let effort = self.effort();
        let session = self.session.clone();

        tokio::spawn(async move {
            let _ = events.send(SessionEvent::TurnStart).await;
            let mut tool_seq = 0u64;
            let aborted = 'turn: loop {
                if cancel.load(Ordering::SeqCst) {
                    break true;
                }
                // Drain steering (guard dropped before any await).
                let steered: Vec<String> = { steer.lock().unwrap().drain(..).collect() };
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
                    tools: tools::schemas(),
                };

                let mut attempt = 0u32;
                let (mut rx, mut handle) = provider::stream(clone_request(&request));

                let mut text = String::new();
                let mut calls: Vec<ToolCall> = Vec::new();
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
                        ProviderEvent::Usage { input, output, cache_read } => {
                            let _ = events.send(SessionEvent::Usage { input, output, cache_read }).await;
                        }
                        ProviderEvent::Error { message, delivered } => {
                            // Only a definitely-unsent failure is safe to retry
                            // (fx's DeliveryCertainty): a delivered request may
                            // have run tools or been billed.
                            let retryable = !delivered
                                && !message.contains("no credentials")
                                && !message.contains("run /login");
                            if retryable && text.is_empty() && calls.is_empty() && attempt < 2 {
                                attempt += 1;
                                let backoff = std::time::Duration::from_millis(500 * attempt as u64);
                                let _ = events
                                    .send(SessionEvent::Retry { attempt, message: message.clone() })
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

                // Commit the assistant turn (text + any calls).
                commit(&history, &session, &cwd, &model, ChatMessage::assistant(text, calls.clone()));

                if calls.is_empty() {
                    break false; // a plain reply ends the turn
                }

                // Run each tool (yolo: no gate), append results, loop.
                for call in calls {
                    let args: serde_json::Value =
                        serde_json::from_str(&call.arguments).unwrap_or(serde_json::Value::Null);
                    let (verb, target) = tools::present(&call.name, &args);
                    tool_seq += 1;
                    let id = tool_seq;
                    let _ = events.send(SessionEvent::ToolStart { id, verb, target }).await;

                    let name = call.name.clone();
                    let arguments = call.arguments.clone();
                    let cwd2 = cwd.clone();
                    let output = tokio::task::spawn_blocking(move || tools::run(&name, &arguments, &cwd2))
                        .await
                        .unwrap_or(tools::ToolOutput {
                            content: "tool panicked".into(),
                            is_error: true,
                            summary: "error".into(),
                        });
                    let _ = events
                        .send(SessionEvent::ToolEnd { id, summary: output.summary.clone(), is_error: output.is_error })
                        .await;
                    commit(&history, &session, &cwd, &model, ChatMessage::tool_result(call.id, output.content));
                }
            };
            let _ = events.send(SessionEvent::TurnEnd { aborted }).await;
        });
    }

    /// Called by the frontend after each TurnEnd so a new turn may start.
    pub fn on_turn_end(&mut self) {
        self.running = false;
    }

    pub fn interrupt(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if !self.running {
            let _ = self.events.try_send(SessionEvent::TurnEnd { aborted: true });
        }
    }
}
