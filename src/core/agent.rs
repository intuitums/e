//! The agent: one session, one event stream.
//!
//! The frontend subscribes once and receives every session event — turn
//! lifecycle, streamed deltas, usage — on a single channel, in order. Tool
//! execution events join the same union when the loop lands. Provider-level
//! events never leave this module; dialects feed the session stream.
//!
//! Terminal contract per turn: exactly one `TurnEnd`, always last, whatever
//! happened (completion, provider error, or abort).

use tokio::sync::mpsc;

use crate::core::model::{slug, Model};
use crate::core::provider::{self, ChatMessage, Event as ProviderEvent, Request};

#[derive(Debug)]
pub enum SessionEvent {
    TurnStart,
    TextDelta(String),
    ReasoningDelta(String),
    Usage { input: u64, output: u64, cache_read: u64 },
    Error(String),
    TurnEnd { aborted: bool },
}

pub struct Agent {
    pub model: Model,
    history: Vec<ChatMessage>,
    events: mpsc::Sender<SessionEvent>,
    turn: Option<TurnHandle>,
}

struct TurnHandle {
    task: tokio::task::JoinHandle<()>,
    /// Text accumulated so far, for committing a partial turn on abort.
    text: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Agent {
    /// Create the agent and the session stream the frontend consumes.
    pub fn new(model: Model) -> (Self, mpsc::Receiver<SessionEvent>) {
        let (events, rx) = mpsc::channel(256);
        (Agent { model, history: Vec::new(), events, turn: None }, rx)
    }

    pub fn is_streaming(&self) -> bool {
        self.turn.is_some()
    }

    pub fn model_slug(&self) -> String {
        slug(&self.model)
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }

    pub fn effort(&self) -> Option<String> {
        if self.model.efforts.is_empty() { None } else { Some("high".into()) }
    }

    /// Start a turn. The reply streams on the session channel; the user
    /// message is committed to history immediately, the assistant text when
    /// the turn ends (including partial text on abort).
    pub fn prompt(&mut self, text: String, system: String) {
        if self.turn.is_some() {
            let _ = self.events.try_send(SessionEvent::Error(
                "a turn is already streaming — esc to interrupt first".into(),
            ));
            return;
        }
        self.history.push(ChatMessage { role: "user".into(), content: text });

        let request = Request {
            model: self.model.clone(),
            system,
            messages: self.history.clone(),
            effort: self.effort(),
        };
        let events = self.events.clone();
        let text_cell = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let cell = text_cell.clone();

        let task = tokio::spawn(async move {
            let _ = events.send(SessionEvent::TurnStart).await;
            let (mut rx, _handle) = provider::stream(request);
            while let Some(event) = rx.recv().await {
                match event {
                    ProviderEvent::TextDelta(delta) => {
                        cell.lock().unwrap().push_str(&delta);
                        let _ = events.send(SessionEvent::TextDelta(delta)).await;
                    }
                    ProviderEvent::ReasoningDelta(delta) => {
                        let _ = events.send(SessionEvent::ReasoningDelta(delta)).await;
                    }
                    ProviderEvent::Usage { input, output, cache_read } => {
                        let _ = events.send(SessionEvent::Usage { input, output, cache_read }).await;
                    }
                    ProviderEvent::Error(message) => {
                        let _ = events.send(SessionEvent::Error(message)).await;
                        break;
                    }
                    ProviderEvent::Done => break,
                }
            }
            let _ = events.send(SessionEvent::TurnEnd { aborted: false }).await;
        });
        self.turn = Some(TurnHandle { task, text: text_cell });
    }

    /// The frontend reports each turn's end so history commits exactly once.
    pub fn on_turn_end(&mut self) {
        if let Some(turn) = self.turn.take() {
            let text = turn.text.lock().unwrap().clone();
            if !text.is_empty() {
                self.history.push(ChatMessage { role: "assistant".into(), content: text });
            }
        }
    }

    /// Abort the in-flight turn; emits the terminal `TurnEnd` itself since
    /// the task may die mid-send.
    pub fn interrupt(&mut self) {
        if let Some(turn) = &self.turn {
            turn.task.abort();
            let _ = self.events.try_send(SessionEvent::TurnEnd { aborted: true });
        }
    }
}
