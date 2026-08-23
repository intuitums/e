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
use crate::core::provider::catalog::{self as model, Model};
use crate::tui::authpanel::{self, AuthStage};
use crate::tui::composer::{Editor, EditorResult, Key};
use crate::tui::menu::{Menu, MenuItem, MenuKind, HINT_SCOPED, HINT_USE};
use crate::tui::screen::Screen;
use crate::tui::statusline::{
    statusline, RecoveredStatus, RetryStatus, StatusData, Turn, TurnPhase, RECOVERED_VISIBLE_MS,
};
use crate::tui::theme::Theme;
use crate::tui::transcript::{Block, Kind, Transcript};
use crate::tui::trustpanel::{self, TrustStage};

/// Per-turn frontend bookkeeping; the engine state lives in the Agent.
struct ActiveTurn {
    /// The current assistant text block, if one is streaming.
    block: Option<usize>,
    text: String,
    turn: Turn,
    started: Instant,
    error: Option<String>,
    /// tool id → stable group block, so lifecycle events update in place.
    tool_blocks: std::collections::HashMap<u64, usize>,
    /// Batch members not yet terminal, including serially pending calls.
    pending_tools: usize,
    /// False until the provider's first real Usage lands; the seed estimate
    /// fills the counters before that.
    usage_seen: bool,
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
    Prompt(String),
    /// A finished /compact: the summary and the recent messages kept verbatim.
    Compacted {
        summary: String,
        kept: Vec<crate::core::provider::ChatMessage>,
    },
    /// A /compact that didn't produce a summary.
    CompactFailed(String),
    /// A finished `!` shell command: what ran and what it printed.
    Shell {
        cmd: String,
        output: crate::core::tools::ToolOutput,
    },
    /// A /reload finished: the restarted extension host.
    Reloaded(std::sync::Arc<crate::core::api::ExtensionHost>),
    /// The background updater installed a new version.
    Updated(String),
    /// A provider model-list refresh finished; rebuild an open picker.
    CatalogRefreshed,
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
    /// Background job narration (login flows) into the transcript.
    jobs: tokio::sync::mpsc::Sender<String>,
    /// How a login flow ended; control flow reads this, never the notices.
    logins: tokio::sync::mpsc::Sender<crate::core::auth::login::Outcome>,
    /// Extension host; commands and prompts come back on `results`.
    host: std::sync::Arc<crate::core::api::ExtensionHost>,
    results: tokio::sync::mpsc::Sender<AppJob>,
    /// A /compact summary is being generated; cleared when it lands or fails.
    compacting: bool,
    /// /compact was asked for mid-turn; runs when the turn ends (the
    /// reference behavior — compaction never touches a running turn).
    compact_requested: bool,
    /// Messages typed while compacting; submitted once the swap lands.
    held_prompts: Vec<String>,
    /// First visit to this directory: the trust question, until answered.
    trust: Option<TrustStage>,
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
    /// A new version is installed on disk; /reload switches to it.
    update_installed: Option<String>,
    /// Exit the loop and exec the (updated) binary with -c.
    relaunch: bool,
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
        let data = StatusData {
            model: self.agent.model_slug(),
            effort: self.agent.effort(),
            session_name: None,
            context_percent: Some(percent),
            queued: 0,
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

    fn command_items(&self) -> Vec<MenuItem> {
        let mut items = vec![
            MenuItem::new(
                "/login",
                "sign in to a provider — account or API key",
                "/login",
            ),
            MenuItem::new("/models", "switch the model", "/models"),
            MenuItem::new(
                "/scoped-models",
                "choose which models ctrl+p cycles",
                "/scoped-models",
            ),
            MenuItem::new(
                "/reload",
                "reload extensions, themes, and config",
                "/reload",
            ),
            MenuItem::new("/resume", "resume a saved session", "/resume"),
            MenuItem::new("/new", "start a fresh session", "/new"),
            MenuItem::new("/copy", "copy the last reply", "/copy"),
            MenuItem::new("/compact", "summarize into a fresh session", "/compact"),
            MenuItem::new(
                "/trust",
                "trust this directory (loads its AGENTS.md, .e resources)",
                "/trust",
            ),
            MenuItem::new("/settings", "change preferences", "/settings"),
            MenuItem::new("/help", "show commands", "/help"),
            MenuItem::new("/version", "show the version", "/version"),
            MenuItem::new("/quit", "exit", "/quit"),
        ];
        for template in crate::core::resources::prompts::list(&self.agent.cwd()) {
            let slash = format!("/{}", template.name);
            let description = if template.argument_hint.is_empty() {
                template.description.clone()
            } else {
                format!("{} — {}", template.description, template.argument_hint)
            };
            items.push(MenuItem::new(&slash, &description, &slash));
        }
        for (name, description) in self.host.commands() {
            let slash = format!("/{name}");
            items.push(MenuItem::new(&slash, &description, &slash));
        }
        items
    }

    /// The scoped-models multi-select: every available model, Space toggling
    /// membership. The reference semantics: no scope stored = everything in
    /// scope; the first toggle narrows the scope to just that model.
    fn open_scoped_menu(&mut self) {
        let available = model::available();
        if available.is_empty() {
            self.notice("no models available — use /login to sign in to a provider".into());
            return;
        }
        let scope = model::scope();
        let mut available = model::provider_grouped(available);
        // The scoped entries lead the list — what you curated, not a hunt.
        if let Some(ids) = &scope {
            available.sort_by_key(|m| !ids.contains(&model::slug(m)));
        }
        let items: Vec<MenuItem> = available
            .iter()
            .map(|m| {
                let slug = model::slug(m);
                let mut item = MenuItem::new(&m.id, &m.provider, &slug);
                let in_scope = match &scope {
                    Some(ids) => ids.contains(&slug),
                    None => true,
                };
                if in_scope {
                    item.meta = "in scope".into();
                }
                item
            })
            .collect();
        self.menu = Some(Menu::new(
            MenuKind::Scoped,
            "Scoped models",
            HINT_SCOPED,
            items,
        ));
    }

    /// Space on the scoped picker: reference toggle semantics — no scope yet
    /// means the first toggle starts a scope of exactly that model.
    fn toggle_scoped(&mut self) {
        let Some(menu) = &mut self.menu else { return };
        let Some(slug) = menu.current().map(|i| i.value.clone()) else {
            return;
        };
        match model::scope() {
            None => {
                model::set_scope(std::slice::from_ref(&slug));
                menu.for_each_item(|item| {
                    item.meta = if item.value == slug {
                        "in scope".into()
                    } else {
                        String::new()
                    };
                });
            }
            Some(mut ids) => {
                let meta = if let Some(pos) = ids.iter().position(|id| *id == slug) {
                    ids.remove(pos);
                    ""
                } else {
                    ids.push(slug.clone());
                    "in scope"
                };
                if let Some(item) = menu.current_mut() {
                    item.meta = meta.into();
                }
                model::set_scope(&ids);
            }
        }
    }

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
    }

    fn open_resume_menu(&mut self) {
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
        let messages = match crate::core::session::Session::load(&path) {
            Ok(m) => m,
            Err(e) => {
                self.notice(format!("could not open session: {e}"));
                return;
            }
        };
        self.transcript.clear();
        self.transcript
            .push(Block::new(Kind::Banner, crate::VERSION));
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
        self.agent.load_history(messages);
        if let Ok(s) = crate::core::session::Session::reopen(&path) {
            self.agent.set_session(Some(s))
        }
        self.notice(format!(
            "resumed {}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
    }

    fn open_settings(&mut self) {
        self.menu = None;
        self.settings = Some(crate::tui::settingspanel::SettingsPanel::new(
            self.agent.efforts(),
        ));
    }

    fn open_model_menu(&mut self) {
        // Instant discovery where it matters: show the cached list now, ask
        // the gateways in the background (60s floor), and pop new rows into
        // the open picker when the answer lands.
        let results = self.results.clone();
        tokio::spawn(async move {
            crate::core::provider::catalog::refresh_remote_within(60_000).await;
            let _ = results.send(AppJob::CatalogRefreshed).await;
        });
        self.build_model_menu();
    }

    /// The picker itself, from the current catalog — no refresh side effects,
    /// so the rebuild-on-refresh arm cannot loop.
    fn build_model_menu(&mut self) {
        let available = model::provider_grouped(model::available());
        if available.is_empty() {
            self.notice("no models available — use /login to sign in to a provider".into());
            return;
        }
        let current = self.agent.model_slug();
        let items = available
            .iter()
            .map(|m| {
                let slug = model::slug(m);
                let mut item = MenuItem::new(&m.id, &m.provider, &slug);
                if slug == current {
                    item.meta = "current".into();
                }
                item
            })
            .collect();
        self.menu = Some(Menu::new(MenuKind::Models, "Models", HINT_USE, items));
    }

    /// /login: the sign-in panel — the flow's method choice in the auth
    /// surface's own look.
    fn open_login_menu(&mut self) {
        self.menu = None;
        self.auth = Some(AuthStage::Choose { selected: 0 });
    }

    /// A choice made on the panel. One account provider and one key provider
    /// today, so provider steps collapse straight through.
    fn auth_choose(&mut self, selected: usize) {
        self.auth = Some(if selected == 0 {
            AuthStage::Account { selected: 0 }
        } else {
            AuthStage::Key { selected: 0 }
        });
    }

    /// A subscription picked on the account panel — the registry names the
    /// flow; this just dispatches it.
    fn auth_account(&mut self, selected: usize) {
        let providers = crate::core::provider::registry::oauth_providers();
        let Some(provider) = providers.get(selected) else {
            return;
        };
        self.auth = Some(AuthStage::Waiting);
        self.notice(format!("starting the {} sign-in…", provider.display));
        match provider.auth.oauth.as_deref() {
            Some("xai-device") => {
                tokio::spawn(crate::core::auth::login::xai_login(
                    self.jobs.clone(),
                    self.logins.clone(),
                ));
            }
            _ => {
                tokio::spawn(crate::core::auth::login::codex_login(
                    provider.name.clone(),
                    self.jobs.clone(),
                    self.logins.clone(),
                ));
            }
        }
    }

    /// A provider picked on the API-key panel.
    fn auth_key(&mut self, selected: usize) {
        let providers = crate::core::provider::registry::key_providers();
        let Some(provider) = providers.get(selected) else {
            return;
        };
        self.auth = Some(AuthStage::ApiKey {
            provider: provider.name.clone(),
        });
        self.pending_key = Some(provider.name.clone());
        self.editor.mask = true;
        self.editor.set_text("");
    }

    fn open_skills_menu(&mut self, query: &str) {
        let items: Vec<MenuItem> = crate::core::resources::skills::list(&self.agent.cwd())
            .into_iter()
            .map(|s| MenuItem::new(&s.name, &s.description, &s.name))
            .collect();
        if items.is_empty() {
            return;
        }
        let mut menu = Menu::new(MenuKind::Skills, "Skills", HINT_USE, items);
        menu.set_query(query);
        self.menu = Some(menu);
    }

    fn open_file_menu(&mut self, query: &str) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let items = crate::core::workspace::list_files(&cwd)
            .into_iter()
            .map(|path| MenuItem::new(&path, "", &path))
            .collect();
        let mut menu = Menu::new(MenuKind::Files, "Files", HINT_USE, items);
        menu.set_query(query);
        self.menu = Some(menu);
    }

    /// Keep pickers in sync with the composer text: `/` at the start opens
    /// the command picker, an `@word` under the cursor the file picker.
    fn sync_menu(&mut self) {
        let text = self.editor.text();
        if self.pending_key.is_some() {
            return;
        }
        // Slash picker: leading '/', no space yet.
        if text.starts_with('/') && !text.contains(' ') && !text.contains('\n') {
            let query = text[1..].to_string();
            match &mut self.menu {
                Some(m) if m.kind == MenuKind::Commands => m.set_query(&query),
                _ => {
                    let mut menu = Menu::new(
                        MenuKind::Commands,
                        "Commands",
                        HINT_USE,
                        self.command_items(),
                    );
                    menu.set_query(&query);
                    self.menu = Some(menu);
                }
            }
            return;
        }
        // File picker: the last token starts with '@'.
        if let Some(token) = text
            .split_whitespace()
            .last()
            .filter(|t| t.starts_with('@'))
        {
            let query = token[1..].to_string();
            match &mut self.menu {
                Some(m) if m.kind == MenuKind::Files => m.set_query(&query),
                _ => self.open_file_menu(&query),
            }
            return;
        }
        // Skills picker: the last token starts with '$'.
        if let Some(token) = text
            .split_whitespace()
            .last()
            .filter(|t| t.starts_with('$'))
        {
            let query = token[1..].to_string();
            match &mut self.menu {
                Some(m) if m.kind == MenuKind::Skills => m.set_query(&query),
                _ => self.open_skills_menu(&query),
            }
            return;
        }
        // Auto pickers close when their trigger text is gone.
        if matches!(
            self.menu.as_ref().map(|m| m.kind),
            Some(MenuKind::Commands) | Some(MenuKind::Files) | Some(MenuKind::Skills)
        ) {
            self.menu = None;
        }
    }

    /// Enter on an open picker. Returns true when the key was consumed.
    fn select_menu(&mut self) -> bool {
        let Some(menu) = &self.menu else { return false };
        let Some(item) = menu.current().cloned() else {
            self.menu = None;
            return true;
        };
        let kind = menu.kind;
        self.menu = None;
        match kind {
            MenuKind::Commands => {
                self.editor.set_text("");
                self.dispatch_command(item.value);
            }
            MenuKind::Files => {
                // Replace the @token under construction with the chosen path.
                let text = self.editor.text();
                let replaced = match text.rfind('@') {
                    Some(at) => format!("{}{}", &text[..at], item.value),
                    None => item.value,
                };
                self.editor.set_text(&replaced);
            }
            MenuKind::Sessions => {
                self.resume_path(std::path::PathBuf::from(item.value));
            }
            MenuKind::Scoped => {}
            MenuKind::Skills => {
                // Replace the $token, then send the skill body as context.
                let text = self.editor.text();
                let rest = match text.rfind('$') {
                    Some(at) => text[..at].trim_end().to_string(),
                    None => String::new(),
                };
                self.editor.set_text("");
                if let Some(skill) =
                    crate::core::resources::skills::get(&item.value, &self.agent.cwd())
                {
                    let combined = if rest.is_empty() {
                        skill.body
                    } else {
                        format!("{}\n\n{rest}", skill.body)
                    };
                    self.prompt(combined);
                }
            }
            MenuKind::Models => {
                if let Some(found) = model::resolve(&item.value) {
                    persist_model(&found);
                    self.notice(format!("model set to {}", model::slug(&found)));
                    self.agent.model = found;
                }
            }
        }
        true
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

        if let Some(secret_for) = self.pending_key.take() {
            // A pasted API key goes to auth.json and nowhere else — in
            // particular not into the composer's recall history below.
            self.auth = None;
            self.editor.mask = false;
            match crate::core::auth::login::save_api_key(&secret_for, &trimmed) {
                Ok(()) => {
                    self.notice(format!("{secret_for}: key saved to ~/.e/auth.json"));
                    // An API-key sign-in is a sign-in: emit the same typed
                    // outcome the OAuth flows send, so the stranded-model
                    // re-pick happens here too, not only for browser logins.
                    let _ = self
                        .logins
                        .try_send(crate::core::auth::login::Outcome::SignedIn {
                            provider: secret_for.clone(),
                        });
                }
                Err(e) => self.notice(format!("{secret_for}: {e}")),
            }
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
            "/help" => self.notice(
                "commands:\n  /login [provider]   sign in (API key or account)\n  /models [name]      list or switch models\n  /scoped-models      choose which models ctrl+p cycles\n  /reload             reload extensions, themes, and config\n  /new                fresh session\n  /compact            summarize into a fresh session\n  /trust              trust this directory (loads its AGENTS.md, .e skills and prompts)\n  ! <cmd>              run a shell command; the model sees the output\n  shift+tab           cycle reasoning effort (per model)\n  /version            show the version\n  /quit               exit"
                    .into(),
            ),
            "/new" | "/clear" => {
                self.compacting = false;
                self.compact_requested = false;
                self.held_prompts.clear();
                self.shell_block = None;
                self.reload_block = None;
                self.context_tokens = 0;
                self.agent.clear();
                self.agent.set_session(None);
                self.transcript.clear();
                self.transcript.push(Block::new(Kind::Banner, crate::VERSION));
            }
            "/resume" => self.open_resume_menu(),
            "/settings" => self.open_settings(),
            "/copy" => self.copy_last(),
            "/compact" => self.compact_now(),
            "/reload" => self.reload(),
            "/trust" => match crate::core::config::trust::set(&self.agent.cwd(), true) {
                Ok(()) => self.notice("directory trusted — its AGENTS.md and .e skills/prompts now load".into()),
                Err(e) => self.notice(format!("trust: {e}")),
            },
            _ if trimmed.starts_with('/') => {
                let (name, args) = trimmed[1..].split_once(' ').unwrap_or((&trimmed[1..], ""));
                if let Some(template) =
                    crate::core::resources::prompts::find(name, &self.agent.cwd())
                {
                    let expanded = crate::core::resources::prompts::substitute(&template.content, args);
                    self.prompt(expanded);
                } else if self.host.has_command(name) {
                    let host = self.host.clone();
                    let results = self.results.clone();
                    let (name, args) = (name.to_string(), args.to_string());
                    tokio::spawn(async move {
                        let out = host.run_command(&name, &args).await;
                        if let Some(notice) = out.notice {
                            let _ = results.send(AppJob::Notice(notice)).await;
                        }
                        if let Some(prompt) = out.prompt {
                            let _ = results.send(AppJob::Prompt(prompt)).await;
                        }
                    });
                } else {
                    self.notice(format!("unknown command {trimmed}"));
                }
            }
            _ => self.prompt(trimmed),
        }
    }

    /// `/login` — bare lists providers and methods; with a provider, runs
    /// that provider's method: Account (browser OAuth) or API key (masked
    /// paste into the composer).
    fn login(&mut self, provider: String) {
        if provider.is_empty() {
            self.open_login_menu();
            return;
        }
        let flow =
            crate::core::provider::registry::find(&provider).and_then(|p| p.auth.oauth.clone());
        if flow.as_deref() == Some("codex") {
            self.notice(format!(
                "starting the {} sign-in…",
                model::display_name(&provider)
            ));
            tokio::spawn(crate::core::auth::login::codex_login(
                provider,
                self.jobs.clone(),
                self.logins.clone(),
            ));
        } else if flow.as_deref() == Some("xai-device") {
            self.notice(format!(
                "starting the {} sign-in…",
                model::display_name(&provider)
            ));
            tokio::spawn(crate::core::auth::login::xai_login(
                self.jobs.clone(),
                self.logins.clone(),
            ));
        } else {
            self.notice(format!(
                "paste the {provider} API key and press enter (esc cancels)"
            ));
            self.pending_key = Some(provider);
            self.editor.mask = true;
        }
    }

    fn prompt(&mut self, text: String) {
        // While compacting or reloading, hold the message; it submits after.
        if self.compacting || self.reloading {
            self.transcript.push(Block::new(Kind::User, text.clone()));
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

    /// The single session stream, in order. Turn bookkeeping hangs off it.
    fn on_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::TurnStart => {
                self.active = Some(ActiveTurn {
                    block: None,
                    text: String::new(),
                    turn: Turn::new(),
                    started: Instant::now(),
                    error: None,
                    tool_blocks: std::collections::HashMap::new(),
                    pending_tools: 0,
                    usage_seen: false,
                });
                // Seed the token counters from request size so the activity
                // row shows ↑ from the first second, like the reference.
                if let Some(s) = &mut self.active {
                    let chars: usize = system_prompt().chars().count()
                        + self
                            .agent
                            .history_snapshot()
                            .iter()
                            .map(|m| m.content.chars().count())
                            .sum::<usize>();
                    s.turn.input = (chars / 4) as u64;
                }
            }
            SessionEvent::Steered(text) => {
                // A mid-turn message: show it as a user turn where it landed.
                self.transcript.push(Block::new(Kind::User, text));
                // The next assistant text opens a fresh block.
                if let Some(s) = &mut self.active {
                    s.block = None;
                    s.text.clear();
                }
            }
            SessionEvent::TextDelta(delta) => {
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::AssistantText;
                    s.turn.note_text(&delta);
                    s.text.push_str(&delta);
                    let idx = match s.block {
                        Some(idx) => idx,
                        None => {
                            let idx = self.transcript.push(Block::new(Kind::Assistant, ""));
                            s.block = Some(idx);
                            idx
                        }
                    };
                    let text = s.text.clone();
                    if let Some(b) = self.transcript.blocks.get_mut(idx) {
                        b.text = text;
                        b.touch();
                    }
                }
            }
            // Reasoning summaries are counted toward the ↓ estimate but
            // never drawn — the reference transcript projects none.
            SessionEvent::ReasoningDelta(delta) => {
                if let Some(s) = &mut self.active {
                    s.turn.note_text(&delta);
                }
            }
            SessionEvent::ToolBatchStart { calls } => {
                if let Some(s) = &mut self.active {
                    s.block = None;
                    s.text.clear();
                    s.turn.phase = TurnPhase::Tool;
                    s.pending_tools += calls.len();
                    let children = calls
                        .iter()
                        .map(|call| {
                            crate::tui::transcript::ToolChild::pending(
                                call.id,
                                call.category.clone(),
                                call.running.clone(),
                                call.completed.clone(),
                                call.target.clone(),
                            )
                        })
                        .collect();
                    let idx = self.transcript.push(Block::tool_group(children));
                    for call in calls {
                        s.tool_blocks.insert(call.id, idx);
                    }
                }
            }
            SessionEvent::ToolStart { id } => {
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Tool;
                    if let Some(&idx) = s.tool_blocks.get(&id) {
                        if let Some(block) = self.transcript.blocks.get_mut(idx) {
                            block.start_tool(id);
                        }
                    }
                }
            }
            SessionEvent::ToolOutput { id, chunk, .. } => {
                if let Some(s) = &mut self.active {
                    if let Some(&idx) = s.tool_blocks.get(&id) {
                        if let Some(block) = self.transcript.blocks.get_mut(idx) {
                            block.append_tool_output(id, &chunk);
                        }
                    }
                }
            }
            SessionEvent::ToolEnd {
                id,
                outcome,
                summary,
                content,
            } => {
                let mut title = None;
                if let Some(s) = &mut self.active {
                    if let Some(&idx) = s.tool_blocks.get(&id) {
                        if let Some(block) = self.transcript.blocks.get_mut(idx) {
                            if let Some(child) =
                                block.tool_children.iter().find(|child| child.id == id)
                            {
                                title = Some(if child.target.is_empty() {
                                    child.completed.clone()
                                } else {
                                    format!("{} {}", child.completed, child.target)
                                });
                            }
                            block.finish_tool(id, outcome, summary, &content);
                        }
                    }
                    s.pending_tools = s.pending_tools.saturating_sub(1);
                    s.turn.phase = if s.pending_tools == 0 {
                        TurnPhase::Thinking
                    } else {
                        TurnPhase::Tool
                    };
                }
                if !content.trim().is_empty() {
                    self.remember_output(
                        title.unwrap_or_else(|| "tool output".into()),
                        crate::core::tools::sanitize_display(&content),
                    );
                }
            }
            SessionEvent::Usage {
                input,
                output,
                cache_read,
            } => {
                self.context_tokens = input + cache_read + output;
                if let Some(s) = &mut self.active {
                    // The seed estimate holds the counters until the first
                    // real usage lands; from then on, accumulate per step.
                    if s.usage_seen {
                        s.turn.input += input + cache_read;
                        s.turn.output += output;
                    } else {
                        s.turn.input = input + cache_read;
                        s.turn.output = output;
                        s.usage_seen = true;
                    }
                }
            }
            SessionEvent::Retry {
                attempt,
                limit,
                delay_secs,
                cause,
                reason,
            } => {
                // Replaces the Thinking row in place — a live status, not a
                // scrollback notice: it's transient by nature and would
                // otherwise leave one permanent line per attempt behind.
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Retrying;
                    s.turn.retry = Some(RetryStatus {
                        attempt,
                        limit,
                        delay_secs,
                        cause,
                        reason,
                    });
                    s.turn.recovered = None;
                }
            }
            SessionEvent::Recovered { attempt, limit } => {
                if let Some(s) = &mut self.active {
                    s.turn.phase = TurnPhase::Thinking;
                    s.turn.retry = None;
                    s.turn.recovered = Some(RecoveredStatus {
                        attempt,
                        limit,
                        since: Instant::now(),
                    });
                }
            }
            SessionEvent::Error(message) => {
                if let Some(s) = &mut self.active {
                    s.error = Some(message);
                } else {
                    self.notice(format!("error: {message}"));
                }
            }
            SessionEvent::TurnEnd { aborted } => {
                self.agent.on_turn_end();
                if aborted {
                    // Every started or serially pending member reaches a
                    // terminal state; no ghost Running row survives Esc.
                    for block in &mut self.transcript.blocks {
                        if block.kind == Kind::ToolGroup {
                            block.cancel_unfinished_tools();
                        } else if block.kind == Kind::Tool && !block.done {
                            block.cancelled = true;
                            block.touch();
                        }
                    }
                }
                let Some(s) = self.active.take() else { return };
                // The reference grammar: a completed turn ends with a dim
                // duration-and-tokens row; a cancelled one says so instead.
                if aborted {
                    self.transcript.push(Block::new(Kind::System, "cancelled"));
                } else {
                    let tokens = if s.turn.input == 0 && s.turn.output == 0 {
                        String::new()
                    } else {
                        format!(
                            " (↑{} ↓{})",
                            format_tokens(s.turn.input),
                            format_tokens(s.turn.output)
                        )
                    };
                    self.transcript.push(Block::new(
                        Kind::Summary,
                        format!(
                            "{}{}",
                            format_duration(s.started.elapsed().as_millis() as u64),
                            tokens
                        ),
                    ));
                }
                if let Some(message) = s.error {
                    // A failed turn ends visibly: the error persists in error
                    // color below the trailer, never a vanishing status blip.
                    self.transcript
                        .push(Block::new(Kind::Error, format!("error: {message}")));
                }
                // Compaction runs between turns, never during one: a deferred
                // /compact fires here, and so does the auto threshold check
                // against real usage (window minus reserve).
                let over = crate::core::agent::compact::should_compact(
                    self.context_tokens,
                    self.agent.model.context_window,
                );
                if !aborted && (self.compact_requested || over) {
                    let auto = !self.compact_requested;
                    self.compact_requested = false;
                    self.start_compaction(auto);
                }
            }
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
        let (to_summarize, kept) = crate::core::agent::compact::split(&history);
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
            });
            let _ = results.send(AppJob::Shell { cmd, output }).await;
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

    /// Reload the theme from settings (auto detects the terminal).
    fn apply_theme(&mut self) {
        let detected = crate::tui::background::detect_light().unwrap_or(false);
        self.theme = crate::tui::theme::resolve(&crate::core::config::settings::theme(), detected);
        self.transcript.invalidate();
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

/// The tab title's path: the working directory, home-relative.
fn title_path() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && cwd.starts_with(&home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd
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

pub fn system_prompt() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    crate::core::agent::context::system_prompt(&cwd)
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
    // cursor — restore the terminal first, then report as usual.
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = terminal::disable_raw_mode();
            print!("\x1b[?2004l\x1b[?25h\r\n");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            default_hook(info);
        }));
    }

    // Raw mode first: background detection needs the reply un-line-buffered.
    terminal::enable_raw_mode()?;
    let _guard = RawGuard;
    execute!(
        std::io::stdout(),
        EnableBracketedPaste,
        // The kitty keyboard protocol: without it, terminals send plain Enter
        // for shift+enter and multi-line entry is unreachable.
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    let detected = crate::tui::background::detect_light().unwrap_or(false);
    let theme = crate::tui::theme::resolve(&crate::core::config::settings::theme(), detected);

    let (cols, rows) = terminal::size()?;
    let mut screen = Screen::new(cols, rows);
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
        jobs: jobs_tx,
        logins: logins_tx,
        host,
        results: results_tx,
        compacting: false,
        compact_requested: false,
        held_prompts: Vec::new(),
        trust: None,
        shell_block: None,
        reloading: false,
        reload_block: None,
        outputs: Vec::new(),
        viewer: None,
        update_installed: None,
        relaunch: false,
    };
    app.transcript
        .push(Block::new(Kind::Banner, crate::VERSION));
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
    tokio::spawn(crate::core::provider::catalog::refresh_remote());
    if crate::core::auth::load().is_empty() {
        app.notice(
            "no provider signed in — use /login to sign in with an account or API key".into(),
        );
    } else if let Some(wanted) = crate::core::config::settings::get_string("model") {
        let current = app.agent.model_slug();
        if wanted != current {
            app.notice(format!(
                "{wanted} is unavailable (provider not signed in) — using {current}"
            ));
        }
    }
    // -c continues this workspace's most recent session.
    let continue_flag = args.iter().any(|a| a == "-c" || a == "--continue");
    let resume_flag = args.iter().any(|a| a == "-r" || a == "--resume");
    let message_args: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let initial: String = message_args
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if resume_flag {
        // The reference behavior: launch straight into the session picker.
        app.open_resume_menu();
    } else if continue_flag {
        app.resume_recent();
    }

    // Terminal tab title: the custom glyph, a dot, the path.
    {
        let mut out = std::io::stdout();
        write!(out, "]0;𝑒 · {}", title_path())?;
        out.flush()?;
    }
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    // SIGTERM/SIGHUP (a kill, a closed tab) exit through the same cleanup as
    // /quit — the terminal is restored, the extension host shut down.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

    if !initial.trim().is_empty() {
        app.submit(initial);
    }
    screen.paint(&app.frame(screen.cols as usize))?;

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
                    TermEvent::Resize(c, r) => screen.resize(c, r),
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
                                    app.trust = None;
                                    if let Err(e) = crate::core::config::trust::set(&app.agent.cwd(), trusted) {
                                        app.notice(format!("trust: {e}"));
                                    } else if !trusted {
                                        app.notice("working untrusted — project AGENTS.md and .e skills/prompts ignored (/trust to allow)".into());
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
                            // A theme change applies immediately.
                            app.apply_theme();
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
                                    let n = crate::core::provider::registry::oauth_providers().len();
                                    *selected = (*selected + 1) % n.max(1);
                                }
                                (AuthStage::Key { selected }, KeyCode::Up) => {
                                    let n = crate::core::provider::registry::key_providers().len();
                                    *selected = (*selected + n - 1) % n.max(1);
                                }
                                (AuthStage::Key { selected }, KeyCode::Down) => {
                                    let n = crate::core::provider::registry::key_providers().len();
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
                                    app.auth = None;
                                    app.pending_key = None;
                                    app.editor.mask = false;
                                    app.editor.set_text("");
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
                                KeyCode::Esc => { app.menu = None; }
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
                            // whatever levels this model declares.
                            match app.agent.cycle_effort() {
                                Some(next) => app.notice(format!("reasoning effort: {next}")),
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
            event = session_events.recv() => {
                match event {
                    Some(e) => app.on_session_event(e),
                    None => break,
                }
            }
            job = results_rx.recv() => {
                match job {
                    Some(AppJob::Notice(notice)) => app.notice(notice),
                    Some(AppJob::Prompt(prompt)) => app.prompt(prompt),
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
                        finish_reload_notice(&mut app.transcript, app.reload_block.take());
                        for text in std::mem::take(&mut app.held_prompts) {
                            app.prompt(text);
                        }
                    }
                    Some(AppJob::Shell { cmd, output }) => {
                        // Display a trimmed tail in the live block; history
                        // gets the full (tool-truncated) output.
                        let display_output = crate::core::tools::sanitize_display(&output.content);
                        let shown: String = {
                            let lines: Vec<&str> = display_output.lines().collect();
                            let tail = &lines[lines.len().saturating_sub(20)..];
                            let mut text = tail.join("\n");
                            if lines.len() > 20 {
                                text = format!("… ({} more lines above)\n{text}", lines.len() - 20);
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
                    Some(crate::core::auth::login::Outcome::SignedIn { .. }) => {
                        if matches!(app.auth, Some(AuthStage::Waiting)) {
                            app.auth = None;
                        }
                        tokio::spawn(crate::core::provider::catalog::refresh_remote());
                        // A fresh credential may make new models available:
                        // if the current model's provider is still signed out,
                        // fall back to the first available model.
                        if !crate::core::auth::load().contains_key(&app.agent.model.provider) {
                            if let Some(m) = crate::core::provider::catalog::available().into_iter().next() {
                                app.notice(format!("model set to {}", crate::core::provider::catalog::slug(&m)));
                                app.agent.model = m;
                            }
                        }
                    }
                    Some(crate::core::auth::login::Outcome::Failed) => app.auth = None,
                    None => {}
                }
            }
            _ = sigterm.recv() => break,
            _ = sighup.recv() => break,
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
        let frame = if app.viewer.is_some() {
            app.viewer_frame(screen.cols as usize, screen.rows as usize)
        } else {
            app.frame(screen.cols as usize)
        };
        screen.paint(&frame)?;
        if app.should_quit {
            break;
        }
    }

    app.host.shutdown().await;
    let _ = execute!(
        std::io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste
    );
    drop(_guard);
    let mut out = std::io::stdout();
    write!(out, "\r\n\x1b[?25h")?;
    out.flush()?;
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

struct RawGuard;
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
