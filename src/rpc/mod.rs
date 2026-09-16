//! `e rpc`: the headless session server. JSONL over stdin and stdout, the
//! same framing the extension protocol uses — one JSON object per line,
//! requests carry an `id` and a `method`, every request gets exactly one
//! response line with that `id`, and events stream between responses tagged
//! with the `session` and `request` they belong to. A client spawns this
//! process, keeps the pipes, and speaks to it from any language; there is no
//! port, no token, and no daemon — the process lives as long as its client.
//!
//! Sessions are the unit: `session.create` builds an agent against a working
//! directory and answers with an id; `session.prompt` streams that agent's
//! events and answers with the turn's result when it ends; sessions run
//! concurrently and each one runs one turn at a time. Extensions are shared
//! by every session in the process. Their interactive `ui.*` requests reach
//! the client as `ask` lines once it has said `hello` with `ask: true`, so a
//! Slack bot can relay an extension's question to a person.
//!
//! A line without a `method` is the version-1 one-shot: `{"id", "prompt",
//! …}` runs a fresh memory-only turn and answers with the flat result
//! object, no events — older callers keep working unchanged. The contract is
//! docs/usage/automation.md; `tests/fixtures/rpc/` pins it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;

use crate::core::agent::{Agent, AgentOptions, SessionEvent};
use crate::core::cli::{self, Options, ToolMode};
use crate::core::extensions::{ExtensionHost, HostRequest};
use crate::core::providers::catalog::{self as catalog, Model, Pricing};
use crate::core::providers::{ChatMessage, ImageInput};
use crate::core::session::{self as log, SessionLog};

pub mod result;
pub use result::TurnAccumulator;

/// The protocol number `hello` reports. Additive fields and methods do not
/// change it; a change to an existing shape does.
pub const PROTOCOL: u32 = 2;

/// Bound on one request line — generous for pasted prompt text (images
/// travel as file paths, not inline bytes) but never unbounded: an
/// unterminated or malicious client must not grow this long-lived
/// process's memory without limit. Hitting it ends the loop rather than
/// skipping the line, since a still-growing line with no newline yet
/// cannot be safely resynced past.
pub const MAX_LINE_BYTES: usize = 10 * 1024 * 1024;

/// Every method, for `hello` and the docs.
pub const METHODS: &[&str] = &[
    "hello",
    "models.list",
    "session.create",
    "session.list",
    "session.info",
    "session.prompt",
    "session.steer",
    "session.interrupt",
    "session.compact",
    "session.set",
    "session.messages",
    "session.fork",
    "session.export",
    "session.close",
    "ask.reply",
    "shutdown",
];

/// The version-1 request: a flat object, `prompt` required.
#[derive(serde::Deserialize)]
struct OneShot {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    tool_mode: Option<String>,
    /// A positive tool allowlist for this turn: the turn sees only these
    /// built-ins. `None` is the full toolset; composes under `tool_mode`.
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    save: bool,
    #[serde(default)]
    images: Vec<String>,
}

/// Per-request options layered over the process defaults. A request may
/// narrow what the process allows (`--no-tools`, `--no-save`) but never
/// widen it.
fn one_shot_options(defaults: &Options, request: &OneShot) -> Result<Options, String> {
    let requested_tools = parse_tool_mode(request.tool_mode.as_deref())?;
    let mut options = defaults.clone();
    options.model = request.model.clone().or(options.model);
    options.effort = request.effort.clone().or(options.effort);
    options.no_save = defaults.no_save || !request.save;
    options.images = request.images.clone();
    options.tool_mode = defaults.tool_mode.restrict(requested_tools);
    Ok(options)
}

fn parse_tool_mode(name: Option<&str>) -> Result<ToolMode, String> {
    match name {
        None | Some("all") => Ok(ToolMode::All),
        Some("none") => Ok(ToolMode::None),
        Some(other) => Err(format!("unknown tool_mode `{other}`")),
    }
}

fn check_allowlist(tools: Option<&Vec<String>>) -> Result<(), String> {
    if let Some(unknown) = tools.and_then(|names| {
        names
            .iter()
            .find(|name| !crate::core::tools::is_builtin(name))
    }) {
        return Err(format!("unknown built-in tool in allowlist: `{unknown}`"));
    }
    Ok(())
}

/// How the turn's result line is shaped when it ends.
enum Reply {
    /// `{"id", "result": {…}}`, events streamed meanwhile.
    Session,
    /// The flat version-1 object, no events, and the session closes after.
    OneShot,
}

/// A turn in flight on one session: the request it answers and the fold of
/// its events so far.
struct Turn {
    request: Value,
    reply: Reply,
    acc: TurnAccumulator,
    model: String,
    effort: Option<String>,
    pricing: Option<Pricing>,
}

/// One open session: its agent, the options it was built with (a fork
/// inherits them), and the turn running on it, if any. Locked briefly and
/// never across an await.
struct Slot {
    id: String,
    agent: Mutex<Agent>,
    options: AgentOptions,
    turn: Mutex<Option<Turn>>,
    /// A version-1 one-shot: gone after its turn ends.
    ephemeral: bool,
}

impl Slot {
    /// Fold one event; the lines to write and whether the turn ended. The
    /// turn lock is released before the agent is consulted for the result:
    /// the prompt path takes agent then turn, so taking them the other way
    /// round here would be a deadlock waiting for a fast client.
    fn on_event(&self, event: &SessionEvent) -> (Vec<String>, bool) {
        let mut lines = Vec::new();
        let ended = matches!(event, SessionEvent::TurnEnd { .. });
        let done = {
            let mut turn = self.turn.lock().unwrap_or_else(|e| e.into_inner());
            let streams = !matches!(turn.as_ref().map(|t| &t.reply), Some(Reply::OneShot));
            if let Some(turn) = turn.as_mut() {
                turn.acc.observe(event);
            }
            if streams {
                if let Some(mut line) = event.to_json() {
                    line["session"] = Value::from(self.id.as_str());
                    line["request"] = turn
                        .as_ref()
                        .map(|t| t.request.clone())
                        .unwrap_or(Value::Null);
                    lines.push(line.to_string());
                }
            }
            if ended {
                turn.take()
            } else {
                None
            }
        };
        if let Some(mut done) = done {
            done.acc.finish();
            lines.push(self.result_line(&done));
        }
        (lines, ended)
    }

    fn result_line(&self, turn: &Turn) -> String {
        let agent = self.agent.lock().unwrap_or_else(|e| e.into_inner());
        let path = agent
            .session_path()
            .map(|p| Value::from(p.display().to_string()))
            .unwrap_or(Value::Null);
        let mut body = turn
            .acc
            .json(&turn.model, turn.effort.as_deref(), turn.pricing.as_ref());
        match turn.reply {
            Reply::OneShot => {
                body["id"] = turn.request.clone();
                // The saved session's JSONL path, when this turn persisted
                // one — the whole transcript lives there.
                body["session"] = path;
                body.to_string()
            }
            Reply::Session => {
                body["session"] = Value::from(self.id.as_str());
                body["path"] = path;
                json!({"id": turn.request, "result": body}).to_string()
            }
        }
    }

    fn info(&self) -> Value {
        let agent = self.agent.lock().unwrap_or_else(|e| e.into_inner());
        let running = agent.is_streaming()
            || self
                .turn
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some();
        json!({
            "session": self.id,
            "model": agent.model_slug(),
            "effort": agent.effort(),
            "cwd": agent.cwd().display().to_string(),
            "path": agent.session_path().map(|p| p.display().to_string()),
            "name": agent.session_name(),
            "messages": agent.history_snapshot().len(),
            "running": running,
        })
    }
}

/// The server state: open sessions, forwarded extension questions, and
/// what the client asked for in `hello`.
struct Server {
    host: Arc<ExtensionHost>,
    defaults: Options,
    out: mpsc::Sender<String>,
    /// Every turn end reports its session here: one-shots are dropped, and
    /// a draining server learns when the last turn is over.
    ended: mpsc::Sender<String>,
    sessions: HashMap<String, Arc<Slot>>,
    /// The client said `hello`: server-initiated lines (`notice`, `ask`)
    /// may flow. Before that only responses and requested events go out, so
    /// a version-1 caller never sees a line it did not ask for.
    greeted: bool,
    /// The client answers extension questions (`hello` with `ask: true`).
    ask: bool,
    asks: HashMap<u64, HostRequest>,
    next_ask: u64,
    /// Lines a synchronous method wants written after its own response.
    also: Vec<String>,
}

enum Flow {
    Continue,
    Shutdown,
}

impl Server {
    async fn emit(&self, line: String) {
        let _ = self.out.send(line).await;
    }

    async fn respond(&self, id: &Value, result: Result<Value, String>) {
        let line = match result {
            Ok(result) => json!({"id": id, "result": result}),
            Err(error) => json!({"id": id, "error": error}),
        };
        self.emit(line.to_string()).await;
    }

    async fn handle_line(&mut self, line: &str) -> Flow {
        let value: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                self.emit(
                    json!({"id": null, "error": format!("invalid request: {error}")}).to_string(),
                )
                .await;
                return Flow::Continue;
            }
        };
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            let outcome = self.one_shot(id.clone(), value);
            if let Err(error) = outcome {
                self.emit(json!({"id": id, "error": error}).to_string())
                    .await;
            }
            return Flow::Continue;
        };
        let method = method.to_string();
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        if method == "shutdown" {
            self.respond(&id, Ok(json!({}))).await;
            return Flow::Shutdown;
        }
        // Turns answer when they end, from the session's forwarder, so a
        // prompt that started has no response here.
        let immediate = match method.as_str() {
            "session.prompt" => match self.prompt(id.clone(), &params) {
                Ok(()) => None,
                Err(error) => Some(Err(error)),
            },
            "session.compact" => match self.compact(id.clone(), &params) {
                Ok(()) => None,
                Err(error) => Some(Err(error)),
            },
            _ => Some(self.dispatch(&method, &params)),
        };
        if let Some(result) = immediate {
            self.respond(&id, result).await;
        }
        for line in std::mem::take(&mut self.also) {
            self.emit(line).await;
        }
        Flow::Continue
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        match method {
            "hello" => {
                self.greeted = true;
                self.ask = params.get("ask").and_then(Value::as_bool).unwrap_or(false);
                Ok(json!({
                    "protocol": PROTOCOL,
                    "version": crate::VERSION,
                    "channel": crate::CHANNEL,
                    "commit": crate::COMMIT,
                    "cwd": std::env::current_dir().unwrap_or_default().display().to_string(),
                    "home": crate::core::config::home::home().display().to_string(),
                    "methods": METHODS,
                    "ask": self.ask,
                }))
            }
            "models.list" => Ok(models_list()),
            "session.create" => self.create(params),
            "session.list" => Ok(sessions_list(params)),
            "session.info" => Ok(self.slot(params)?.info()),
            "session.steer" => {
                let slot = self.slot(params)?;
                let text = text_param(params, "text")?;
                let mut agent = slot.agent.lock().unwrap_or_else(|e| e.into_inner());
                if agent.steer(text) {
                    Ok(json!({"held": true}))
                } else {
                    Err("no turn is running; use session.prompt".into())
                }
            }
            "session.interrupt" => {
                let slot = self.slot(params)?;
                slot.agent
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .interrupt();
                Ok(json!({}))
            }
            "session.set" => self.set(params),
            "session.messages" => {
                let slot = self.slot(params)?;
                let history = slot
                    .agent
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .history_snapshot();
                Ok(json!({"messages": history}))
            }
            "session.fork" => self.fork(params),
            "session.export" => self.export(params),
            "session.close" => {
                let id = text_param(params, "session")?;
                let slot = self
                    .sessions
                    .remove(&id)
                    .ok_or_else(|| format!("unknown session `{id}`"))?;
                slot.agent
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .interrupt();
                // A prompt still running gets its answer now: the forwarder
                // will see the agent go, not a `TurnEnd`.
                if let Some(turn) = slot.turn.lock().unwrap_or_else(|e| e.into_inner()).take() {
                    self.also
                        .push(json!({"id": turn.request, "error": "session closed"}).to_string());
                }
                Ok(json!({}))
            }
            "ask.reply" => {
                let n = params
                    .get("ask")
                    .and_then(Value::as_u64)
                    .ok_or("ask is required")?;
                let request = self
                    .asks
                    .remove(&n)
                    .ok_or_else(|| format!("no open ask {n}"))?;
                let answer = params
                    .get("result")
                    .cloned()
                    .unwrap_or_else(|| json!({"cancelled": true}));
                request.respond(Ok(answer));
                Ok(json!({}))
            }
            other => Err(format!("unknown method `{other}`")),
        }
    }

    fn slot(&self, params: &Value) -> Result<Arc<Slot>, String> {
        let id = text_param(params, "session")?;
        self.sessions
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("unknown session `{id}`"))
    }

    /// Build an agent and start forwarding its events. The forwarder owns
    /// the event receiver for the session's whole life; it ends when the
    /// agent is dropped.
    fn open(&mut self, model: Model, options: AgentOptions, ephemeral: bool) -> Arc<Slot> {
        let (mut agent, mut events) = Agent::with_options(model, options.clone());
        agent.set_host(self.host.clone());
        let id = uuid::Uuid::now_v7().to_string();
        let slot = Arc::new(Slot {
            id: id.clone(),
            agent: Mutex::new(agent),
            options,
            turn: Mutex::new(None),
            ephemeral,
        });
        self.sessions.insert(id, slot.clone());
        let out = self.out.clone();
        let ended_tx = self.ended.clone();
        // Weak on purpose: the agent owns the event sender, so a forwarder
        // that owned the agent would keep its own channel open forever.
        // When the registry drops the slot the agent goes, the channel
        // closes, and this task ends.
        let forwarder = Arc::downgrade(&slot);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let Some(slot) = forwarder.upgrade() else {
                    return;
                };
                let (lines, ended) = slot.on_event(&event);
                let ephemeral = slot.ephemeral;
                let id = slot.id.clone();
                drop(slot);
                for line in lines {
                    if out.send(line).await.is_err() {
                        return;
                    }
                }
                if ended {
                    let _ = ended_tx.send(id).await;
                    if ephemeral {
                        return;
                    }
                }
            }
        });
        slot
    }

    /// Version 1: a fresh agent for one turn, the flat result, and the
    /// session is gone — the forwarder retires it at `TurnEnd`.
    fn one_shot(&mut self, id: Value, value: Value) -> Result<(), String> {
        let request: OneShot =
            serde_json::from_value(value).map_err(|error| format!("invalid request: {error}"))?;
        if let Some(refusal) =
            crate::core::config::trust::refusal(&std::env::current_dir().unwrap_or_default())
        {
            return Err(refusal);
        }
        check_allowlist(request.tools.as_ref())?;
        let options = one_shot_options(&self.defaults, &request)?;
        if request.prompt.trim().is_empty() {
            return Err("prompt is empty".into());
        }
        let model = cli::resolve_model(&options)?;
        let images = cli::load_images(&options, &model)?;
        let mut agent_opts = cli::agent_options(&options);
        agent_opts.allowed_tools = request.tools.clone();
        let slot = self.open(model, agent_opts, true);
        let mut agent = slot.agent.lock().unwrap_or_else(|e| e.into_inner());
        *slot.turn.lock().unwrap_or_else(|e| e.into_inner()) = Some(Turn {
            request: id,
            reply: Reply::OneShot,
            acc: TurnAccumulator::with_warnings(catalog::config_warnings()),
            model: agent.model_slug(),
            effort: agent.effort(),
            pricing: agent.model.pricing.clone(),
        });
        let system = agent.system_prompt();
        agent.submit_message(
            ChatMessage::user_with_images(request.prompt, images),
            system,
        );
        Ok(())
    }

    fn create(&mut self, params: &Value) -> Result<Value, String> {
        let process_cwd = std::env::current_dir().unwrap_or_default();
        let cwd = match params.get("cwd").and_then(Value::as_str) {
            Some(path) => {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    process_cwd.join(path)
                }
            }
            None => process_cwd,
        };
        if !cwd.is_dir() {
            return Err(format!("cwd `{}` is not a directory", cwd.display()));
        }
        if let Some(refusal) = crate::core::config::trust::refusal(&cwd) {
            return Err(refusal);
        }
        let mut options = self.defaults.clone();
        if let Some(model) = params.get("model").and_then(Value::as_str) {
            options.model = Some(model.to_string());
        }
        if let Some(effort) = params.get("effort").and_then(Value::as_str) {
            options.effort = Some(effort.to_string());
        }
        let tools: Option<Vec<String>> = match params.get("tools") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                serde_json::from_value(value.clone())
                    .map_err(|_| "tools must be a list of built-in tool names".to_string())?,
            ),
        };
        check_allowlist(tools.as_ref())?;
        let requested = parse_tool_mode(params.get("tool_mode").and_then(Value::as_str))?;
        options.tool_mode = self.defaults.tool_mode.restrict(requested);
        let save = params.get("save").and_then(Value::as_bool).unwrap_or(false);
        options.no_save = self.defaults.no_save || !save;
        let model = cli::resolve_model(&options)?;
        let resume = params
            .get("resume")
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let resumed = match &resume {
            Some(path) => {
                // Only a writer takes ownership and repairs the log. A
                // memory-only resume reads the intact history without writes.
                let session = if options.no_save {
                    None
                } else {
                    Some(SessionLog::reopen(path).map_err(|e| format!("resume: {e}"))?)
                };
                let messages = SessionLog::load(path).map_err(|e| format!("resume: {e}"))?;
                Some((session, messages, log::name_of(path)))
            }
            None => None,
        };
        let mut agent_opts = cli::agent_options(&options);
        agent_opts.cwd = Some(cwd);
        agent_opts.allowed_tools = tools;
        let slot = self.open(model, agent_opts, false);
        let mut agent = slot.agent.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((session, messages, name)) = resumed {
            agent.load_history(messages);
            agent.set_session(session);
            agent.adopt_session_name(name);
        }
        if let Some(name) = params.get("name").and_then(Value::as_str) {
            agent.set_session_name(name.to_string());
        }
        Ok(json!({
            "session": slot.id,
            "model": agent.model_slug(),
            "effort": agent.effort(),
            "cwd": agent.cwd().display().to_string(),
            "path": agent.session_path().map(|p| p.display().to_string()),
        }))
    }

    /// Start a turn. The response comes from the forwarder at `TurnEnd`;
    /// an error here means nothing started.
    fn prompt(&mut self, id: Value, params: &Value) -> Result<(), String> {
        let slot = self.slot(params)?;
        let text = text_param(params, "prompt")?;
        if text.trim().is_empty() {
            return Err("prompt is empty".into());
        }
        let paths: Vec<String> = match params.get("images") {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|_| "images must be a list of file paths".to_string())?,
        };
        let mut agent = slot.agent.lock().unwrap_or_else(|e| e.into_inner());
        if !paths.is_empty() && !agent.model.image_input {
            return Err(format!(
                "model `{}` is not declared image-capable",
                agent.model_slug()
            ));
        }
        let images = ImageInput::from_paths(&paths)?;
        let mut turn = slot.turn.lock().unwrap_or_else(|e| e.into_inner());
        if agent.is_streaming() || turn.is_some() {
            return Err("a turn is running; use session.steer or session.interrupt".into());
        }
        *turn = Some(Turn {
            request: id,
            reply: Reply::Session,
            acc: TurnAccumulator::with_warnings(catalog::config_warnings()),
            model: agent.model_slug(),
            effort: agent.effort(),
            pricing: agent.model.pricing.clone(),
        });
        drop(turn);
        let system = agent.system_prompt();
        agent.submit_message(ChatMessage::user_with_images(text, images), system);
        Ok(())
    }

    /// Summarize now. Between turns only: mid-turn compaction belongs to
    /// the running turn and reports through its result.
    fn compact(&mut self, id: Value, params: &Value) -> Result<(), String> {
        let slot = self.slot(params)?;
        let focus = params
            .get("focus")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut agent = slot.agent.lock().unwrap_or_else(|e| e.into_inner());
        let mut turn = slot.turn.lock().unwrap_or_else(|e| e.into_inner());
        if agent.is_streaming() || turn.is_some() {
            return Err("a turn is running; compact between turns".into());
        }
        *turn = Some(Turn {
            request: id,
            reply: Reply::Session,
            acc: TurnAccumulator::default(),
            model: agent.model_slug(),
            effort: agent.effort(),
            pricing: agent.model.pricing.clone(),
        });
        drop(turn);
        let system = agent.system_prompt();
        agent.request_compaction_with(system, focus);
        Ok(())
    }

    /// Change the model or effort between turns. History carries over.
    fn set(&mut self, params: &Value) -> Result<Value, String> {
        let slot = self.slot(params)?;
        let mut agent = slot.agent.lock().unwrap_or_else(|e| e.into_inner());
        if agent.is_streaming() {
            return Err("a turn is running; change models between turns".into());
        }
        if let Some(query) = params.get("model").and_then(Value::as_str) {
            let model = catalog::resolve(query).ok_or_else(|| {
                format!("model `{query}` is unavailable; sign in to its provider or pick one from models.list")
            })?;
            agent.model = model;
        }
        if let Some(effort) = params.get("effort").and_then(Value::as_str) {
            if !agent.set_run_effort(effort) {
                let supported = agent.effort_levels();
                return Err(format!(
                    "model `{}` does not support effort `{effort}` (supported: {})",
                    agent.model_slug(),
                    if supported.is_empty() {
                        "none".to_string()
                    } else {
                        supported.join(", ")
                    }
                ));
            }
        }
        Ok(json!({"model": agent.model_slug(), "effort": agent.effort()}))
    }

    /// A new session carrying this one's conversation, in a file of its
    /// own when the original persists. The two grow apart from here.
    fn fork(&mut self, params: &Value) -> Result<Value, String> {
        let slot = self.slot(params)?;
        let (history, model, mut options, name, effort) = {
            let agent = slot.agent.lock().unwrap_or_else(|e| e.into_inner());
            if agent.is_streaming() {
                return Err("a turn is running; fork between turns".into());
            }
            (
                agent.history_snapshot(),
                agent.model.clone(),
                slot.options.clone(),
                agent.session_name(),
                agent.effort(),
            )
        };
        options.effort_override = effort;
        let log = if options.save_session {
            let cwd = options.cwd.clone().unwrap_or_default();
            Some(
                SessionLog::create_with(&cwd, &catalog::slug(&model), &history)
                    .map_err(|e| format!("fork: {e}"))?,
            )
        } else {
            None
        };
        let forked = self.open(model, options, false);
        let mut agent = forked.agent.lock().unwrap_or_else(|e| e.into_inner());
        agent.load_history(history);
        agent.set_session(log);
        agent.adopt_session_name(name);
        Ok(json!({
            "session": forked.id,
            "path": agent.session_path().map(|p| p.display().to_string()),
        }))
    }

    /// The conversation as one self-contained HTML page — e's stand-in for
    /// a share link: the client decides where it goes.
    fn export(&mut self, params: &Value) -> Result<Value, String> {
        let slot = self.slot(params)?;
        let agent = slot.agent.lock().unwrap_or_else(|e| e.into_inner());
        let messages = agent.history_snapshot();
        let title = agent
            .session_name()
            .or_else(|| {
                messages
                    .iter()
                    .find(|m| matches!(m.kind, crate::core::providers::MessageKind::User { .. }))
                    .map(|m| m.content.lines().next().unwrap_or_default().to_string())
            })
            .unwrap_or_else(|| "e session".to_string());
        let target = match params.get("path").and_then(Value::as_str) {
            Some(path) => {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    agent.cwd().join(path)
                }
            }
            None => {
                let short: String = slot.id.chars().take(8).collect();
                agent.cwd().join(format!("e-session-{short}.html"))
            }
        };
        let page = crate::core::export::html(&title, &agent.model_slug(), &messages);
        std::fs::write(&target, page).map_err(|e| format!("export: {e}"))?;
        Ok(json!({"path": target.display().to_string(), "title": title}))
    }

    /// An extension asked something. Display goes out as a `notice`;
    /// questions go to the client as `ask` when it answers them; the rest
    /// is refused, as every headless host refuses it.
    async fn host_request(&mut self, request: HostRequest) {
        match request.method.as_str() {
            "ui.notify" | "ui.show" => {
                if self.greeted {
                    self.emit(
                        json!({
                            "type": "notice",
                            "extension": request.extension,
                            "method": request.method,
                            "params": request.params,
                        })
                        .to_string(),
                    )
                    .await;
                }
                request.ok();
            }
            "ui.confirm" | "ui.select" | "ui.input" | "ui.editor" if self.ask => {
                self.next_ask += 1;
                let n = self.next_ask;
                self.emit(
                    json!({
                        "type": "ask",
                        "ask": n,
                        "extension": request.extension,
                        "method": request.method,
                        "params": request.params,
                    })
                    .to_string(),
                )
                .await;
                self.asks.insert(n, request);
            }
            _ => request.respond(Err("no ui".into())),
        }
    }

    /// A turn ended on `id`: a one-shot session is gone with it.
    fn turn_ended(&mut self, id: &str) {
        if self.sessions.get(id).is_some_and(|slot| slot.ephemeral) {
            self.sessions.remove(id);
        }
    }

    fn active_turns(&self) -> usize {
        self.sessions
            .values()
            .filter(|slot| {
                slot.turn
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some()
            })
            .count()
    }

    fn interrupt_all(&self) {
        for slot in self.sessions.values() {
            slot.agent
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .interrupt();
        }
    }
}

fn text_param(params: &Value, name: &str) -> Result<String, String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{name} is required"))
}

fn models_list() -> Value {
    let models = catalog::available();
    let default = (!models.is_empty()).then(|| catalog::slug(&catalog::default_model()));
    json!({
        "default": default,
        "models": models.iter().map(|m| json!({
            "model": catalog::slug(m),
            "provider": m.provider,
            "id": m.id,
            "effort": m.effort,
            "image_input": m.image_input,
            "tools": m.supports_tools,
            "context_window": m.context_window,
        })).collect::<Vec<_>>(),
    })
}

/// Saved sessions on disk, newest first — this workspace's, another
/// directory's, or every workspace's with `all`.
fn sessions_list(params: &Value) -> Value {
    let all = params.get("all").and_then(Value::as_bool).unwrap_or(false);
    let sessions = if all {
        log::list_all()
    } else {
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        log::list(&cwd)
    };
    json!({
        "sessions": sessions.iter().map(|s| json!({
            "path": s.path.display().to_string(),
            "title": s.title,
            "name": s.name,
            "modified": s.modified,
            "messages": s.message_count,
            "turns": s.user_turns,
            "cwd": s.cwd.display().to_string(),
        })).collect::<Vec<_>>(),
    })
}

#[cfg(unix)]
struct Signals {
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Self {
            terminate: signal(SignalKind::terminate())?,
            hangup: signal(SignalKind::hangup())?,
        })
    }

    async fn recv(&mut self) -> i32 {
        tokio::select! {
            _ = self.terminate.recv() => 143,
            _ = self.hangup.recv() => 129,
        }
    }
}

/// Serve until stdin closes or the client says `shutdown`. `requests` is
/// the extensions' `ui.*`/`session.*` channel and `notices` their
/// transcript notices; both are the host's, started by the caller.
pub async fn serve(
    host: Arc<ExtensionHost>,
    defaults: &Options,
    mut requests: mpsc::Receiver<HostRequest>,
    mut notices: mpsc::Receiver<String>,
) -> std::io::Result<()> {
    let (out, mut out_rx) = mpsc::channel::<String>(1024);
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
            {
                return;
            }
        }
        let _ = stdout.flush().await;
    });
    // A dedicated reader: the select below must never cancel a read
    // mid-line, and the blocking stdin handle cannot be woken for shutdown.
    let (lines_tx, mut lines) = mpsc::channel::<std::io::Result<Option<String>>>(16);
    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
        loop {
            let read =
                crate::core::extensions::read_bounded_line(&mut reader, MAX_LINE_BYTES).await;
            let done = !matches!(read, Ok(Some(_)));
            if lines_tx.send(read).await.is_err() || done {
                return;
            }
        }
    });
    let (ended, mut turns_ended) = mpsc::channel::<String>(64);
    let mut server = Server {
        host: host.clone(),
        defaults: defaults.clone(),
        out,
        ended,
        sessions: HashMap::new(),
        greeted: false,
        ask: false,
        asks: HashMap::new(),
        next_ask: 0,
        also: Vec::new(),
    };
    #[cfg(unix)]
    let mut signals = Signals::new()?;
    let mut requested_shutdown = false;
    // EOF means no more requests, not stop: a version-1 caller writes its
    // line and closes stdin at once, and its turn must still run to the
    // answer. Draining serves everything but new lines until the last turn
    // ends.
    let mut draining = false;
    loop {
        #[cfg(unix)]
        let status = tokio::select! {
            line = lines.recv(), if !draining => match line {
                Some(Ok(Some(line))) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match server.handle_line(&line).await {
                        Flow::Continue => continue,
                        Flow::Shutdown => {
                            requested_shutdown = true;
                            break;
                        }
                    }
                }
                Some(Ok(None)) | None => {
                    draining = true;
                    if server.active_turns() == 0 {
                        break;
                    }
                    continue;
                }
                Some(Err(error)) => {
                    // Fatal, same as a too-large extension line is fatal to
                    // its reader: an oversized or unterminated line leaves
                    // the stream mid-line with no safe resync point.
                    server.emit(json!({"id": null, "error": format!("invalid request: {error}")}).to_string()).await;
                    break;
                }
            },
            Some(request) = requests.recv() => {
                server.host_request(request).await;
                continue;
            }
            Some(notice) = notices.recv() => {
                if server.greeted {
                    server.emit(json!({"type": "notice", "message": notice}).to_string()).await;
                }
                continue;
            }
            Some(id) = turns_ended.recv() => {
                server.turn_ended(&id);
                if draining && server.active_turns() == 0 {
                    break;
                }
                continue;
            }
            status = signals.recv() => status,
        };
        #[cfg(not(unix))]
        let status: i32 = tokio::select! {
            line = lines.recv(), if !draining => match line {
                Some(Ok(Some(line))) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    match server.handle_line(&line).await {
                        Flow::Continue => continue,
                        Flow::Shutdown => {
                            requested_shutdown = true;
                            break;
                        }
                    }
                }
                Some(Ok(None)) | None => {
                    draining = true;
                    if server.active_turns() == 0 {
                        break;
                    }
                    continue;
                }
                Some(Err(error)) => {
                    server.emit(json!({"id": null, "error": format!("invalid request: {error}")}).to_string()).await;
                    break;
                }
            },
            Some(request) = requests.recv() => {
                server.host_request(request).await;
                continue;
            }
            Some(notice) = notices.recv() => {
                if server.greeted {
                    server.emit(json!({"type": "notice", "message": notice}).to_string()).await;
                }
                continue;
            }
            Some(id) = turns_ended.recv() => {
                server.turn_ended(&id);
                if draining && server.active_turns() == 0 {
                    break;
                }
                continue;
            }
        };
        // Signal: stop owned shell groups before extension cleanup.
        // `process::exit` is intentional — the blocking stdin reader cannot
        // be cancelled, so returning from main could hang shutdown forever.
        crate::core::tools::kill_tracked_processes();
        server.interrupt_all();
        host.shutdown().await;
        std::process::exit(status);
    }
    let by_request = requested_shutdown;
    crate::core::tools::kill_tracked_processes();
    server.interrupt_all();
    drop(server);
    host.shutdown().await;
    let _ = writer.await;
    if by_request {
        // `shutdown` arrived with stdin still open. Tokio's blocking stdin
        // reader cannot be cancelled, and a returning main would wait on it
        // forever — so leave the way the signal path does, after the flush.
        std::process::exit(0);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_shot_cannot_relax_process_safety_flags() {
        let defaults = Options {
            no_save: true,
            tool_mode: ToolMode::None,
            ..Options::default()
        };
        let request = OneShot {
            prompt: "x".into(),
            model: None,
            effort: None,
            tool_mode: Some("all".into()),
            tools: None,
            save: true,
            images: vec![],
        };
        let resolved = one_shot_options(&defaults, &request).unwrap();
        assert!(resolved.no_save);
        assert_eq!(resolved.tool_mode, ToolMode::None);
    }

    #[test]
    fn unknown_tool_mode_is_an_error() {
        assert!(parse_tool_mode(Some("some")).is_err());
        assert_eq!(parse_tool_mode(None).unwrap(), ToolMode::All);
    }
}
