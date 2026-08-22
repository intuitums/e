//! The composer: a `┃`-railed editor at column zero, no rules, no tint.
//!
//! Fixed v1 key set: insert/delete, arrows, home/end, word-left/right,
//! ctrl+a/e/k/u/w, history up/down, shift+enter (or alt+enter) newline.
//! Anything else waits until asked for.

use crate::tui::theme::Theme;

pub struct Editor {
    /// Render `•` per character — for secret entry (/login keys).
    pub mask: bool,
    /// Live paste placeholders: (token, full text), expanded on submit.
    pastes: Vec<(String, String)>,
    paste_seq: usize,
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
            pastes: Vec::new(),
            paste_seq: 0,
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

    /// A paste becomes a placeholder token — the reference behavior — when
    /// it is long or multiline; the token expands back on submit. Small
    /// single-line pastes insert literally.
    pub fn insert_paste(&mut self, text: &str) {
        let lines = text.lines().count().max(1);
        if lines == 1 && text.chars().count() <= 120 {
            self.insert_str(text);
            return;
        }
        self.paste_seq += 1;
        let token = format!(
            "[Pasted text #{}, {} line{}]",
            self.paste_seq,
            lines,
            if lines == 1 { "" } else { "s" }
        );
        self.pastes.push((token.clone(), text.to_string()));
        self.insert_str(&token);
    }

    /// Replace every live placeholder with its pasted content.
    pub fn expand_pastes(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (token, content) in &self.pastes {
            out = out.replace(token, content);
        }
        out
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
        // A draft starting with `!` is a shell command: the rail turns the
        // bash-mode color — the whole indicator, no words.
        let rail_token =
            if !self.mask && self.text.iter().find(|c| !c.is_whitespace()) == Some(&'!') {
                "bashMode"
            } else {
                "userMessageText"
            };
        let rail = format!("{} ", theme.fg(rail_token, "┃"));
        let inner = width.saturating_sub(2).max(8);
        let mut rows = vec![String::new()];

        // Logical lines wrap to `inner`-wide visual rows, every row carrying
        // the rail — the reference shape. The cursor maps to its visual row.
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
            let chars: Vec<char> = line.chars().collect();
            consumed += chars.len() + 1; // + the newline
            let cursor_here = self.cursor >= line_start
                && self.cursor <= line_start + chars.len()
                && (li + 1 == logical.len() || self.cursor < consumed);
            let cursor_col = if cursor_here {
                Some(self.cursor - line_start)
            } else {
                None
            };

            // Visual rows: chunks of `inner` chars; the cursor resting past a
            // full final chunk needs one extra empty row to sit on.
            let mut chunk_count = chars.len().div_ceil(inner).max(1);
            if cursor_col == Some(chars.len())
                && !chars.is_empty()
                && chars.len().is_multiple_of(inner)
            {
                chunk_count += 1;
            }
            for row in 0..chunk_count {
                let begin = row * inner;
                let slice: &[char] = if begin < chars.len() {
                    &chars[begin..(begin + inner).min(chars.len())]
                } else {
                    &[]
                };
                let rendered = match cursor_col {
                    Some(col) if col >= begin && col < begin + inner => {
                        let at = col - begin;
                        let before: String = slice[..at.min(slice.len())].iter().collect();
                        let cursor_char = slice
                            .get(at)
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| " ".into());
                        let after: String = if at < slice.len() {
                            slice[at + 1..].iter().collect()
                        } else {
                            String::new()
                        };
                        format!("{before}\x1b[7m{cursor_char}\x1b[27m{after}")
                    }
                    _ => slice.iter().collect(),
                };
                rows.push(format!("{rail}{rendered}"));
            }
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
