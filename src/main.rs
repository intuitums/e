//! The `e` binary: CLI subcommands (auth, models) and the interactive frame.
//!
//! The frame streams real turns: submit → provider request → deltas fold into
//! the tail assistant block live → usage lands in the turn trailer. Esc
//! aborts the in-flight request; history is in-memory until sessions land.

use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use futures::StreamExt;
use std::io::Write;
use std::time::{Duration, Instant};

use e::core::agent::{Agent, SessionEvent};
use e::core::model::{self, Model};
use e::core::output::{format_duration, format_tokens};
use e::tui::composer::{Editor, EditorResult, Key};
use e::tui::statusline::{statusline, StatusData, Turn};
use e::tui::theme::{load_bundled, Theme};
use e::tui::transcript::{Block, Kind, Transcript};
use e::tui::screen::Screen;

/// Per-turn frontend bookkeeping; the engine state lives in the Agent.
struct ActiveTurn {
    block: usize,
    text: String,
    turn: Turn,
    started: Instant,
    error: Option<String>,
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
    /// Background job narration (login flows) into the transcript.
    jobs: tokio::sync::mpsc::Sender<String>,
}

impl App {
    fn frame(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.transcript.render(&self.theme, width);
        if let Some(s) = &self.active {
            lines.push(String::new());
            lines.push(format!(" • {}", s.turn.label(s.started.elapsed().as_secs())));
        }
        lines.extend(self.editor.render(&self.theme, width));
        let window = self.agent.model.context_window.max(1);
        let percent = ((self.context_tokens.saturating_mul(100)) / window).min(100) as u8;
        let data = StatusData {
            model: self.agent.model_slug(),
            effort: self.agent.effort(),
            session_name: None,
            context_percent: Some(percent),
            queued: 0,
        };
        lines.extend(statusline(&self.theme, &data, self.overlay.as_deref(), None, width));
        lines
    }

    fn submit(&mut self, text: String) {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.editor.push_history(text);

        if let Some(secret_for) = self.pending_key.take() {
            self.editor.mask = false;
            match e::core::login::save_api_key(&secret_for, &trimmed) {
                Ok(()) => self.notice(format!("{secret_for}: key saved to ~/.e/auth.json")),
                Err(e) => self.notice(format!("{secret_for}: {e}")),
            }
            return;
        }

        if let Some(rest) = trimmed.strip_prefix("/login") {
            self.login(rest.trim().to_string());
            return;
        }

        if let Some(rest) = trimmed.strip_prefix("/model") {
            let query = rest.trim();
            if query.is_empty() {
                let current = self.agent.model_slug();
                let mut listing = String::from("models:");
                for m in model::catalog() {
                    let slug = model::slug(&m);
                    let mark = if slug == current { "  ← current" } else { "" };
                    listing.push_str(&format!("\n  {slug}{mark}"));
                }
                listing.push_str("\n\nswitch with /model <name>");
                self.notice(listing);
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
                self.transcript.clear();
                self.transcript.push(Block::new(Kind::Banner, e::VERSION));
            }
            _ if trimmed.starts_with('/') => {
                self.notice(format!("unknown command {trimmed}"));
            }
            _ => self.prompt(trimmed),
        }
    }

    /// `/login` — bare lists providers and methods; with a provider, runs
    /// that provider's method: Account (browser OAuth) or API key (masked
    /// paste into the composer).
    fn login(&mut self, provider: String) {
        if provider.is_empty() {
            self.notice(
                "sign in with /login <provider>:\n  openai-codex   Account — opens the browser (ChatGPT sign-in)\n  opencode-go    API key — pasted here, stored in ~/.e/auth.json\n\nany other provider name is treated as an API-key provider"
                    .into(),
            );
            return;
        }
        if provider == "openai-codex" {
            self.notice("starting the openai-codex sign-in…".into());
            tokio::spawn(e::core::login::codex_login(provider, self.jobs.clone()));
        } else {
            self.notice(format!("paste the {provider} API key and press enter (esc cancels)"));
            self.pending_key = Some(provider);
            self.editor.mask = true;
        }
    }

    fn prompt(&mut self, text: String) {
        if self.agent.is_streaming() {
            self.notice("a turn is already streaming — esc to interrupt first".into());
            return;
        }
        self.transcript.push(Block::new(Kind::User, text.clone()));
        self.agent.prompt(text, system_prompt());
    }

    /// The single session stream, in order. Turn bookkeeping hangs off it.
    fn on_session_event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::TurnStart => {
                let block = self.transcript.push(Block::new(Kind::Assistant, ""));
                self.active = Some(ActiveTurn {
                    block,
                    text: String::new(),
                    turn: Turn::new(),
                    started: Instant::now(),
                    error: None,
                });
            }
            SessionEvent::TextDelta(delta) => {
                if let Some(s) = &mut self.active {
                    s.text.push_str(&delta);
                    let (idx, text) = (s.block, s.text.clone());
                    if let Some(b) = self.transcript.blocks.get_mut(idx) {
                        b.text = text;
                        b.touch();
                    }
                }
            }
            SessionEvent::ReasoningDelta(_) => {
                // Reasoning stays out of the transcript; the indicator says Thinking.
            }
            SessionEvent::Usage { input, output, cache_read } => {
                self.context_tokens = input + cache_read;
                if let Some(s) = &mut self.active {
                    s.turn.input += input + cache_read;
                    s.turn.output += output;
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
                let Some(s) = self.active.take() else { return };
                let tokens = if s.turn.input == 0 && s.turn.output == 0 {
                    String::new()
                } else {
                    format!(" (↑{} ↓{})", format_tokens(s.turn.input), format_tokens(s.turn.output))
                };
                let mark = if aborted { " · interrupted" } else { "" };
                self.transcript.push(Block::new(
                    Kind::Summary,
                    format!("{}{}{}", format_duration(s.started.elapsed().as_millis() as u64), tokens, mark),
                ));
                if let Some(message) = s.error {
                    self.notice(format!("error: {message}"));
                }
            }
        }
    }

    fn notice(&mut self, text: String) {
        self.transcript.push(Block::new(Kind::Notice, text));
    }
}

/// The tab title's path: the working directory, home-relative.
fn title_path() -> String {
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && cwd.starts_with(&home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd
    }
}

fn system_prompt() -> String {
    let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
    format!(
        "You are e, a fast, concise coding agent in a terminal. Answer in markdown. \
         Be direct; prefer short answers unless asked for depth.\nWorking directory: {cwd}"
    )
}

fn persist_model(m: &Model) {
    let path = e::core::home::settings_path();
    let mut value: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    value["model"] = serde_json::Value::String(model::slug(m));
    let _ = e::core::home::ensure();
    let _ = std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap_or_default());
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

    let light = std::env::var("COLORFGBG")
        .ok()
        .and_then(|v| v.rsplit(';').next().and_then(|n| n.parse::<u8>().ok()))
        .map(|bg| bg >= 7)
        .unwrap_or(false);
    let theme = load_bundled(light).map_err(std::io::Error::other)?;

    let (cols, rows) = terminal::size()?;
    let mut screen = Screen::new(cols, rows);
    let (agent, mut session_events) = Agent::new(model::default_model());
    let (jobs_tx, mut jobs_rx) = tokio::sync::mpsc::channel::<String>(16);
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
        jobs: jobs_tx,
    };
    app.transcript.push(Block::new(Kind::Banner, e::VERSION));
    // A message on the command line becomes the first prompt.
    let initial: String = args.join(" ");

    terminal::enable_raw_mode()?;
    let _guard = RawGuard;
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
                    TermEvent::Resize(c, r) => screen.resize(c, r),
                    TermEvent::Key(k) if k.kind != crossterm::event::KeyEventKind::Release => {
                        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                        if k.code == KeyCode::Esc && app.pending_key.is_some() {
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
            message = jobs_rx.recv() => {
                if let Some(message) = message {
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
