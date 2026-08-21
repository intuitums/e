//! The `e` binary — M2: the interactive frame.
//!
//! Frame = transcript lines + activity row + composer band + status row, fed
//! to the line differ. Echo-only until the provider lands (M3): submitting
//! text pushes a user block; slash commands /help /version /quit work.

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use futures::StreamExt;
use std::io::Write;
use std::time::{Duration, Instant};

use e_tui::app::blocks::{Block, Kind, Transcript};
use e_tui::app::editor::{Editor, EditorResult, Key};
use e_tui::app::status::{statusline, StatusData, Turn};
use e_tui::render::theme::{load_bundled, Theme};
use e_tui::term::screen::Screen;

struct App {
    theme: Theme,
    transcript: Transcript,
    editor: Editor,
    turn: Option<Turn>,
    status: StatusData,
    overlay: Option<String>,
    armed_at: Option<Instant>,
    should_quit: bool,
}

impl App {
    fn frame(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.transcript.render(&self.theme, width);
        if let Some(turn) = &self.turn {
            lines.push(String::new());
            lines.push(format!(" • {}", turn.label(turn.started_at.elapsed().as_secs())));
        }
        lines.extend(self.editor.render(&self.theme, width));
        lines.extend(statusline(
            &self.theme,
            &self.status,
            self.overlay.as_deref(),
            None,
            width,
        ));
        lines
    }
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
        println!("e {}", e_core::VERSION);
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
        turn: None,
        status: StatusData {
            model: "no model".into(),
            effort: None,
            session_name: None,
            context_percent: None,
            queued: 0,
            cwd: std::env::current_dir()?.display().to_string(),
        },
        overlay: None,
        armed_at: None,
        should_quit: false,
    };
    app.transcript.push(Block::new(Kind::Banner, e_core::VERSION));

    terminal::enable_raw_mode()?;
    let _guard = RawGuard;
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    let width = screen.cols as usize;
    screen.paint(&app.frame(width))?;

    loop {
        tokio::select! {
            maybe = events.next() => {
                let Some(Ok(event)) = maybe else { break };
                match event {
                    Event::Resize(c, r) => {
                        screen.resize(c, r);
                    }
                    Event::Key(k) if k.kind != crossterm::event::KeyEventKind::Release => {
                        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                        // App-level chords first.
                        if ctrl && k.code == KeyCode::Char('c') {
                            if !app.editor.is_empty() {
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
                            match app.editor.key(key) {
                                EditorResult::Submit(text) => submit(&mut app, text),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
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
        let width = screen.cols as usize;
        let frame = app.frame(width);
        screen.paint(&frame)?;
        if app.should_quit { break; }
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

fn submit(app: &mut App, text: String) {
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return;
    }
    app.editor.push_history(text);
    match trimmed.as_str() {
        "/quit" | "/exit" => {
            app.should_quit = true;
        }
        "/version" => {
            app.transcript.push(Block::new(Kind::Notice, format!("e {}", e_core::VERSION)));
        }
        _ => {
            app.transcript.push(Block::new(Kind::User, trimmed));
        }
    }
}

struct RawGuard;
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}
