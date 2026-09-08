//! e-sdk — programmatic access to e's coding-agent core.
//!
//! This package is a second in-repo consumer of e's library target, behind an
//! explicit crate boundary (see `docs/decisions/0002-rust-sdk-package.md`).
//! Until the first release declares a semantic-versioning policy, this surface
//! is unstable.
//!
//! The facade drives the same harness the terminal frontend does, without a
//! terminal: build a [`Session`] against a working directory, send a prompt,
//! and consume one ordered stream of [`SessionEvent`]s — text and thinking
//! deltas, tool execution, usage, errors — the same vocabulary the frontend
//! sees. There is no side channel, matching the architecture invariant, and
//! turn lifecycle (resetting running state, compaction, continuation) stays in
//! the core: a caller just reads events and prompts again after a `TurnEnd`.
//!
//! ```no_run
//! # async fn run() -> Result<(), e_sdk::Error> {
//! use e_sdk::{Session, SessionEvent};
//!
//! let mut session = Session::builder()
//!     .cwd("/path/to/project")
//!     .save_session(false)
//!     .build()?;
//!
//! session.prompt("What files are in the current directory?");
//! while let Some(event) = session.next_event().await {
//!     match event {
//!         SessionEvent::TextDelta(delta) => print!("{delta}"),
//!         SessionEvent::TurnEnd { .. } => break,
//!         _ => {}
//!     }
//! }
//! # Ok(()) }
//! ```
//!
//! What it is not: not an extension (extensions are child processes speaking a
//! JSONL protocol to a running e — see `docs/extensions.md`), and not a daemon
//! (e stays a spawned process). This links the core into your program.

use std::path::PathBuf;

use e::core::agent::{Agent, AgentOptions};
use e::core::config::home;
use e::core::providers::catalog::{self, Model};

// The core vocabulary is re-exported so a consumer depends only on `e_sdk`,
// never on the unstable library target directly (see docs/compatibility.md).
pub use e::core::agent::SessionEvent;
pub use e::core::providers::ChatMessage;

/// What can go wrong building a session.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The requested model slug did not resolve against the catalog. The
    /// string is the slug that was asked for. (With no model requested the
    /// catalog's shipped default is used, so building never fails for lack of
    /// one — an unauthenticated run surfaces its auth failure as an `Error`
    /// event at turn time instead.)
    #[error("no such model: {0} — sign in with `e login`, or pass a slug from `e models`")]
    ModelNotFound(String),
}

/// A configured, not-yet-built session. Every field has a working default:
/// the process working directory, e's usual home, the catalog's default
/// model, a persistent session log, and the full built-in tool set.
pub struct SessionBuilder {
    cwd: Option<PathBuf>,
    home: Option<PathBuf>,
    model: Option<String>,
    system: Option<String>,
    save_session: bool,
}

impl SessionBuilder {
    /// The working directory tools run in and the session log is written to.
    /// Defaults to the process working directory.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// An explicit `~/.e`-style home for this session — credentials, config,
    /// sessions, and resources. Defaults to e's usual home; set it to run an
    /// embedding fully isolated, the same seam the test suite uses.
    pub fn home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// The model to run, by slug (`provider/model`, e.g.
    /// `anthropic/claude-opus-4-5`). Defaults to the catalog's default model.
    pub fn model(mut self, slug: impl Into<String>) -> Self {
        self.model = Some(slug.into());
        self
    }

    /// Override the system prompt. Defaults to the same prompt the frontend
    /// builds for the working directory — base instructions plus the skills
    /// catalog and any AGENTS.md the directory trusts.
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    /// Whether the conversation persists to a resumable session file under
    /// the home. Defaults to true; set false for an in-memory run.
    pub fn save_session(mut self, save: bool) -> Self {
        self.save_session = save;
        self
    }

    /// Resolve the model, build the agent, and open the event stream.
    pub fn build(self) -> Result<Session, Error> {
        // Resolve the effective home once: explicit builder home or the normal
        // configured home. All construction and later agent operations use it.
        let effective_home = self.home.unwrap_or_else(home::home);

        // Model resolution and the default system prompt both read from the
        // home (settings.json for the default model, auth.json for available
        // providers, AGENTS.md for project instructions), so run them inside
        // with_home for the effective home.
        let model = home::with_home(effective_home.clone(), || match &self.model {
            Some(slug) => catalog::resolve(slug).ok_or_else(|| Error::ModelNotFound(slug.clone())),
            None => Ok(catalog::default_model()),
        })?;

        let cwd = self
            .cwd
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        // An explicit system override is used as-is; the default prompt reads
        // the home's AGENTS.md, so it too runs inside with_home.
        let system = match self.system {
            Some(s) => s,
            None => home::with_home(effective_home.clone(), || {
                e::core::agent::context::system_prompt(&cwd)
            }),
        };

        // cwd and home ride AgentOptions — the in-process-caller seam the core
        // exposes for exactly this — so the facade needs no core changes.
        let options = AgentOptions {
            cwd: Some(cwd),
            home: Some(effective_home),
            save_session: self.save_session,
            ..AgentOptions::default()
        };
        let (agent, events) = Agent::with_options(model, options);
        Ok(Session {
            agent,
            events,
            system,
        })
    }
}

/// A live session: an agent, its ordered event stream, and the system prompt
/// used for each turn. Drive it by [`prompt`](Session::prompt)ing and reading
/// [`next_event`](Session::next_event) until a `TurnEnd`.
pub struct Session {
    agent: Agent,
    events: tokio::sync::mpsc::Receiver<SessionEvent>,
    system: String,
}

impl Session {
    /// Start configuring a session.
    pub fn builder() -> SessionBuilder {
        SessionBuilder {
            cwd: None,
            home: None,
            model: None,
            system: None,
            save_session: true,
        }
    }

    /// Submit a prompt. If no turn is running this starts one; if a turn is
    /// running it steers into it at the next step. Events for the turn arrive
    /// on [`next_event`](Session::next_event).
    pub fn prompt(&mut self, text: impl Into<String>) {
        self.agent.submit(text.into(), self.system.clone());
    }

    /// The next event in the ordered stream, or `None` once the session is
    /// dropped. A `TurnEnd` marks the end of a turn's events; the core has
    /// already reset its running state by then, so a caller can simply prompt
    /// again after seeing one.
    pub async fn next_event(&mut self) -> Option<SessionEvent> {
        self.events.recv().await
    }

    /// The model this session runs, by slug.
    pub fn model_slug(&self) -> String {
        self.agent.model_slug()
    }

    /// A snapshot of the conversation so far — every committed message.
    pub fn history(&self) -> Vec<ChatMessage> {
        self.agent.history_snapshot()
    }

    /// Request that a running turn stop at the next safe point.
    pub fn interrupt(&mut self) {
        self.agent.interrupt();
    }

    /// The underlying agent, for capabilities the facade does not yet cover
    /// (attaching an extension host, resuming a saved session). Reaching in
    /// here steps outside the stable surface — the agent's Rust items are not
    /// themselves a supported contract (see `docs/compatibility.md`).
    pub fn agent_mut(&mut self) -> &mut Agent {
        &mut self.agent
    }

    /// The model, for inspection (context window, capabilities).
    pub fn model(&self) -> &Model {
        &self.agent.model
    }
}
