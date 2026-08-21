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

use e::core::model::{self, Model};
use e::core::output::{format_duration, format_tokens};
use e::core::provider::{self, ChatMessage, Event as ProviderEvent};
use e::tui::composer::{Editor, EditorResult, Key};
use e::tui::statusline::{statusline, StatusData, Turn};
use e::tui::theme::{load_bundled, Theme};
use e::tui::transcript::{Block, Kind, Transcript};
use e::tui::screen::Screen;

struct Streaming {
    rx: tokio::sync::mpsc::Receiver<ProviderEvent>,
    handle: tokio::task::JoinHandle<()>,
    block: usize,
    text: String,
    turn: Turn,
    started: Instant,
}

struct App {
    theme: Theme,
    transcript: Transcript,
    editor: Editor,
    model: Model,
    history: Vec<ChatMessage>,
    streaming: Option<Streaming>,
    overlay: Option<String>,
    armed_at: Option<Instant>,
    should_quit: bool,
}

impl App {
    fn frame(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.transcript.render(&self.theme, width);
        if let Some(s) = &self.streaming {
            lines.push(String::new());
            lines.push(format!(" • {}", s.turn.label(s.started.elapsed().as_secs())));
        }
        lines.extend(self.editor.render(&self.theme, width));
        let effort = if self.model.efforts.is_empty() { None } else { Some("high".to_string()) };
        let data = StatusData {
            model: model::slug(&self.model),
            effort,
            session_name: None,
            context_percent: None,
            queued: 0,
            cwd: std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
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

        if let Some(rest) = trimmed.strip_prefix("/model") {
            let query = rest.trim();
            if query.is_empty() {
                self.notice(format!("model: {}", model::slug(&self.model)));
            } else if let Some(found) = model::resolve(query) {
                persist_model(&found);
                self.notice(format!("model set to {}", model::slug(&found)));
                self.model = found;
            } else {
                self.notice(format!("no model matches {query:?} — see `e models`"));
            }
            return;
        }
        match trimmed.as_str() {
            "/quit" | "/exit" => self.should_quit = true,
            "/version" => self.notice(format!("e {}", e::VERSION)),
            "/new" | "/clear" => {
                self.history.clear();
                self.transcript.clear();
                self.transcript.push(Block::new(Kind::Banner, e::VERSION));
            }
            _ if trimmed.starts_with('/') => {
                self.notice(format!("unknown command {trimmed}"));
            }
            _ => self.prompt(trimmed),
        }
    }

    fn prompt(&mut self, text: String) {
        if self.streaming.is_some() {
            self.notice("a turn is already streaming — esc to interrupt first".into());
            return;
        }
        self.transcript.push(Block::new(Kind::User, text.clone()));
        self.history.push(ChatMessage { role: "user".into(), content: text });

        let block = self.transcript.push(Block::new(Kind::Assistant, ""));
        let effort = if self.model.efforts.is_empty() { None } else { Some("high".to_string()) };
        let (rx, handle) = provider::stream(provider::Request {
            model: self.model.clone(),
            system: system_prompt(),
            messages: self.history.clone(),
            effort,
        });
        self.streaming = Some(Streaming {
            rx,
            handle,
            block,
            text: String::new(),
            turn: Turn::new(),
            started: Instant::now(),
        });
    }

    fn on_provider_event(&mut self, event: ProviderEvent) {
        let Some(s) = &mut self.streaming else { return };
        match event {
            ProviderEvent::TextDelta(delta) => {
                s.text.push_str(&delta);
                let idx = s.block;
                let text = s.text.clone();
                if let Some(b) = self.transcript.blocks.get_mut(idx) {
                    b.text = text;
                    b.touch();
                }
            }
            ProviderEvent::ReasoningDelta(_) => {
                // Reasoning stays out of the transcript; the indicator says Thinking.
            }
            ProviderEvent::Usage { input, output, cache_read } => {
                s.turn.input += input + cache_read;
                s.turn.output += output;
            }
            ProviderEvent::Done => self.finish_turn(None),
            ProviderEvent::Error(message) => self.finish_turn(Some(message)),
        }
    }

    fn finish_turn(&mut self, error: Option<String>) {
        let Some(s) = self.streaming.take() else { return };
        if !s.text.is_empty() {
            self.history.push(ChatMessage { role: "assistant".into(), content: s.text.clone() });
        }
        let tokens = if s.turn.input == 0 && s.turn.output == 0 {
            String::new()
        } else {
            format!(" (↑{} ↓{})", format_tokens(s.turn.input), format_tokens(s.turn.output))
        };
        self.transcript.push(Block::new(
            Kind::Summary,
            format!("{}{}", format_duration(s.started.elapsed().as_millis() as u64), tokens),
        ));
        if let Some(message) = error {
            self.notice(format!("error: {message}"));
        }
    }

    fn interrupt(&mut self) {
        if let Some(s) = &self.streaming {
            s.handle.abort();
        }
        self.finish_turn(None);
    }

    fn notice(&mut self, text: String) {
        self.transcript.push(Block::new(Kind::Notice, text));
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
        let result = match args.get(1).map(String::as_str) {
            None | Some("status") => {
                e::core::login::auth_status();
                Ok(())
            }
            Some("openai-codex") => e::core::login::auth_codex("openai-codex").await,
            Some(provider) => e::core::login::auth_api_key(provider),
        };
        if let Err(message) = result {
            eprintln!("error: {message}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("models") {
        for m in e::core::model::catalog() {
            println!("{}", e::core::model::slug(&m));
        }
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
    let mut app = App {
        theme,
        transcript: Transcript::default(),
        editor: Editor::new(),
        model: model::default_model(),
        history: Vec::new(),
        streaming: None,
        overlay: None,
        armed_at: None,
        should_quit: false,
    };
    app.transcript.push(Block::new(Kind::Banner, e::VERSION));
    // A message on the command line becomes the first prompt.
    let initial: String = args.join(" ");

    terminal::enable_raw_mode()?;
    let _guard = RawGuard;
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
                        if k.code == KeyCode::Esc && app.streaming.is_some() {
                            app.interrupt();
                        } else if ctrl && k.code == KeyCode::Char('c') {
                            if app.streaming.is_some() {
                                app.interrupt();
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
            event = async {
                match &mut app.streaming {
                    Some(s) => s.rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match event {
                    Some(e) => app.on_provider_event(e),
                    None => app.finish_turn(None),
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
