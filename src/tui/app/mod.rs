//! The interactive frame: App state, key handling, and the paint loop.
//!
//! The binary (`main.rs`) owns CLI dispatch (`auth`, `ask`, `docs`, …) and
//! hands off here once a session should open.

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode,
    KeyEvent, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};
use futures::StreamExt;
use std::io::Write;
use std::time::{Duration, Instant};

use crate::core::agent::{Agent, SessionEvent};
use crate::core::output::{format_duration, format_tokens};
use crate::core::providers::catalog::{self as model, Model};
use crate::tui::authpanel::{self, AuthStage};
use crate::tui::background::stdout_is_tty;
use crate::tui::composer::{Editor, EditorResult, Key};
use crate::tui::menu::{Menu, MenuItem, MenuKind, HINT_SCOPED, HINT_USE};
use crate::tui::screen::Painter;
use crate::tui::statusline::{
    statusline, RecoveredStatus, RetryStatus, StatusData, Turn, TurnPhase, RECOVERED_VISIBLE_MS,
};
use crate::tui::theme::Theme;
use crate::tui::transcript::{Block, Kind, Transcript};
use crate::tui::trustpanel::{self, TrustStage};

mod events;
mod login;
mod menus;

/// Per-turn frontend bookkeeping; the engine state lives in the Agent.
struct ActiveTurn {
    /// The current assistant text block, if one is streaming.
    block: Option<usize>,
    text: String,
    /// The live thinking block for the current burst, if reasoning has
    /// streamed. Earlier bursts from this turn stay in the transcript and
    /// dim together at TurnEnd — this index is only the open segment.
    thinking_block: Option<usize>,
    thinking: String,
    turn: Turn,
    started: Instant,
    error: Option<String>,
    /// tool id → stable group block, so lifecycle events update in place.
    tool_blocks: std::collections::HashMap<u64, usize>,
    /// Batch members not yet terminal, including serially pending calls.
    pending_tools: usize,
}

/// The ctrl+o full-detail screen: one stored output at a time, scrollable,
/// ←/→ switching between outputs — the reference surface, e-sized.
struct Viewer {
    /// Index into App::outputs.
    index: usize,
    scroll: usize,
}

/// Asynchronous work landing back in the frame loop.
enum AppJob {
    /// A line for the transcript (login progress, extension notify…).
    Notice(String),
    /// A prompt an extension command asked to submit as the user.
    Prompt { text: String, epoch: u64 },
    /// An input hook's verdict on a submitted line: consume/replace/notice.
    InputVerdict {
        sequence: u64,
        text: String,
        verdict: crate::core::api::InputVerdict,
    },
    /// An extension named the session (command result). Tagged with the
    /// session epoch the command started in.
    Rename { name: String, epoch: u64 },
    /// A finished /compact: the summary and the recent messages kept verbatim.
    Compacted {
        summary: String,
        kept: Vec<crate::core::providers::ChatMessage>,
    },
    /// A /compact that didn't produce a summary.
    CompactFailed(String),
    /// A finished `!` shell command: what ran and what it printed. Tagged
    /// with the session epoch it started in.
    Shell {
        cmd: String,
        output: crate::core::tools::ToolOutput,
        epoch: u64,
    },
    /// A /reload finished: the restarted extension host.
    Reloaded(std::sync::Arc<crate::core::api::ExtensionHost>),
    /// The background updater installed a new version.
    Updated(String),
    /// A provider model-list refresh finished; rebuild an open picker.
    CatalogRefreshed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputRoute {
    ApiKey,
    Hook,
    Direct,
}

fn input_route(awaiting_api_key: bool, has_input_hook: bool) -> InputRoute {
    match (awaiting_api_key, has_input_hook) {
        (true, _) => InputRoute::ApiKey,
        (false, true) => InputRoute::Hook,
        (false, false) => InputRoute::Direct,
    }
}

/// Input hooks run concurrently so a slow extension does not block the frame,
/// but their verdicts must be applied in submission order. Otherwise a fast
/// second line can overtake a slow first one and reverse the conversation.
#[derive(Default)]
struct PendingInputVerdicts {
    next_sequence: u64,
    next_to_apply: u64,
    ready: std::collections::BTreeMap<u64, (String, crate::core::api::InputVerdict)>,
}

impl PendingInputVerdicts {
    fn reserve(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        sequence
    }

    fn complete(
        &mut self,
        sequence: u64,
        text: String,
        verdict: crate::core::api::InputVerdict,
    ) -> Vec<(String, crate::core::api::InputVerdict)> {
        self.ready.insert(sequence, (text, verdict));
        let mut ordered = Vec::new();
        while let Some(item) = self.ready.remove(&self.next_to_apply) {
            ordered.push(item);
            self.next_to_apply += 1;
        }
        ordered
    }
}

struct ActiveLogin {
    flow_id: u64,
    cancellation: crate::core::auth::login::LoginCancellation,
    task: tokio::task::JoinHandle<()>,
    wait_for_callback: bool,
}

impl Drop for ActiveLogin {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
        if self.wait_for_callback {
            crate::core::auth::login::wait_for_callback_release();
        }
    }
}

struct App {
    theme: Theme,
    transcript: Transcript,
    editor: Editor,
    agent: Agent,
    active: Option<ActiveTurn>,
    overlay: Option<String>,
    armed_at: Option<Instant>,
    should_quit: bool,
    /// Prompt-side tokens of the latest request ≈ current context size.
    context_tokens: u64,
    /// A provider awaiting a pasted API key; the next submit is the secret.
    pending_key: Option<String>,
    /// The open picker, if any — commands, files, models.
    menu: Option<Menu>,
    /// The sign-in panel, when /login is active.
    auth: Option<AuthStage>,
    /// The settings panel, when /settings is active.
    settings: Option<crate::tui::settingspanel::SettingsPanel>,
    /// Whether streamed thinking is drawn (the `show_thinking` setting,
    /// default on). Gating only the drawing — the ↓ token estimate always
    /// counts reasoning.
    show_thinking: bool,
    /// Background job narration (login flows) into the transcript.
    jobs: tokio::sync::mpsc::Sender<String>,
    /// How a login flow ended; control flow reads this, never the notices.
    logins: tokio::sync::mpsc::Sender<crate::core::auth::login::Outcome>,
    /// The owned OAuth task; dropping it cancels polling and callback waits.
    login_task: Option<ActiveLogin>,
    /// Monotonic identity used to ignore a canceled flow's queued outcome.
    login_sequence: u64,
    /// Extension host; commands and prompts come back on `results`.
    host: std::sync::Arc<crate::core::api::ExtensionHost>,
    results: tokio::sync::mpsc::Sender<AppJob>,
    /// Completed input-hook calls waiting for earlier submissions to finish.
    input_verdicts: PendingInputVerdicts,
    /// A /compact summary is being generated; cleared when it lands or fails.
    compacting: bool,
    /// /compact was asked for mid-turn; runs when the turn ends (the
    /// reference behavior — compaction never touches a running turn).
    compact_requested: bool,
    /// Messages typed while compacting; submitted once the swap lands.
    held_prompts: Vec<String>,
    /// First visit to this directory: the trust question, until answered.
    trust: Option<TrustStage>,
    /// A command-line prompt held until the first-visit trust choice is
    /// persisted, so its system prompt reflects that choice.
    pending_initial: Option<String>,
    /// Transcript index of the running `!` block, updated on completion.
    shell_block: Option<usize>,
    /// A /reload is restarting the extension host; prompts are held.
    reloading: bool,
    /// Transcript index of the reload notice, replaced when reload finishes.
    reload_block: Option<usize>,
    /// Full tool outputs for the ctrl+o viewer: (title, content), newest
    /// last, capped.
    outputs: Vec<(String, String)>,
    /// The ctrl+o full-detail viewer, when open.
    viewer: Option<Viewer>,
    /// Bumped whenever session identity changes (/new, resume). Async work
    /// launched in one epoch may not mutate a later one: a late extension
    /// command or shell result carries the epoch it started in and is
    /// dropped on mismatch.
    session_epoch: u64,
    /// A new version is installed on disk; /reload switches to it.
    update_installed: Option<String>,
    /// Exit the loop and exec the (updated) binary with -c.
    relaunch: bool,
    /// OSC-11 background detection, probed once at startup before the
    /// event stream owns stdin. Re-probing mid-session would block the
    /// loop and swallow keystrokes, so a changed terminal background
    /// applies on restart.
    light_background: bool,
    /// Cached statusline inputs. Deriving them reads `~/.e/auth.json` and
    /// `~/.e/settings.json`; doing that per frame stalls streaming, so
    /// they refresh only via `refresh_status_cache`.
    signed_in: bool,
    status_effort: Option<String>,
}

impl App {
    /// The full-detail screen: title, a scroll window, the reference's
    /// footer wording.
    fn viewer_frame(&self, width: usize, height: usize) -> Vec<String> {
        let Some(viewer) = &self.viewer else {
            return Vec::new();
        };
        let Some((title, content)) = self.outputs.get(viewer.index) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        rows.push(format!(
            "{}{}",
            crate::tui::render::bold(title),
            self.theme.fg(
                "muted",
                &format!("  ({}/{})", viewer.index + 1, self.outputs.len())
            )
        ));
        rows.push(String::new());
        let body: Vec<&str> = content.lines().collect();
        let window = height.saturating_sub(4).max(1);
        for line in body.iter().skip(viewer.scroll).take(window) {
            rows.push(crate::tui::markdown::clip_styled(line, width));
        }
        while rows.len() < height.saturating_sub(1) {
            rows.push(String::new());
        }
        rows.push(self.theme.fg(
            "statusline",
            "Full detail · ←/→ switch · ctrl o close · ↑↓ scroll · Esc close",
        ));
        rows
    }

    fn frame(&mut self, width: usize) -> Vec<String> {
        let blink_on = self
            .active
            .as_ref()
            .map(|turn| (turn.started.elapsed().as_millis() / 500) % 2 == 0)
            .unwrap_or(true);
        let mut lines = self
            .transcript
            .render_animated(&self.theme, width, blink_on);
        if let Some(s) = &self.active {
            if let Some(label) = s.turn.label(s.started.elapsed().as_secs()) {
                lines.push(String::new());
                if s.turn.recovered.is_some() {
                    // A brief, non-blinking confirmation — not an ongoing
                    // wait, so no dot animation.
                    lines.push(self.theme.fg("success", &format!("✓ {label}")));
                } else if s.turn.phase == TurnPhase::Retrying {
                    // Same blinking dot as Thinking — still an in-progress
                    // wait — toned as a warning so a struggling provider
                    // reads distinctly from ordinary thinking.
                    let dot = if blink_on { "•" } else { " " };
                    lines.push(self.theme.fg("warning", &format!("{dot} {label}")));
                } else if s.turn.phase == TurnPhase::Thinking {
                    // The reference runs the activity dot on the same column
                    // as the user rail — flush left, no indent. The blink is
                    // presence, not color: the dot shows and hides, no dim
                    // half-state between.
                    let line = if blink_on {
                        format!("{} {label}", self.theme.fg("userMessageText", "•"))
                    } else {
                        format!("  {label}")
                    };
                    lines.push(line);
                } else {
                    lines.push(label);
                }
            }
        }
        let entering_key = matches!(self.auth, Some(AuthStage::ApiKey { .. }));
        if !entering_key {
            lines.extend(self.editor.render(&self.theme, width));
        }
        if let Some(stage) = &self.trust {
            let dir = self.agent.cwd().to_string_lossy().into_owned();
            lines.extend(trustpanel::render(stage, &self.theme, width, &dir));
        } else if let Some(stage) = &self.auth {
            lines.extend(authpanel::render(
                stage,
                &self.theme,
                width,
                self.editor.text().chars().count(),
            ));
        } else if let Some(panel) = &self.settings {
            lines.extend(panel.render(&self.theme, width));
        } else if let Some(menu) = &self.menu {
            lines.extend(menu.render(&self.theme, width));
        }
        let window = self.agent.model.context_window.max(1);
        let percent = ((self.context_tokens.saturating_mul(100)) / window).min(100) as u8;
        // Nothing is signed in for the current model — it's a bootstrap
        // placeholder, not something the user chose, so don't show it.
        let signed_in = self.signed_in;
        let data = StatusData {
            model: signed_in.then(|| self.agent.model_slug()),
            effort: signed_in.then(|| self.status_effort.clone()).flatten(),
            session_name: self.agent.session_name(),
            context_percent: Some(percent),
            queued: self.agent.queued_count() + self.held_prompts.len(),
        };
        let hint = self
            .settings
            .as_ref()
            .map(|_| crate::tui::settingspanel::HINT)
            .or_else(|| self.menu.as_ref().map(|m| m.hint));
        lines.extend(statusline(
            &self.theme,
            &data,
            self.overlay.as_deref(),
            hint,
            width,
        ));
        lines
    }

    /* ---------- pickers ---------- */

    /// ctrl+p / ctrl+shift+p: cycle through the scope (or all available
    /// models when no scope is set), persisting the switch. The statusline is
    /// the feedback — it shows the new model immediately.
    fn cycle_model(&mut self, forward: bool) {
        let pool = model::cycle_pool();
        if pool.len() <= 1 {
            let scoped = model::scope().map(|s| !s.is_empty()).unwrap_or(false);
            self.notice(
                if scoped {
                    "only one model in scope"
                } else {
                    "only one model available"
                }
                .into(),
            );
            return;
        }
        let current = self.agent.model_slug();
        let idx = pool
            .iter()
            .position(|m| model::slug(m) == current)
            .unwrap_or(0);
        let next = if forward {
            (idx + 1) % pool.len()
        } else {
            (idx + pool.len() - 1) % pool.len()
        };
        persist_model(&pool[next]);
        self.agent.model = pool[next].clone();
        self.refresh_status_cache();
    }

    fn open_resume_menu(&mut self) {
        // Both checks: `active` covers a turn whose TurnStart has been seen,
        // `is_streaming` covers the gap between submit and that event.
        if self.active.is_some() || self.agent.is_streaming() {
            self.notice("a turn is running — press Esc to stop it, then /resume".into());
            return;
        }
        let cwd = self.agent.cwd();
        let items: Vec<MenuItem> = crate::core::session::list(&cwd)
            .into_iter()
            .map(|info| {
                let mut item = MenuItem::new(
                    if info.title.is_empty() {
                        "(untitled)"
                    } else {
                        &info.title
                    },
                    "",
                    &info.path.to_string_lossy(),
                );
                item.meta = format!("{} · {} msgs", ago(info.modified), info.message_count);
                item
            })
            .collect();
        if items.is_empty() {
            self.notice("no saved sessions for this workspace".into());
            return;
        }
        self.menu = Some(Menu::new(MenuKind::Sessions, "Sessions", HINT_USE, items));
    }

    fn resume_recent(&mut self) {
        let cwd = self.agent.cwd();
        match crate::core::session::most_recent(&cwd) {
            Some(path) => self.resume_path(path),
            None => self.notice("no saved sessions for this workspace".into()),
        }
    }

    fn resume_path(&mut self, path: std::path::PathBuf) {
        // The picker can already be open when a turn starts (queued prompt);
        // re-check here so a selection can never splice a running turn's
        // output into the resumed session. `is_streaming` also covers the
        // gap between a submit and its TurnStart event.
        if self.active.is_some() || self.agent.is_streaming() {
            self.notice("a turn is running — press Esc to stop it, then resume".into());
            return;
        }
        let messages = match crate::core::session::Session::load(&path) {
            Ok(m) => m,
            Err(e) => {
                self.notice(format!("could not open session: {e}"));
                self.release_initial_prompt();
                return;
            }
        };
        // Ownership first: a session another e is appending to must not be
        // replayed into a second, diverging history.
        let session = match crate::core::session::Session::reopen(&path) {
            Ok(s) => s,
            Err(e) => {
                self.notice(format!("could not resume session: {e}"));
                self.release_initial_prompt();
                return;
            }
        };
        self.transcript.clear();
        self.transcript
            .push(Block::new(Kind::Banner, crate::VERSION));
        // The old transcript's shell block index and held prompts die with
        // it; a still-running `!` command's result is epoch-discarded, and a
        // /compact still summarizing the old session must not land its swap
        // on the resumed one.
        self.shell_block = None;
        self.held_prompts.clear();
        self.compacting = false;
        self.compact_requested = false;
        let mut restored_calls = std::collections::HashMap::<String, (usize, u64)>::new();
        let mut restored_id = 0u64;
        for m in &messages {
            match m.role.as_str() {
                "user" => {
                    self.transcript
                        .push(Block::new(Kind::User, m.content.clone()));
                }
                "assistant" => {
                    if !m.content.trim().is_empty() {
                        self.transcript
                            .push(Block::new(Kind::Assistant, m.content.clone()));
                    }
                    if !m.tool_calls.is_empty() {
                        let mut children = Vec::with_capacity(m.tool_calls.len());
                        let mut ids = Vec::with_capacity(m.tool_calls.len());
                        for call in &m.tool_calls {
                            restored_id += 1;
                            let args = serde_json::from_str(&call.arguments)
                                .unwrap_or(serde_json::Value::Null);
                            let shown = crate::core::tools::present(&call.name, &args);
                            children.push(crate::tui::transcript::ToolChild::pending(
                                restored_id,
                                shown.category,
                                shown.running,
                                shown.completed,
                                shown.target,
                            ));
                            ids.push((call.id.clone(), restored_id));
                        }
                        let block = self.transcript.push(Block::tool_group(children));
                        for (call_id, id) in ids {
                            restored_calls.insert(call_id, (block, id));
                        }
                    }
                }
                "tool" => {
                    let Some(call_id) = m.tool_call_id.as_ref() else {
                        continue;
                    };
                    let Some(&(block, id)) = restored_calls.get(call_id) else {
                        continue;
                    };
                    let (outcome, summary) = m
                        .tool_meta
                        .as_ref()
                        .map(|meta| (meta.outcome, meta.summary.clone()))
                        .unwrap_or((crate::core::tools::ToolOutcome::Completed, "done".into()));
                    if let Some(group) = self.transcript.blocks.get_mut(block) {
                        group.start_tool(id);
                        group.finish_tool(id, outcome, summary, &m.content);
                    }
                }
                _ => {}
            }
        }
        // Seed the context gauge from the restored history so the statusline
        // and the auto-compact check don't see an empty context until the
        // first real usage report lands.
        let seeded: usize = messages.iter().map(|m| m.content.chars().count()).sum();
        self.context_tokens = (seeded / 4) as u64;
        self.agent.load_history(messages);
        self.agent.set_session(Some(session));
        // Identity travels together: the resumed log's persisted name
        // replaces whatever the previous session was called.
        self.agent
            .adopt_session_name(crate::core::session::name_of(&path));
        self.session_epoch += 1;
        self.notice(format!(
            "resumed {}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        // A -r launch prompt waits for this selection; deliver it against
        // the loaded history.
        if let Some(initial) = self.pending_initial.take() {
            self.submit(initial);
        }
    }

    fn open_settings(&mut self) {
        self.menu = None;
        self.settings = Some(crate::tui::settingspanel::SettingsPanel::new(
            self.agent.efforts(),
        ));
    }

    fn dispatch_command(&mut self, command: String) {
        match command.as_str() {
            "/login" => self.open_login_menu(),
            "/models" | "/model" => self.open_model_menu(),
            "/scoped-models" => self.open_scoped_menu(),
            "/reload" => self.reload(),
            "/settings" => self.open_settings(),
            "/resume" => self.open_resume_menu(),
            "/copy" => self.copy_last(),
            other => self.submit(other.to_string()),
        }
    }

    fn submit(&mut self, text: String) {
        let text = self.editor.expand_pastes(&text);
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }

        let route = input_route(self.pending_key.is_some(), self.host.has_input_hook());
        // API keys are consumed before extension dispatch, matching the
        // documented boundary that secrets never reach input hooks.
        if route == InputRoute::ApiKey {
            self.submit_api_key(&trimmed);
            return;
        }

        // An input hook can consume or rewrite the line before anything else
        // sees it. Completed calls are applied in submission order below.
        if route == InputRoute::Hook {
            let host = self.host.clone();
            let results = self.results.clone();
            let sequence = self.input_verdicts.reserve();
            tokio::spawn(async move {
                let verdict = host.hook_input(&trimmed).await;
                let _ = results
                    .send(AppJob::InputVerdict {
                        sequence,
                        text,
                        verdict,
                    })
                    .await;
            });
            return;
        }
        self.submit_direct(trimmed);
    }

    fn apply_input_verdict(&mut self, text: String, verdict: crate::core::api::InputVerdict) {
        if let Some(notice) = verdict.notice.filter(|n| !n.trim().is_empty()) {
            self.notice(notice);
        }
        if verdict.consume {
            // Swallowed entirely — nothing reaches the agent.
        } else if let Some(replace) = verdict.replace {
            // The extension rewrote the line; it already saw the original, so
            // no second hook pass.
            self.submit_direct(replace);
        } else {
            // Allowed through — the hook already saw the text, so submit
            // directly. Re-running submit() here would loop through the hook.
            self.submit_direct(text);
        }
    }

    /// The real submit flow, after the input hook (if any) has had its say.
    fn submit_direct(&mut self, text: String) {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.editor.push_history(text);

        // `!cmd` runs in the shell directly; the output lands in the
        // transcript and in history, so the model sees what the user did.
        if let Some(cmd) = trimmed.strip_prefix('!').map(str::trim) {
            if !cmd.is_empty() {
                self.run_shell(cmd.to_string());
                return;
            }
        }

        if let Some(rest) = command_arg(&trimmed, "/login") {
            let provider = rest.trim().to_string();
            if provider.is_empty() {
                self.open_login_menu();
            } else {
                self.login(provider);
            }
            return;
        }

        if trimmed == "/scoped-models" {
            self.open_scoped_menu();
            return;
        }
        let model_rest =
            command_arg(&trimmed, "/models").or_else(|| command_arg(&trimmed, "/model"));
        if let Some(rest) = model_rest {
            let query = rest.trim();
            if query.is_empty() {
                self.open_model_menu();
            } else if let Some(found) = model::resolve(query) {
                persist_model(&found);
                self.notice(format!("model set to {}", model::slug(&found)));
                self.agent.model = found;
                self.refresh_status_cache();
            } else {
                self.notice(format!(
                    "no available model matches {query:?} — sign in to its provider with /login"
                ));
            }
            return;
        }
        match trimmed.as_str() {
            "/quit" | "/exit" => self.should_quit = true,
            "/version" => self.notice(format!("e {}", crate::VERSION)),
            "/help" => {
                let mut help = "commands:\n  /login [provider]   sign in (API key or account)\n  /models [name]      list or switch models\n  /scoped-models      choose which models ctrl+p cycles\n  /reload             reload extensions, themes, and config\n  /new                fresh session\n  /compact            summarize into a fresh session\n  /trust              trust this directory (loads its AGENTS.md, .e skills and prompts)\n  ! <cmd>              run a shell command; the model sees the output\n  shift+tab           cycle reasoning effort (per model)\n  /version            show the version\n  /quit               exit".to_string();
                let ext_commands = self.host.commands();
                if !ext_commands.is_empty() {
                    help.push_str("\n\nextension commands:");
                    for (name, description) in ext_commands {
                        help.push_str(&format!("\n  /{name:<21}{description}"));
                    }
                }
                self.notice(help);
            }
            "/new" | "/clear" => {
                // A running turn owns the history and session log; replacing
                // them mid-turn would commit its reply into the wrong
                // session. `is_streaming` also covers the gap between a
                // submit and its TurnStart event.
                if self.active.is_some() || self.agent.is_streaming() {
                    self.notice("a turn is running — press Esc to stop it, then /new".into());
                    return;
                }
                self.compacting = false;
                self.compact_requested = false;
                self.held_prompts.clear();
                self.shell_block = None;
                self.reload_block = None;
                self.context_tokens = 0;
                self.agent.clear();
                self.agent.clear_session_name();
                self.agent.set_session(None);
                // The name is part of session identity: a fresh session must
                // not inherit the old one's.
                self.agent.adopt_session_name(None);
                self.session_epoch += 1;
                self.transcript.clear();
                self.transcript
                    .push(Block::new(Kind::Banner, crate::VERSION));
                set_tab_title(&tab_title(&title_path(), None));
            }
            "/resume" => self.open_resume_menu(),
            "/settings" => self.open_settings(),
            "/copy" => self.copy_last(),
            "/compact" => self.compact_now(),
            "/reload" => self.reload(),
            "/trust" => match crate::core::config::trust::set(&self.agent.cwd(), true) {
                Ok(()) => self.notice(
                    "directory trusted — its AGENTS.md and .e skills/prompts now load".into(),
                ),
                Err(e) => self.notice(format!("trust: {e}")),
            },
            _ if trimmed.starts_with('/') => {
                let (name, args) = trimmed[1..].split_once(' ').unwrap_or((&trimmed[1..], ""));
                if let Some(template) =
                    crate::core::resources::prompts::find(name, &self.agent.cwd())
                {
                    let expanded =
                        crate::core::resources::prompts::substitute(&template.content, args);
                    self.prompt(expanded);
                } else if self.host.has_command(name) {
                    let host = self.host.clone();
                    let results = self.results.clone();
                    let (name, args) = (name.to_string(), args.to_string());
                    let epoch = self.session_epoch;
                    tokio::spawn(async move {
                        let out = host.run_command(&name, &args).await;
                        if let Some(notice) = out.notice {
                            let _ = results.send(AppJob::Notice(notice)).await;
                        }
                        if let Some(text) = out.prompt {
                            let _ = results.send(AppJob::Prompt { text, epoch }).await;
                        }
                        if let Some(name) = out.session_name.filter(|n| !n.trim().is_empty()) {
                            let _ = results.send(AppJob::Rename { name, epoch }).await;
                        }
                    });
                } else {
                    self.notice(format!("unknown command {trimmed}"));
                }
            }
            _ => self.prompt(trimmed),
        }
    }

    /// Deliver a held -r launch prompt into the current session — the pick
    /// it was waiting for fell through (declined, or the resume failed), and
    /// stranding it would silently drop typed text. The trust question, if
    /// still open, keeps holding it.
    fn release_initial_prompt(&mut self) {
        if self.trust.is_none() {
            if let Some(initial) = self.pending_initial.take() {
                self.submit(initial);
            }
        }
    }

    fn prompt(&mut self, text: String) {
        // While compacting, reloading, or a `!` shell command is running,
        // hold the message; it submits (and displays) when the block lifts —
        // a turn must not start without the shell output it was promised.
        if self.compacting || self.reloading || self.shell_block.is_some() {
            self.held_prompts.push(text);
            return;
        }
        // While a turn runs the message is held and steered in (echoed later
        // via Steered); idle, it begins a turn now.
        let held = self.agent.submit(text.clone(), system_prompt());
        if !held {
            self.transcript.push(Block::new(Kind::User, text));
        }
    }

    /// /compact: mid-turn it is deferred to TurnEnd (compaction never touches
    /// a running turn); idle it starts now.
    fn compact_now(&mut self) {
        if self.agent.is_streaming() {
            if !self.compact_requested {
                self.compact_requested = true;
                self.notice("will compact when this turn ends".into());
            }
            return;
        }
        self.start_compaction(false);
    }

    /// Summarize everything before the keep-recent cut, off-task; the swap
    /// lands via AppJob::Compacted. `auto` softens the too-small notice.
    fn start_compaction(&mut self, auto: bool) {
        if self.compacting {
            if !auto {
                self.notice("already compacting".into());
            }
            return;
        }
        let history = self.agent.history_snapshot();
        if history.is_empty() {
            self.notice("nothing to compact yet".into());
            return;
        }
        let (to_summarize, kept) =
            crate::core::agent::compact::split(&history, self.agent.model.context_window);
        if to_summarize.is_empty() {
            if !auto {
                self.notice("recent context already fits — nothing to compact".into());
            }
            return;
        }
        self.compacting = true;
        self.notice("compacting…".into());
        let model = self.agent.model.clone();
        let results = self.results.clone();
        tokio::spawn(async move {
            let job = match crate::core::agent::compact::summarize(model, &to_summarize).await {
                Ok(summary) => AppJob::Compacted { summary, kept },
                Err(message) => AppJob::CompactFailed(message),
            };
            let _ = results.send(job).await;
        });
    }

    /// `!cmd`: run it through the bash tool off-task; the result arrives as
    /// AppJob::Shell. Idle only — mid-turn the history is the model's.
    fn run_shell(&mut self, cmd: String) {
        if self.agent.is_streaming() || self.compacting {
            self.notice("busy — run shell commands between turns".into());
            return;
        }
        if self.shell_block.is_some() {
            self.notice("a shell command is still running".into());
            return;
        }
        self.transcript.push(Block::new(Kind::Shell, cmd.clone()));
        self.shell_block = Some(self.transcript.blocks.len() - 1);
        let results = self.results.clone();
        let cwd = self.agent.cwd();
        let epoch = self.session_epoch;
        tokio::spawn(async move {
            let shell_cmd = cmd.clone();
            let output = tokio::task::spawn_blocking(move || {
                crate::core::tools::run_shell(&shell_cmd, &cwd)
            })
            .await
            .unwrap_or(crate::core::tools::ToolOutput {
                content: "shell command panicked".into(),
                outcome: crate::core::tools::ToolOutcome::Failed,
                summary: "error".into(),
                display: None,
            });
            let _ = results.send(AppJob::Shell { cmd, output, epoch }).await;
        });
    }

    fn remember_output(&mut self, title: String, content: String) {
        const OUTPUT_BUDGET: usize = 4 * 1024 * 1024;
        self.outputs.push((title, content));
        let mut bytes: usize = self.outputs.iter().map(|(_, body)| body.len()).sum();
        while self.outputs.len() > 1 && (self.outputs.len() > 50 || bytes > OUTPUT_BUDGET) {
            let removed = self.outputs.remove(0).1.len();
            bytes = bytes.saturating_sub(removed);
            if let Some(viewer) = &mut self.viewer {
                viewer.index = viewer.index.saturating_sub(1);
            }
        }
    }

    /// /reload, the reference behavior: refresh what a session caches. In e
    /// that is the extension host (restarted) and the theme (re-resolved) —
    /// skills, prompts, AGENTS.md, settings, and models.json are read fresh
    /// on every use already.
    fn reload(&mut self) {
        if self.agent.is_streaming() {
            self.notice("wait for the turn to finish before /reload".into());
            return;
        }
        if self.compacting {
            self.notice("wait for compaction to finish before /reload".into());
            return;
        }
        if self.reloading {
            return;
        }
        // With a freshly installed update on disk, /reload becomes the
        // switch: exit through the normal cleanup and exec the new binary
        // with -c, which resumes this session.
        if self.update_installed.is_some() {
            self.relaunch = true;
            self.should_quit = true;
            return;
        }
        self.reloading = true;
        self.reload_block = Some(self.transcript.push(Block::new(Kind::Notice, "reloading…")));
        let old = self.host.clone();
        let jobs = self.jobs.clone();
        let results = self.results.clone();
        tokio::spawn(async move {
            old.shutdown().await;
            let host = crate::core::api::ExtensionHost::start(jobs).await;
            let _ = results.send(AppJob::Reloaded(host)).await;
        });
    }

    fn copy_last(&mut self) {
        let last = self
            .agent
            .history_snapshot()
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
            .map(|m| m.content.clone());
        match last {
            Some(text) => {
                // OSC 52: the terminal-native clipboard, no helper binary,
                // works over ssh too. Terminals without it silently ignore
                // the sequence.
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
                let ok = write!(std::io::stdout(), "\x1b]52;c;{encoded}\x07").is_ok();
                let _ = std::io::stdout().flush();
                self.notice(if ok {
                    "copied the last reply".into()
                } else {
                    "copy failed".into()
                });
            }
            None => self.notice("nothing to copy yet".into()),
        }
    }

    /// Reload the theme from settings, using the startup background probe.
    fn apply_theme(&mut self) {
        self.theme = crate::tui::theme::resolve(
            &crate::core::config::settings::theme(),
            self.light_background,
        );
        self.transcript.invalidate();
    }

    /// Re-derive the cached sign-in and effort state from disk. Call after
    /// anything that can change them: sign-in, model switch, effort cycle,
    /// settings changes, /reload.
    fn refresh_status_cache(&mut self) {
        self.signed_in =
            crate::core::auth::signed_in(&crate::core::auth::load(), &self.agent.model.provider);
        self.status_effort = self.agent.effort();
    }

    fn notice(&mut self, text: String) {
        self.transcript.push(Block::new(Kind::Notice, text));
    }
}

/// Replace /reload's in-progress notice, or append the result if that block
/// disappeared when another command cleared the transcript.
fn finish_reload_notice(transcript: &mut Transcript, reload_block: Option<usize>) {
    const STARTED: &str = "reloading…";
    const FINISHED: &str = "reloaded extensions, themes, and config — skills, prompts, and AGENTS.md are always read fresh";

    if let Some(block) = reload_block
        .and_then(|index| transcript.blocks.get_mut(index))
        .filter(|block| block.kind == Kind::Notice && block.text == STARTED)
    {
        block.text = FINISHED.into();
        block.touch();
    } else {
        transcript.push(Block::new(Kind::Notice, FINISHED));
    }
}

/// The terminal tab title: the custom glyph, a dot, then the session name
/// or the working directory (the reference prefers the session name and
/// falls back to the workspace path).
fn tab_title(path: &str, session_name: Option<&str>) -> String {
    let label = session_name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(path);
    format!("𝑒 · {label}")
}

fn set_tab_title(title: &str) {
    // Escape codes into a pipe are garbage in the pipe; titles only make
    // sense on a terminal.
    if !stdout_is_tty() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]0;{title}\x07");
    let _ = out.flush();
}

/// The tab title's path: a short showcase, never the full absolute path.
/// Under $HOME the prefix collapses to `~`; elsewhere only the last two
/// components are shown, so a volume-qualified worktree reads cleanly
/// instead of bleeding its whole path into the tab.
fn title_path() -> String {
    title_path_from(
        &std::env::current_dir().unwrap_or_default(),
        &std::env::var("HOME").unwrap_or_default(),
    )
}

/// The shortening rule, split out for tests.
fn title_path_from(cwd: &std::path::Path, home: &str) -> String {
    use std::path::Component;
    let under_home = !home.is_empty() && cwd.starts_with(home);
    let mut comps: Vec<&str> = cwd
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_str().unwrap_or_default()),
            _ => None,
        })
        .collect();
    // The `~` marker replaces the whole home prefix, not one level of it.
    if under_home {
        let prefix = std::path::Path::new(home)
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_str().unwrap_or_default()),
                _ => None,
            })
            .count();
        comps.drain(..prefix.min(comps.len()));
    }
    let tail = comps.split_off(comps.len().saturating_sub(2)).join("/");
    if under_home {
        if tail.is_empty() {
            "~".to_string()
        } else {
            format!("~/{tail}")
        }
    } else if tail.is_empty() {
        // The root itself stays a slash rather than a bare "".
        "/".to_string()
    } else {
        tail
    }
}

fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let secs = now.saturating_sub(ms) / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

fn system_prompt() -> String {
    crate::core::agent::context::system_prompt_here()
}

fn persist_model(m: &Model) {
    crate::core::config::settings::set_string("model", &model::slug(m));
}

/// The text after a slash command, only on a word boundary: `/login x` →
/// `Some(" x")`, `/login` → `Some("")`, `/loginfoo` → `None` (so a typo falls
/// through to the unknown-command notice instead of inventing an argument).
fn command_arg<'a>(input: &'a str, command: &str) -> Option<&'a str> {
    input
        .strip_prefix(command)
        .filter(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// Decide whether a command-line prompt may start now or must wait for the
/// first-visit trust panel. Kept separate so launch ordering stays testable
/// without constructing a terminal frame.
/// Command names dispatch resolves before templates and extension commands.
/// Keep in sync with `dispatch_command`'s match arms.
fn is_builtin_command(name: &str) -> bool {
    matches!(
        name,
        "login"
            | "models"
            | "model"
            | "scoped-models"
            | "reload"
            | "resume"
            | "new"
            | "clear"
            | "copy"
            | "compact"
            | "trust"
            | "settings"
            | "help"
            | "version"
            | "quit"
            | "exit"
    )
}

fn stage_initial_prompt(
    initial: String,
    awaiting_trust: bool,
    pending: &mut Option<String>,
) -> Option<String> {
    if initial.trim().is_empty() {
        return None;
    }
    if awaiting_trust {
        *pending = Some(initial);
        None
    } else {
        Some(initial)
    }
}

/// Replace this process with the current e binary, optionally in a new cwd.
/// Extensions may choose arguments and environment, but never an arbitrary
/// executable.
pub fn relaunch_self(
    cwd: &str,
    args: &[String],
    env: &std::collections::BTreeMap<String, Option<String>>,
) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command.current_dir(cwd).args(args);
    for (name, value) in env {
        if name.is_empty()
            || name.contains('=')
            || name.contains('\0')
            || value.as_deref().is_some_and(|value| value.contains('\0'))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid relaunch environment entry",
            ));
        }
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    Err(command.exec())
}

fn key_of(event: &KeyEvent) -> Option<Key> {
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    Some(match (event.code, ctrl, alt) {
        (KeyCode::Enter, ..) if shift || alt => Key::Newline,
        (KeyCode::Enter, ..) => Key::Enter,
        (KeyCode::Backspace, _, true) => Key::KillWord,
        (KeyCode::Backspace, ..) => Key::Backspace,
        (KeyCode::Delete, ..) => Key::Delete,
        (KeyCode::Left, _, true) => Key::WordLeft,
        (KeyCode::Right, _, true) => Key::WordRight,
        (KeyCode::Left, ..) => Key::Left,
        (KeyCode::Right, ..) => Key::Right,
        (KeyCode::Up, ..) => Key::Up,
        (KeyCode::Down, ..) => Key::Down,
        (KeyCode::Home, ..) => Key::Home,
        (KeyCode::End, ..) => Key::End,
        (KeyCode::Char('a'), true, _) => Key::Home,
        (KeyCode::Char('e'), true, _) => Key::End,
        (KeyCode::Char('k'), true, _) => Key::KillToEnd,
        (KeyCode::Char('u'), true, _) => Key::KillToStart,
        (KeyCode::Char('w'), true, _) => Key::KillWord,
        (KeyCode::Char('b'), true, _) => Key::Left,
        (KeyCode::Char('f'), true, _) => Key::Right,
        (KeyCode::Char('j'), true, _) => Key::Newline,
        (KeyCode::Char(c), false, false) => Key::Char(c),
        _ => return None,
    })
}

/// Open the interactive session. `args` are already past the extension
/// startup hook (flags rewritten, cwd possibly changed).
pub async fn run(
    args: Vec<String>,
    host: std::sync::Arc<crate::core::api::ExtensionHost>,
    jobs_tx: tokio::sync::mpsc::Sender<String>,
    mut jobs_rx: tokio::sync::mpsc::Receiver<String>,
) -> std::io::Result<()> {
    // A panic mid-frame must not strand the shell in raw mode with a hidden
    // cursor or kitty keyboard flags — restore the terminal first, then
    // report as usual. (\x1b[<u pops the keyboard enhancement stack.)
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = terminal::disable_raw_mode();
            print!("\x1b[<u\x1b[?2004l\x1b[?25h\r\n");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            default_hook(info);
        }));
    }

    // Raw mode first so the frame loop can take the terminal over. Theme
    // detection reads COLORFGBG only — no OSC 11 stdin probe, which would
    // race the reply against startup keystrokes (audit #93). The guard
    // exists before any further mode changes, so every exit path —
    // including `?` returns below — restores all of them.
    terminal::enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(
        std::io::stdout(),
        EnableBracketedPaste,
        // The kitty keyboard protocol: without it, terminals send plain Enter
        // for shift+enter and multi-line entry is unreachable.
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    // COLORFGBG-only detection: nothing is read from stdin, so there are no
    // probe-window keystrokes to salvage (audit #93).
    let detected = crate::tui::background::detect_light();
    let detected = detected.unwrap_or(false);
    let theme = crate::tui::theme::resolve(&crate::core::config::settings::theme(), detected);

    let (mut cols, mut rows) = terminal::size()?;
    let mut painter = Painter::spawn(cols, rows);
    let (mut agent, mut session_events) = Agent::new(model::default_model());
    let (logins_tx, mut logins_rx) =
        tokio::sync::mpsc::channel::<crate::core::auth::login::Outcome>(4);
    let (results_tx, mut results_rx) = tokio::sync::mpsc::channel::<AppJob>(16);
    agent.set_host(host.clone());
    let mut app = App {
        theme,
        transcript: Transcript::default(),
        editor: Editor::new(),
        agent,
        active: None,
        overlay: None,
        armed_at: None,
        should_quit: false,
        context_tokens: 0,
        pending_key: None,
        menu: None,
        auth: None,
        settings: None,
        show_thinking: crate::core::config::settings::get_string("show_thinking").as_deref()
            != Some("off"),
        jobs: jobs_tx,
        logins: logins_tx,
        login_task: None,
        login_sequence: 0,
        host,
        results: results_tx,
        input_verdicts: PendingInputVerdicts::default(),
        compacting: false,
        compact_requested: false,
        held_prompts: Vec::new(),
        trust: None,
        pending_initial: None,
        shell_block: None,
        reloading: false,
        reload_block: None,
        outputs: Vec::new(),
        viewer: None,
        session_epoch: 0,
        update_installed: None,
        relaunch: false,
        light_background: detected,
        signed_in: false,
        status_effort: None,
    };
    app.refresh_status_cache();
    app.transcript
        .push(Block::new(Kind::Banner, crate::VERSION));
    for warning in model::config_warnings() {
        app.notice(format!("warning: {warning}"));
    }
    if crate::core::config::trust::status(&app.agent.cwd()).is_none() {
        app.trust = Some(TrustStage { selected: 0 });
    }
    // The harness pattern: check for a newer release in the background at
    // launch, install it silently, and say so — the running session is
    // untouched until a restart. Dev builds and the opt-out are exempt.
    if !crate::core::update::is_dev_build() && crate::core::config::settings::auto_update() {
        let results = app.results.clone();
        tokio::spawn(async move {
            if let Ok(Some(version)) = crate::core::update::self_update().await {
                let _ = results.send(AppJob::Updated(version)).await;
            }
        });
    }
    // Providers' model lists refresh in the background (the reference
    // behavior, sourced from each gateway's own /models): a model a provider
    // ships today shows in /models today, no e release involved.
    tokio::spawn(crate::core::providers::catalog::refresh_remote());
    if crate::core::auth::load().is_empty() {
        app.notice(
            "no provider signed in — use /login to sign in with an account or API key".into(),
        );
        // Route straight to sign-in instead of leaving a phantom model
        // implied on the status bar. Yields to the trust panel above it,
        // if that's showing too — this still renders once trust is settled.
        app.open_login_menu();
    } else if let Some(wanted) = crate::core::config::settings::get_string("model") {
        let current = app.agent.model_slug();
        if wanted != current {
            app.notice(format!(
                "{wanted} is unavailable (provider not signed in) — using {current}"
            ));
        }
    }
    // -c continues this workspace's most recent session. Only the flags e
    // actually knows are removed from the prompt — a word like `-O2` is
    // prompt content, and `--` ends flag parsing entirely.
    let mut continue_flag = false;
    let mut resume_flag = false;
    let mut message_args: Vec<&str> = Vec::new();
    let mut past_flags = false;
    for arg in &args {
        if !past_flags {
            match arg.as_str() {
                "--" => {
                    past_flags = true;
                    continue;
                }
                "-c" | "--continue" => {
                    continue_flag = true;
                    continue;
                }
                "-r" | "--resume" => {
                    resume_flag = true;
                    continue;
                }
                _ => {}
            }
        }
        message_args.push(arg);
    }
    let initial: String = message_args.join(" ");
    if resume_flag {
        // The reference behavior: launch straight into the session picker.
        app.open_resume_menu();
    } else if continue_flag {
        app.resume_recent();
    }

    // Terminal tab title: the custom glyph, a dot, the path — a named
    // session takes over the title when one lands (fx also prefers the
    // session name over the workspace).
    set_tab_title(&tab_title(
        &title_path(),
        app.agent.session_name().as_deref(),
    ));
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    // SIGTERM/SIGHUP (a kill, a closed tab) exit through the same cleanup as
    // /quit — the terminal is restored, the extension host shut down.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

    // A -r launch prompt must wait for the session pick, or it would start a
    // turn whose reply splices into whichever session gets selected.
    let hold_initial = app.trust.is_some() || (resume_flag && app.menu.is_some());
    if let Some(initial) = stage_initial_prompt(initial, hold_initial, &mut app.pending_initial) {
        app.submit(initial);
    }
    painter.frame(app.frame(cols as usize));

    // Frame pacing: every select arm may change what's on screen, but frames
    // are built at most once per interval — a token burst becomes one paint,
    // and a deferred paint fires when the interval lapses.
    const FRAME_INTERVAL: Duration = Duration::from_millis(33);
    let mut next_paint = tokio::time::Instant::now();
    let mut paint_deferred = false;
    let mut event_buf: Vec<SessionEvent> = Vec::with_capacity(128);

    loop {
        tokio::select! {
            maybe = events.next() => {
                let Some(Ok(event)) = maybe else { break };
                match event {
                    TermEvent::Paste(text) => {
                        // A paste is one unit; long or multiline pastes become
                        // a placeholder token (the reference behavior) that
                        // expands back on submit.
                        app.editor.insert_paste(&text.replace('\r', "\n"));
                        app.sync_menu();
                    }
                    TermEvent::Resize(c, r) => {
                        cols = c;
                        rows = r;
                        painter.resize(c, r);
                    }
                    TermEvent::Key(k) if k.kind != crossterm::event::KeyEventKind::Release => {
                        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                        if app.viewer.is_some() {
                            let close = k.code == KeyCode::Esc
                                || (ctrl && k.code == KeyCode::Char('o'));
                            if close {
                                app.viewer = None;
                            } else if let Some(viewer) = &mut app.viewer {
                                let total = app
                                    .outputs
                                    .get(viewer.index)
                                    .map(|(_, c)| c.lines().count())
                                    .unwrap_or(0);
                                match k.code {
                                    KeyCode::Up => viewer.scroll = viewer.scroll.saturating_sub(1),
                                    KeyCode::Down => {
                                        viewer.scroll = (viewer.scroll + 1)
                                            .min(total.saturating_sub(1))
                                    }
                                    KeyCode::PageUp => {
                                        viewer.scroll = viewer.scroll.saturating_sub(20)
                                    }
                                    KeyCode::PageDown => {
                                        viewer.scroll = (viewer.scroll + 20)
                                            .min(total.saturating_sub(1))
                                    }
                                    KeyCode::Left => {
                                        viewer.index = viewer.index.saturating_sub(1);
                                        viewer.scroll = 0;
                                    }
                                    KeyCode::Right => {
                                        viewer.index = (viewer.index + 1)
                                            .min(app.outputs.len().saturating_sub(1));
                                        viewer.scroll = 0;
                                    }
                                    _ => {}
                                }
                            }
                        } else if ctrl && k.code == KeyCode::Char('o') {
                            if app.outputs.is_empty() {
                                app.notice("no tool output to view yet".into());
                            } else {
                                app.viewer = Some(Viewer {
                                    index: app.outputs.len() - 1,
                                    scroll: 0,
                                });
                            }
                        } else if let Some(stage) = &mut app.trust {
                            match k.code {
                                KeyCode::Up | KeyCode::Down => stage.selected = 1 - stage.selected,
                                KeyCode::Enter => {
                                    let trusted = stage.selected == 0;
                                    match crate::core::config::trust::set(&app.agent.cwd(), trusted) {
                                        Err(e) => app.notice(format!("trust: {e}")),
                                        Ok(()) => {
                                            app.trust = None;
                                            if !trusted {
                                                app.notice("working untrusted — project AGENTS.md and .e skills/prompts ignored (/trust to allow)".into());
                                            }
                                            // An open -r picker still owns
                                            // the launch prompt; submitting
                                            // it now would start a turn the
                                            // session pick then refuses.
                                            if app.menu.is_none() {
                                                if let Some(initial) = app.pending_initial.take() {
                                                    app.submit(initial);
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else if let Some(panel) = &mut app.settings {
                            match k.code {
                                KeyCode::Up => panel.step(-1),
                                KeyCode::Down => panel.step(1),
                                KeyCode::Left => panel.change(-1),
                                KeyCode::Right => panel.change(1),
                                KeyCode::Esc | KeyCode::Enter => app.settings = None,
                                _ => {}
                            }
                            // A theme change applies immediately; settings can
                            // also change what the statusline derives from disk.
                            // The thinking toggle is file-backed too — re-read
                            // it so a mid-session change lands this frame.
                            app.apply_theme();
                            app.show_thinking = crate::core::config::settings::get_string(
                                "show_thinking",
                            )
                            .as_deref()
                            != Some("off");
                            app.refresh_status_cache();
                        } else if let Some(stage) = &mut app.auth {
                            match (&mut *stage, k.code) {
                                (AuthStage::Choose { selected }, KeyCode::Up | KeyCode::Down) => {
                                    *selected = 1 - *selected;
                                }
                                (AuthStage::Choose { selected }, KeyCode::Enter) => {
                                    let choice = *selected;
                                    app.auth_choose(choice);
                                }
                                (AuthStage::Account { selected }, KeyCode::Up | KeyCode::Down) => {
                                    let n = crate::core::providers::registry::oauth_providers().len();
                                    *selected = (*selected + 1) % n.max(1);
                                }
                                (AuthStage::Key { selected }, KeyCode::Up) => {
                                    let n = crate::core::providers::registry::key_providers().len();
                                    *selected = (*selected + n - 1) % n.max(1);
                                }
                                (AuthStage::Key { selected }, KeyCode::Down) => {
                                    let n = crate::core::providers::registry::key_providers().len();
                                    *selected = (*selected + 1) % n.max(1);
                                }
                                (AuthStage::Account { selected }, KeyCode::Enter) => {
                                    let choice = *selected;
                                    app.auth_account(choice);
                                }
                                (AuthStage::Key { selected }, KeyCode::Enter) => {
                                    let choice = *selected;
                                    app.auth_key(choice);
                                }
                                (_, KeyCode::Esc) => {
                                    let waiting = matches!(&*stage, AuthStage::Waiting);
                                    let cancelled = app.cancel_login();
                                    app.auth = None;
                                    app.pending_key = None;
                                    app.editor.mask = false;
                                    app.editor.set_text("");
                                    if waiting && cancelled {
                                        app.notice("login cancelled".into());
                                    }
                                }
                                (AuthStage::ApiKey { .. }, _) => {
                                    if let Some(key) = key_of(&k) {
                                        if let EditorResult::Submit(text) = app.editor.key(key) {
                                            app.submit(text);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else if app
                            .menu
                            .as_ref()
                            .map(|m| m.kind == MenuKind::Scoped)
                            .unwrap_or(false)
                            && ((k.code == KeyCode::Char(' ') && !ctrl)
                                || (ctrl && k.code == KeyCode::Char('x')))
                        {
                            if ctrl {
                                model::clear_scope();
                                app.notice("scope cleared — ctrl+p cycles every model again".into());
                                app.open_scoped_menu();
                            } else {
                                app.toggle_scoped();
                            }
                        } else if app.menu.is_some()
                            && matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Esc)
                            && !ctrl
                        {
                            match k.code {
                                KeyCode::Up => { app.menu.as_mut().unwrap().step(-1); }
                                KeyCode::Down => { app.menu.as_mut().unwrap().step(1); }
                                KeyCode::Enter => { app.select_menu(); }
                                KeyCode::Esc => {
                                    app.menu = None;
                                    // Declining the -r picker releases a
                                    // held launch prompt into the current
                                    // session.
                                    app.release_initial_prompt();
                                }
                                _ => {}
                            }
                        } else if k.code == KeyCode::Esc && app.pending_key.is_some() {
                            app.pending_key = None;
                            app.editor.mask = false;
                            app.editor.set_text("");
                            app.notice("login cancelled".into());
                        } else if k.code == KeyCode::Esc && app.agent.is_streaming() {
                            app.agent.interrupt();
                        } else if ctrl && k.code == KeyCode::Char('c') {
                            if app.agent.is_streaming() {
                                app.agent.interrupt();
                                arm(&mut app);
                            } else if !app.editor.is_empty() {
                                app.editor.set_text("");
                                arm(&mut app);
                            } else if app.armed_at.map(|t| t.elapsed() < Duration::from_millis(1500)).unwrap_or(false) {
                                break;
                            } else {
                                arm(&mut app);
                            }
                        } else if ctrl && matches!(k.code, KeyCode::Char('p') | KeyCode::Char('P')) {
                            let backward = k.code == KeyCode::Char('P')
                                || k.modifiers.contains(KeyModifiers::SHIFT);
                            app.cycle_model(!backward);
                        } else if ctrl && k.code == KeyCode::Char('d') && app.editor.is_empty() {
                            break;
                        } else if k.code == KeyCode::BackTab
                            || (k.code == KeyCode::Tab
                                && !ctrl
                                && k.modifiers.contains(KeyModifiers::SHIFT))
                        {
                            // Shift+tab cycles the reasoning effort through
                            // whatever levels this model declares. The
                            // statusline already shows the new level; only a
                            // model without a reasoning knob gets a notice.
                            match app.agent.cycle_effort() {
                                Some(_) => app.refresh_status_cache(),
                                None => app.notice("this model has no reasoning effort control".into()),
                            }
                        } else if let Some(key) = key_of(&k) {
                            if let EditorResult::Submit(text) = app.editor.key(key) {
                                app.submit(text);
                            }
                            app.sync_menu();
                        }
                    }
                    _ => {}
                }
            }
            count = session_events.recv_many(&mut event_buf, 128) => {
                if count == 0 {
                    break;
                }
                // Apply the whole burst before building one frame — a fast
                // stream must not cost one paint per delta.
                for e in event_buf.drain(..) {
                    app.on_session_event(e);
                }
            }
            job = results_rx.recv() => {
                match job {
                    Some(AppJob::Notice(notice)) => app.notice(notice),
                    Some(AppJob::Prompt { text, epoch }) => {
                        // A prompt from a command that started in an earlier
                        // session must not start a turn in its replacement.
                        if epoch == app.session_epoch {
                            app.prompt(text);
                        } else {
                            app.notice(
                                "an extension command finished after the session changed — its prompt was discarded"
                                    .into(),
                            );
                        }
                    }
                    Some(AppJob::InputVerdict { sequence, text, verdict }) => {
                        // A later hook may finish first; hold it until every
                        // earlier submission has a verdict, then apply the
                        // contiguous ordered prefix.
                        for (text, verdict) in
                            app.input_verdicts.complete(sequence, text, verdict)
                        {
                            app.apply_input_verdict(text, verdict);
                        }
                    }
                    Some(AppJob::Rename { name, epoch }) => {
                        // A rename from a command that started in an earlier
                        // session must not rename its replacement.
                        if epoch == app.session_epoch {
                            app.agent.set_session_name(name.clone());
                            app.notice(format!("session: {name}"));
                            set_tab_title(&tab_title(&title_path(), Some(&name)));
                        }
                    }
                    Some(AppJob::Compacted { summary, kept }) => {
                        // Ignore a result that outlived its session (/new won).
                        if app.compacting {
                            app.compacting = false;
                            app.agent.load_compacted(&summary, kept);
                            let seeded: usize = app
                                .agent
                                .history_snapshot()
                                .iter()
                                .map(|m| m.content.len())
                                .sum();
                            app.context_tokens = (seeded / 4) as u64;
                            app.shell_block = None;
                            app.transcript.clear();
                            app.transcript.push(Block::new(Kind::Banner, crate::VERSION));
                            app.transcript.push(Block::new(Kind::Notice, "compacted — recent messages kept, the full session is under /resume"));
                            app.transcript.push(Block::new(Kind::Summary, summary));
                            for text in std::mem::take(&mut app.held_prompts) {
                                app.prompt(text);
                            }
                        }
                    }
                    Some(AppJob::CompactFailed(message)) if app.compacting => {
                        app.compacting = false;
                        app.notice(format!("compact failed: {message}"));
                        for text in std::mem::take(&mut app.held_prompts) {
                            app.prompt(text);
                        }
                    }
                    Some(AppJob::CompactFailed(_)) => {}
                    Some(AppJob::CatalogRefreshed) => {
                        if let Some(menu) = &app.menu {
                            if menu.kind == MenuKind::Models {
                                let selected =
                                    menu.current().map(|item| item.value.clone());
                                app.build_model_menu();
                                if let (Some(menu), Some(value)) =
                                    (&mut app.menu, selected)
                                {
                                    menu.select_value(&value);
                                }
                            }
                        }
                    }
                    Some(AppJob::Updated(version)) => {
                        app.notice(format!(
                            "e {version} installed — /reload to switch to it now"
                        ));
                        app.update_installed = Some(version);
                    }
                    Some(AppJob::Reloaded(host)) => {
                        app.reloading = false;
                        app.host = host.clone();
                        app.agent.set_host(host);
                        app.apply_theme();
                        app.refresh_status_cache();
                        finish_reload_notice(&mut app.transcript, app.reload_block.take());
                        for text in std::mem::take(&mut app.held_prompts) {
                            app.prompt(text);
                        }
                    }
                    Some(AppJob::Shell { cmd, output, epoch }) => {
                        // A result from a command started in an earlier
                        // session must not be recorded into this one.
                        if epoch != app.session_epoch {
                            app.notice(format!(
                                "`{cmd}` finished after the session changed — output discarded"
                            ));
                        } else {
                            // Display a trimmed tail in the live block;
                            // history gets the full (tool-truncated) output.
                            let display_output =
                                crate::core::tools::sanitize_display(&output.content);
                            let shown: String = {
                                let lines: Vec<&str> = display_output.lines().collect();
                                let tail = &lines[lines.len().saturating_sub(20)..];
                                let mut text = tail.join("\n");
                                if lines.len() > 20 {
                                    text = format!(
                                        "… ({} more lines above)\n{text}",
                                        lines.len() - 20
                                    );
                                }
                                text
                            };
                            if let Some(idx) = app.shell_block.take() {
                                if let Some(block) = app.transcript.blocks.get_mut(idx) {
                                    block.done = true;
                                    block.is_error = output.is_error();
                                    block.detail = Some(shown);
                                    block.touch();
                                }
                            }
                            if !output.content.trim().is_empty() {
                                app.remember_output(format!("$ {cmd}"), display_output);
                            }
                            app.agent.record_user(format!(
                                "I ran `{cmd}` in my shell. Output:\n```\n{}\n```",
                                output.content
                            ));
                            // Prompts held for the shell result submit now,
                            // ordered after it.
                            for text in std::mem::take(&mut app.held_prompts) {
                                app.prompt(text);
                            }
                        }
                    }
                    None => {}
                }
            }
            message = jobs_rx.recv() => {
                if let Some(message) = message {
                    app.notice(message);
                }
            }
            outcome = logins_rx.recv() => {
                // Control flow hangs off the typed outcome; the human-readable
                // notice arrives separately on `jobs`.
                match outcome {
                    Some(crate::core::auth::login::Outcome::SignedIn { flow_id, .. })
                        if app.login_outcome_is_current(flow_id) => {
                            app.login_task.take();
                            if matches!(app.auth, Some(AuthStage::Waiting)) {
                                app.auth = None;
                            }
                            tokio::spawn(crate::core::providers::catalog::refresh_remote());
                            // A fresh credential may make new models available:
                            // if the current model's provider is still signed out,
                            // fall back to the first available model.
                            if !crate::core::auth::signed_in(&crate::core::auth::load(), &app.agent.model.provider) {
                                if let Some(m) = crate::core::providers::catalog::available().into_iter().next() {
                                    app.notice(format!("model set to {}", crate::core::providers::catalog::slug(&m)));
                                    app.agent.model = m;
                                }
                            }
                            app.refresh_status_cache();
                        }
                    Some(crate::core::auth::login::Outcome::Failed { flow_id })
                        if app.login_outcome_is_current(Some(flow_id)) => {
                            app.login_task.take();
                            app.auth = None;
                        }
                    // A canceled flow can finish just before its task aborts.
                    // Its queued outcome must not affect the replacement flow.
                    Some(_) => {}
                    None => {}
                }
            }
            _ = sigterm.recv() => break,
            _ = sighup.recv() => break,
            // A paint was skipped inside the frame interval; fire it when
            // the interval lapses.
            _ = tokio::time::sleep_until(next_paint), if paint_deferred => {}
            _ = tick.tick() => {
                if let Some(at) = app.armed_at {
                    if at.elapsed() > Duration::from_millis(1600) {
                        app.armed_at = None;
                        app.overlay = None;
                    }
                }
                if let Some(s) = &mut app.active {
                    let expired = s.turn.recovered
                        .is_some_and(|r| r.since.elapsed() > Duration::from_millis(RECOVERED_VISIBLE_MS));
                    if expired {
                        s.turn.recovered = None;
                    }
                }
            }
        }
        let now = tokio::time::Instant::now();
        if now >= next_paint || app.should_quit {
            let frame = if app.viewer.is_some() {
                app.viewer_frame(cols as usize, rows as usize)
            } else {
                app.frame(cols as usize)
            };
            painter.frame(frame);
            next_paint = now + FRAME_INTERVAL;
            paint_deferred = false;
        } else {
            paint_deferred = true;
        }
        if app.should_quit {
            break;
        }
    }

    // Let the final frame land before the terminal is restored.
    painter.shutdown();
    app.host.shutdown().await;
    drop(_guard);
    // The tab title we set at launch (or from a session name) is ours to
    // clear — the reference leaves the terminal pristine on exit.
    set_tab_title("");
    if app.relaunch {
        // The terminal is restored and the host is down: replace this
        // process with the updated binary, continuing the same session.
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string();
        let args = vec!["-c".to_string()];
        if let Err(error) = relaunch_self(&cwd, &args, &std::collections::BTreeMap::new()) {
            eprintln!("relaunch failed: {error} — start e again by hand");
        }
    }
    Ok(())
}

fn arm(app: &mut App) {
    app.armed_at = Some(Instant::now());
    app.overlay = Some("press ctrl+c again to exit".into());
}

/// Restores every terminal mode the TUI enables — keyboard enhancement
/// flags, bracketed paste, raw mode, cursor visibility — on every exit
/// path, `?` returns and unwinds included. Popping a mode that never got
/// enabled is harmless; leaving one enabled corrupts the user's shell.
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            std::io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste
        );
        let _ = terminal::disable_raw_mode();
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = write!(out, "\r\n\x1b[?25h");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_title_shortens_to_two_components() {
        assert_eq!(
            title_path_from(
                std::path::Path::new("/Volumes/v0/workspaces/worktrees/e/bold-fox"),
                ""
            ),
            "e/bold-fox"
        );
        assert_eq!(title_path_from(std::path::Path::new("/etc"), ""), "etc");
        assert_eq!(title_path_from(std::path::Path::new("/"), ""), "/");
    }

    #[test]
    fn tab_title_is_home_relative_under_home() {
        assert_eq!(
            title_path_from(std::path::Path::new("/Users/fschr/code/x"), "/Users/fschr"),
            "~/code/x"
        );
        assert_eq!(
            title_path_from(std::path::Path::new("/Users/fschr"), "/Users/fschr"),
            "~"
        );
        assert_eq!(
            title_path_from(
                std::path::Path::new("/Users/fschr/code/a/b/c"),
                "/Users/fschr"
            ),
            "~/b/c"
        );
    }

    #[test]
    fn reload_result_replaces_the_in_progress_notice() {
        let mut transcript = Transcript::default();
        let reload_block = transcript.push(Block::new(Kind::Notice, "reloading…"));
        transcript.push(Block::new(Kind::Notice, "an unrelated notice"));

        finish_reload_notice(&mut transcript, Some(reload_block));

        assert_eq!(transcript.blocks.len(), 2);
        assert_eq!(
            transcript.blocks[reload_block].text,
            "reloaded extensions, themes, and config — skills, prompts, and AGENTS.md are always read fresh"
        );
        assert_eq!(transcript.blocks[1].text, "an unrelated notice");
    }

    #[test]
    fn initial_prompt_waits_for_first_visit_trust() {
        let mut pending = None;
        let now = stage_initial_prompt("inspect this repo".into(), true, &mut pending);
        assert!(now.is_none());
        assert_eq!(pending.as_deref(), Some("inspect this repo"));

        let mut pending = None;
        let now = stage_initial_prompt("inspect this repo".into(), false, &mut pending);
        assert_eq!(now.as_deref(), Some("inspect this repo"));
        assert!(pending.is_none());
    }

    #[test]
    fn api_keys_bypass_input_hooks() {
        assert_eq!(input_route(true, true), InputRoute::ApiKey);
        assert_eq!(input_route(true, false), InputRoute::ApiKey);
        assert_eq!(input_route(false, true), InputRoute::Hook);
        assert_eq!(input_route(false, false), InputRoute::Direct);
    }

    #[test]
    fn tab_title_prefers_the_session_name_over_the_path() {
        assert_eq!(tab_title("~/work", None), "𝑒 · ~/work");
        assert_eq!(
            tab_title("~/work", Some("fix the renderer")),
            "𝑒 · fix the renderer"
        );
        // A blank name falls back to the path, never an empty title.
        assert_eq!(tab_title("~/work", Some("   ")), "𝑒 · ~/work");
    }

    #[test]
    fn input_hook_verdicts_apply_in_submission_order() {
        let mut pending = PendingInputVerdicts::default();
        let first = pending.reserve();
        let second = pending.reserve();

        let later = pending.complete(
            second,
            "second".into(),
            crate::core::api::InputVerdict::default(),
        );
        assert!(later.is_empty(), "a later verdict must wait");

        let ordered = pending.complete(
            first,
            "first".into(),
            crate::core::api::InputVerdict::default(),
        );
        assert_eq!(
            ordered
                .into_iter()
                .map(|(text, _)| text)
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }
    #[tokio::test]
    async fn active_login_guard_cancels_on_drop() {
        let cancellation = crate::core::auth::login::LoginCancellation::default();
        let observed = cancellation.clone();
        let task = tokio::spawn(std::future::pending());
        let login = ActiveLogin {
            flow_id: 1,
            cancellation,
            task,
            wait_for_callback: false,
        };

        drop(login);
        assert!(observed.is_cancelled());
        tokio::task::yield_now().await;
    }

    fn session_app() -> App {
        let (agent, _rx) = Agent::new(Model {
            provider: "mock".into(),
            id: "m".into(),
            base_url: "http://localhost".into(),
            api: crate::core::providers::catalog::Api::Completions,
            efforts: Vec::new(),
            thinking: crate::core::providers::catalog::Thinking::Manual,
            context_window: 200_000,
        });
        let (jobs, _) = tokio::sync::mpsc::channel(1);
        let (logins, _) = tokio::sync::mpsc::channel(1);
        let (results, _) = tokio::sync::mpsc::channel(1);
        App {
            theme: crate::tui::theme::load_bundled(false).unwrap(),
            transcript: Transcript::default(),
            editor: Editor::new(),
            agent,
            active: None,
            overlay: None,
            armed_at: None,
            should_quit: false,
            context_tokens: 0,
            pending_key: None,
            menu: None,
            auth: None,
            settings: None,
            show_thinking: true,
            jobs,
            logins,
            login_task: None,
            login_sequence: 0,
            host: crate::core::api::ExtensionHost::empty(),
            results,
            input_verdicts: PendingInputVerdicts::default(),
            compacting: false,
            compact_requested: false,
            held_prompts: Vec::new(),
            trust: None,
            pending_initial: None,
            shell_block: None,
            reloading: false,
            reload_block: None,
            outputs: Vec::new(),
            viewer: None,
            session_epoch: 0,
            update_installed: None,
            relaunch: false,
            light_background: false,
            signed_in: false,
            status_effort: None,
        }
    }

    fn thinking_flags(app: &App) -> Vec<(String, bool)> {
        app.transcript
            .blocks
            .iter()
            .filter(|block| block.kind == Kind::Thinking)
            .map(|block| (block.text.clone(), block.done))
            .collect()
    }

    fn tool_batch() -> SessionEvent {
        SessionEvent::ToolBatchStart {
            calls: vec![crate::core::agent::ToolCallPresentation {
                id: 1,
                category: "read".into(),
                running: "reading".into(),
                completed: "read".into(),
                target: "f.rs".into(),
            }],
        }
    }

    /// A typical think-then-tools turn opens a second thinking block when
    /// the batch starts. Both segments stay live through the turn and dim
    /// together at TurnEnd — not only the last index.
    #[test]
    fn turn_end_dims_pre_tool_thinking() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::ReasoningDelta("before tools".into()));
        assert_eq!(thinking_flags(&app), vec![("before tools".into(), false)]);

        app.on_session_event(tool_batch());
        assert_eq!(
            thinking_flags(&app),
            vec![("before tools".into(), false)],
            "pre-tool thinking must stay live until the turn commits"
        );

        app.on_session_event(SessionEvent::ReasoningDelta("after tools".into()));
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("before tools".into(), false),
                ("after tools".into(), false)
            ]
        );

        app.on_session_event(SessionEvent::TurnEnd { aborted: false });
        assert_eq!(
            thinking_flags(&app),
            vec![("before tools".into(), true), ("after tools".into(), true)]
        );
    }

    /// Retries and steered messages also drop the live index. Those earlier
    /// blocks must still dim when the turn commits.
    #[test]
    fn turn_end_dims_thinking_cleared_by_retry_and_steer() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::ReasoningDelta("attempt one".into()));
        app.on_session_event(SessionEvent::Retry {
            attempt: 1,
            limit: 3,
            delay_secs: 1,
            cause: crate::core::providers::FailureCause::Network,
            reason: "timeout".into(),
        });
        app.on_session_event(SessionEvent::ReasoningDelta("attempt two".into()));
        app.on_session_event(SessionEvent::Steered("also check this".into()));
        app.on_session_event(SessionEvent::ReasoningDelta("after steer".into()));
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("attempt one".into(), false),
                ("attempt two".into(), false),
                ("after steer".into(), false)
            ]
        );

        app.on_session_event(SessionEvent::TurnEnd { aborted: false });
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("attempt one".into(), true),
                ("attempt two".into(), true),
                ("after steer".into(), true)
            ]
        );
    }
}
