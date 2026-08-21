//! The `e` binary: CLI subcommands (auth, models) and the interactive frame.
//!
//! The frame streams real turns: submit → provider request → deltas fold into
//! the tail assistant block live → usage lands in the turn trailer. Esc
//! aborts the in-flight request; history is in-memory until sessions land.

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode,
    KeyEvent, KeyModifiers,
};
use crossterm::{execute, terminal};
use futures::StreamExt;
use std::io::Write;
use std::time::{Duration, Instant};

use e::core::agent::{Agent, SessionEvent};
use e::core::model::{self, Model};
use e::core::output::{format_duration, format_tokens};
use e::tui::authpanel::{self, AuthStage};
use e::tui::composer::{Editor, EditorResult, Key};
use e::tui::menu::{Menu, MenuItem, MenuKind, HINT_USE};
use e::tui::screen::Screen;
use e::tui::statusline::{statusline, StatusData, Turn};
use e::tui::theme::Theme;
use e::tui::transcript::{Block, Kind, Transcript};

/// Per-turn frontend bookkeeping; the engine state lives in the Agent.
struct ActiveTurn {
    /// The current assistant text block, if one is streaming.
    block: Option<usize>,
    text: String,
    turn: Turn,
    started: Instant,
    error: Option<String>,
    /// tool id → transcript block index, so ToolEnd updates the right row.
    tool_blocks: std::collections::HashMap<u64, usize>,
    /// The live reasoning block, shown dimmed while the model thinks.
    reasoning: Option<usize>,
}

/// Asynchronous work landing back in the frame loop.
enum AppJob {
    /// A line for the transcript (login progress, extension notify…).
    Notice(String),
    /// A prompt an extension command asked to submit as the user.
    Prompt(String),
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
    settings: Option<e::tui::settingspanel::SettingsPanel>,
    /// Background job narration (login flows) into the transcript.
    jobs: tokio::sync::mpsc::Sender<String>,
    /// Extension host; commands and prompts come back on `results`.
    host: std::sync::Arc<e::core::api::ExtensionHost>,
    results: tokio::sync::mpsc::Sender<AppJob>,
}

impl App {
    fn frame(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.transcript.render(&self.theme, width);
        if let Some(s) = &self.active {
            lines.push(String::new());
            lines.push(format!(
                " • {}",
                s.turn.label(s.started.elapsed().as_secs())
            ));
        }
        let entering_key = matches!(self.auth, Some(AuthStage::ApiKey { .. }));
        if !entering_key {
            lines.extend(self.editor.render(&self.theme, width));
        }
        if let Some(stage) = &self.auth {
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
            .map(|_| e::tui::settingspanel::HINT)
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
            MenuItem::new("/model", "switch the model", "/model"),
            MenuItem::new("/resume", "resume a saved session", "/resume"),
            MenuItem::new("/new", "start a fresh session", "/new"),
            MenuItem::new("/copy", "copy the last reply", "/copy"),
            MenuItem::new("/settings", "change preferences", "/settings"),
            MenuItem::new("/help", "show commands", "/help"),
            MenuItem::new("/version", "show the version", "/version"),
            MenuItem::new("/quit", "exit", "/quit"),
        ];
        for (name, description) in self.host.commands() {
            let slash = format!("/{name}");
            items.push(MenuItem::new(&slash, &description, &slash));
        }
        items
    }

    fn open_resume_menu(&mut self) {
        let cwd = self.agent.cwd();
        let items: Vec<MenuItem> = e::core::session::list(&cwd)
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
        match e::core::session::most_recent(&cwd) {
            Some(path) => self.resume_path(path),
            None => self.notice("no saved sessions for this workspace".into()),
        }
    }

    fn resume_path(&mut self, path: std::path::PathBuf) {
        let messages = match e::core::session::Session::load(&path) {
            Ok(m) => m,
            Err(e) => {
                self.notice(format!("could not open session: {e}"));
                return;
            }
        };
        self.transcript.clear();
        self.transcript.push(Block::new(Kind::Banner, e::VERSION));
        for m in &messages {
            match m.role.as_str() {
                "user" => {
                    self.transcript
                        .push(Block::new(Kind::User, m.content.clone()));
                }
                "assistant" if !m.content.trim().is_empty() => {
                    self.transcript
                        .push(Block::new(Kind::Assistant, m.content.clone()));
                }
                _ => {}
            }
        }
        self.agent.load_history(messages);
        if let Ok(s) = e::core::session::Session::reopen(&path) {
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
        self.settings = Some(e::tui::settingspanel::SettingsPanel::new());
    }

    fn open_model_menu(&mut self) {
        let current = self.agent.model_slug();
        let items = model::catalog()
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
        if selected == 0 {
            self.auth = Some(AuthStage::Waiting);
            self.notice("starting the openai-codex sign-in…".into());
            tokio::spawn(e::core::login::codex_login(
                "openai-codex".into(),
                self.jobs.clone(),
            ));
        } else {
            self.auth = Some(AuthStage::ApiKey {
                provider: "opencode-go".into(),
            });
            self.pending_key = Some("opencode-go".into());
            self.editor.mask = true;
            self.editor.set_text("");
        }
    }

    fn open_skills_menu(&mut self, query: &str) {
        let items: Vec<MenuItem> = e::core::skills::list()
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
        let items = e::core::workspace::list_files(&cwd)
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
            MenuKind::Skills => {
                // Replace the $token, then send the skill body as context.
                let text = self.editor.text();
                let rest = match text.rfind('$') {
                    Some(at) => text[..at].trim_end().to_string(),
                    None => String::new(),
                };
                self.editor.set_text("");
                if let Some(skill) = e::core::skills::get(&item.value) {
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
            "/model" => self.open_model_menu(),
            "/settings" => self.open_settings(),
            "/resume" => self.open_resume_menu(),
            "/copy" => self.copy_last(),
            other => self.submit(other.to_string()),
        }
    }

    fn submit(&mut self, text: String) {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.editor.push_history(text);

        if let Some(secret_for) = self.pending_key.take() {
            self.auth = None;
            self.editor.mask = false;
            match e::core::login::save_api_key(&secret_for, &trimmed) {
                Ok(()) => self.notice(format!("{secret_for}: key saved to ~/.e/auth.json")),
                Err(e) => self.notice(format!("{secret_for}: {e}")),
            }
            return;
        }

        if let Some(rest) = trimmed.strip_prefix("/login") {
            let provider = rest.trim().to_string();
            if provider.is_empty() {
                self.open_login_menu();
            } else {
                self.login(provider);
            }
            return;
        }

        if let Some(rest) = trimmed.strip_prefix("/model") {
            let query = rest.trim();
            if query.is_empty() {
                self.open_model_menu();
            } else if let Some(found) = model::resolve(query) {
                persist_model(&found);
                self.notice(format!("model set to {}", model::slug(&found)));
                self.agent.model = found;
            } else {
                self.notice(format!("no model matches {query:?} — see `e models`"));
            }
            return;
        }
        match trimmed.as_str() {
            "/quit" | "/exit" => self.should_quit = true,
            "/version" => self.notice(format!("e {}", e::VERSION)),
            "/help" => self.notice(
                "commands:\n  /login [provider]   sign in (API key or account)\n  /model [name]       list or switch models\n  /new                fresh session\n  /version            show the version\n  /quit               exit"
                    .into(),
            ),
            "/new" | "/clear" => {
                self.context_tokens = 0;
                self.agent.clear();
                self.agent.set_session(None);
                self.transcript.clear();
                self.transcript.push(Block::new(Kind::Banner, e::VERSION));
            }
            "/resume" => self.open_resume_menu(),
            "/settings" => self.open_settings(),
            "/copy" => self.copy_last(),
            _ if trimmed.starts_with('/') => {
                let (name, args) = trimmed[1..].split_once(' ').unwrap_or((&trimmed[1..], ""));
                if self.host.has_command(name) {
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
        if provider == "openai-codex" {
            self.notice("starting the openai-codex sign-in…".into());
            tokio::spawn(e::core::login::codex_login(provider, self.jobs.clone()));
        } else {
            self.notice(format!(
                "paste the {provider} API key and press enter (esc cancels)"
            ));
            self.pending_key = Some(provider);
            self.editor.mask = true;
        }
    }

    fn prompt(&mut self, text: String) {
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
                    reasoning: None,
                });
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
                    s.reasoning = None;
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
            SessionEvent::ReasoningDelta(delta) => {
                if let Some(s) = &mut self.active {
                    let idx = match s.reasoning {
                        Some(idx) => idx,
                        None => {
                            let idx = self.transcript.push(Block::new(Kind::Reasoning, ""));
                            s.reasoning = Some(idx);
                            idx
                        }
                    };
                    if let Some(b) = self.transcript.blocks.get_mut(idx) {
                        b.text.push_str(&delta);
                        b.touch();
                    }
                }
            }
            SessionEvent::ToolStart { id, verb, target } => {
                if let Some(s) = &mut self.active {
                    // A new tool ends the current text block; text after tools
                    // opens a fresh one.
                    s.block = None;
                    s.reasoning = None;
                    s.text.clear();
                    s.turn.note_tool_verb(&verb);
                    let mut block = Block::new(Kind::Tool, verb);
                    block.detail = Some(target);
                    let idx = self.transcript.push(block);
                    s.tool_blocks.insert(id, idx);
                }
            }
            SessionEvent::ToolEnd {
                id,
                summary,
                is_error,
            } => {
                if let Some(s) = &mut self.active {
                    if let Some(&idx) = s.tool_blocks.get(&id) {
                        if let Some(b) = self.transcript.blocks.get_mut(idx) {
                            b.done = true;
                            b.is_error = is_error;
                            b.detail = Some(match b.detail.take() {
                                Some(t) if !t.is_empty() => format!("{t}  ({summary})"),
                                _ => summary,
                            });
                            b.touch();
                        }
                    }
                }
            }
            SessionEvent::Usage {
                input,
                output,
                cache_read,
            } => {
                self.context_tokens = input + cache_read;
                if let Some(s) = &mut self.active {
                    s.turn.input += input + cache_read;
                    s.turn.output += output;
                }
            }
            SessionEvent::Retry { attempt, message } => {
                self.notice(format!("retrying ({attempt}/2): {message}"));
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
                let Some(s) = self.active.take() else { return };
                let tokens = if s.turn.input == 0 && s.turn.output == 0 {
                    String::new()
                } else {
                    format!(
                        " (↑{} ↓{})",
                        format_tokens(s.turn.input),
                        format_tokens(s.turn.output)
                    )
                };
                let mark = if aborted { " · interrupted" } else { "" };
                self.transcript.push(Block::new(
                    Kind::Summary,
                    format!(
                        "{}{}{}",
                        format_duration(s.started.elapsed().as_millis() as u64),
                        tokens,
                        mark
                    ),
                ));
                if let Some(message) = s.error {
                    self.notice(format!("error: {message}"));
                }
            }
        }
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
                let ok = std::process::Command::new("pbcopy")
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut c| {
                        use std::io::Write;
                        c.stdin.take().unwrap().write_all(text.as_bytes())?;
                        c.wait()
                    })
                    .is_ok();
                self.notice(if ok {
                    "copied the last reply".into()
                } else {
                    "copy failed (no pbcopy)".into()
                });
            }
            None => self.notice("nothing to copy yet".into()),
        }
    }

    /// Reload the theme from settings (auto detects the terminal).
    fn apply_theme(&mut self) {
        let detected = e::tui::background::detect_light().unwrap_or(false);
        self.theme = e::tui::theme::resolve(&e::core::settings::theme(), detected);
        self.transcript.invalidate();
    }

    fn notice(&mut self, text: String) {
        self.transcript.push(Block::new(Kind::Notice, text));
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

fn system_prompt() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    e::core::context::system_prompt(&cwd)
}

fn persist_model(m: &Model) {
    e::core::settings::set_string("model", &model::slug(m));
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
        (KeyCode::Up, ..) => Key::HistoryPrev,
        (KeyCode::Down, ..) => Key::HistoryNext,
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

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("e {}", e::VERSION);
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("auth") {
        e::core::login::auth_status();
        return Ok(());
    }

    // Raw mode first: background detection needs the reply un-line-buffered.
    terminal::enable_raw_mode()?;
    let _guard = RawGuard;
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    let detected = e::tui::background::detect_light().unwrap_or(false);
    let theme = e::tui::theme::resolve(&e::core::settings::theme(), detected);

    let (cols, rows) = terminal::size()?;
    let mut screen = Screen::new(cols, rows);
    let (mut agent, mut session_events) = Agent::new(model::default_model());
    let (jobs_tx, mut jobs_rx) = tokio::sync::mpsc::channel::<String>(16);
    let (results_tx, mut results_rx) = tokio::sync::mpsc::channel::<AppJob>(16);
    let host = e::core::api::ExtensionHost::start(jobs_tx.clone()).await;
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
        host,
        results: results_tx,
    };
    app.transcript.push(Block::new(Kind::Banner, e::VERSION));
    // -c continues this workspace's most recent session.
    let continue_flag = args.iter().any(|a| a == "-c" || a == "--continue");
    let message_args: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let initial: String = message_args
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if continue_flag {
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
                        // A paste is one unit: insert literally, never triggering
                        // a picker or submit mid-block.
                        app.editor.insert_str(&text.replace('\r', "\n"));
                        app.sync_menu();
                    }
                    TermEvent::Resize(c, r) => screen.resize(c, r),
                    TermEvent::Key(k) if k.kind != crossterm::event::KeyEventKind::Release => {
                        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                        if let Some(panel) = &mut app.settings {
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
                        } else if ctrl && k.code == KeyCode::Char('d') && app.editor.is_empty() {
                            break;
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
                    None => {}
                }
            }
            message = jobs_rx.recv() => {
                if let Some(message) = message {
                    if matches!(app.auth, Some(AuthStage::Waiting)) && message.contains("signed in")
                        || message.contains("login failed")
                    {
                        app.auth = None;
                    }
                    app.notice(message);
                }
            }
            _ = tick.tick() => {
                if let Some(at) = app.armed_at {
                    if at.elapsed() > Duration::from_millis(1600) {
                        app.armed_at = None;
                        app.overlay = None;
                    }
                }
            }
        }
        let frame = app.frame(screen.cols as usize);
        screen.paint(&frame)?;
        if app.should_quit {
            break;
        }
    }

    app.host.shutdown().await;
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    drop(_guard);
    let mut out = std::io::stdout();
    write!(out, "\r\n\x1b[?25h")?;
    out.flush()?;
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
