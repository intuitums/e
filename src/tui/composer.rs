//! The composer: a `┃`-railed editor at column zero, no rules, no tint.
//!
//! Fixed v1 key set: insert/delete, arrows, home/end, word-left/right,
//! ctrl+a/e/k/u/w, history up/down, shift+enter (or alt+enter) newline.
//! Anything else waits until asked for.

use crate::tui::markdown::visible_width;
use crate::tui::theme::Theme;

pub struct Editor {
    /// Render `•` per character — for secret entry (/login keys).
    pub mask: bool,
    text: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    history_pos: Option<usize>,
    draft: String,
}

pub enum EditorResult {
    Consumed,
    Submit(String),
    Ignored,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Editor {
            mask: false,
            text: Vec::new(),
            cursor: 0,
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
        }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn set_text(&mut self, s: &str) {
        self.text = s.chars().collect();
        self.cursor = self.text.len();
        self.history_pos = None;
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn push_history(&mut self, entry: String) {
        if !entry.trim().is_empty() {
            self.history.push(entry);
        }
        self.history_pos = None;
    }

    fn word_left(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && self.text[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !self.text[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn word_right(&self) -> usize {
        let mut i = self.cursor;
        while i < self.text.len() && self.text[i].is_whitespace() {
            i += 1;
        }
        while i < self.text.len() && !self.text[i].is_whitespace() {
            i += 1;
        }
        i
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert(c);
        }
    }

    pub fn key(&mut self, key: Key) -> EditorResult {
        use EditorResult::*;
        match key {
            Key::Char(c) => {
                self.insert(c);
                Consumed
            }
            Key::Enter => {
                let text = self.text();
                self.text.clear();
                self.cursor = 0;
                self.history_pos = None;
                Submit(text)
            }
            Key::Newline => {
                self.insert('\n');
                Consumed
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                }
                Consumed
            }
            Key::Delete => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
                Consumed
            }
            Key::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                Consumed
            }
            Key::Right => {
                self.cursor = (self.cursor + 1).min(self.text.len());
                Consumed
            }
            Key::WordLeft => {
                self.cursor = self.word_left();
                Consumed
            }
            Key::WordRight => {
                self.cursor = self.word_right();
                Consumed
            }
            Key::Home => {
                self.cursor = 0;
                Consumed
            }
            Key::End => {
                self.cursor = self.text.len();
                Consumed
            }
            Key::KillToEnd => {
                self.text.truncate(self.cursor);
                Consumed
            }
            Key::KillToStart => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
                Consumed
            }
            Key::KillWord => {
                let start = self.word_left();
                self.text.drain(start..self.cursor);
                self.cursor = start;
                Consumed
            }
            Key::HistoryPrev => {
                if self.history.is_empty() {
                    return Consumed;
                }
                match self.history_pos {
                    None => {
                        self.draft = self.text();
                        self.history_pos = Some(self.history.len() - 1);
                    }
                    Some(0) => {}
                    Some(p) => self.history_pos = Some(p - 1),
                }
                if let Some(p) = self.history_pos {
                    let entry = self.history[p].clone();
                    self.set_text(&entry);
                    self.history_pos = Some(p);
                }
                Consumed
            }
            Key::HistoryNext => {
                match self.history_pos {
                    None => {}
                    Some(p) if p + 1 < self.history.len() => {
                        let entry = self.history[p + 1].clone();
                        self.set_text(&entry);
                        self.history_pos = Some(p + 1);
                    }
                    Some(_) => {
                        let draft = self.draft.clone();
                        self.set_text(&draft);
                        self.history_pos = None;
                    }
                }
                Consumed
            }
        }
    }

    /// Render the composer band: leading blank, then railed rows with a
    /// reverse-video cursor cell.
    pub fn render(&self, theme: &Theme, width: usize) -> Vec<String> {
        let rail = format!("{} ", theme.fg("userMessageText", "┃"));
        let inner = width.saturating_sub(2).max(8);
        let mut rows = vec![String::new()];

        // Split into logical lines, tracking where the cursor falls.
        let text = if self.mask {
            "•".repeat(self.text.len())
        } else {
            self.text()
        };
        let mut consumed = 0usize;
        let logical: Vec<&str> = if text.is_empty() {
            vec![""]
        } else {
            text.split('\n').collect()
        };
        for (li, line) in logical.iter().enumerate() {
            let line_start = consumed;
            let line_len = line.chars().count();
            consumed += line_len + 1; // + the newline
            let cursor_here = self.cursor >= line_start
                && self.cursor <= line_start + line_len
                && (li + 1 == logical.len() || self.cursor < consumed);
            let mut rendered = String::new();
            if cursor_here {
                let col = self.cursor - line_start;
                let chars: Vec<char> = line.chars().collect();
                let before: String = chars[..col].iter().collect();
                let at: String = chars
                    .get(col)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| " ".into());
                let after: String = if col < chars.len() {
                    chars[col + 1..].iter().collect()
                } else {
                    String::new()
                };
                rendered = format!("{before}\x1b[7m{at}\x1b[27m{after}");
            } else {
                rendered.push_str(line);
            }
            // Naive width clamp: scroll horizontally so the cursor stays visible.
            if visible_width(&rendered) > inner {
                // v1: show the tail.
                let plain: Vec<char> = rendered.chars().collect();
                let keep: String = plain[plain.len().saturating_sub(inner + 8)..]
                    .iter()
                    .collect();
                rendered = keep;
            }
            rows.push(format!("{rail}{rendered}"));
        }
        rows
    }
}

pub enum Key {
    Char(char),
    Enter,
    Newline,
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    KillToEnd,
    KillToStart,
    KillWord,
    HistoryPrev,
    HistoryNext,
}
