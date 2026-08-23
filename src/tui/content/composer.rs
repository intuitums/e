//! The composer: a `┃`-railed editor at column zero, no rules, no tint.
//!
//! Fixed v1 key set: insert/delete, arrows (line movement in wrapped or
//! multi-line drafts, history at the edges), home/end, word-left/right,
//! ctrl+a/e/k/u/w, shift+enter (or alt+enter) newline. Anything else waits
//! until asked for.

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
    /// Inner width from the latest render; vertical arrow keys need the
    /// visual layout to know whether a line movement is possible.
    inner_width: Option<usize>,
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

/// Wrap one buffer into visual rows of at most `inner` columns. Breaks at
/// word boundaries whenever the row has one — a word that would cross the
/// edge comes down whole; only space-less runs hard-break mid-word.
fn layout_rows(chars: &[char], inner: usize) -> Vec<VisualRow> {
    let mut rows = Vec::new();
    let mut i = 0usize;
    loop {
        if i >= chars.len() {
            rows.push(VisualRow { start: i, end: i });
            break;
        }
        if chars[i] == '\n' {
            rows.push(VisualRow { start: i, end: i });
            i += 1;
            continue;
        }
        // Consume one visual row, remembering the last word boundary.
        let mut j = i;
        let mut col = 0usize;
        let mut brk: Option<usize> = None;
        while j < chars.len() && chars[j] != '\n' && col < inner {
            if chars[j].is_whitespace() {
                brk = Some(j + 1);
            }
            col += 1;
            j += 1;
        }
        if j < chars.len() && chars[j] != '\n' {
            // Row full with more line remaining: prefer the word boundary.
            let end = brk.filter(|&b| b > i).unwrap_or(j);
            rows.push(VisualRow { start: i, end });
            i = end;
        } else {
            rows.push(VisualRow { start: i, end: j });
            if j >= chars.len() {
                break;
            }
            i = j + 1; // step over the newline
        }
    }
    rows
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
            inner_width: None,
        }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
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

    /// Vertical arrows: move between visual rows of the draft when there is
    /// one above/below — preserving the column where possible — otherwise
    /// fall through to input history recall, the single-line behavior.
    fn move_line(&mut self, direction: isize) {
        let Some(inner) = self.inner_width else {
            return;
        };
        let chars: Vec<char> = self.text.clone();
        let rows = layout_rows(&chars, inner);
        let Some(index) = rows.iter().position(|row| row.contains(self.cursor)) else {
            return;
        };
        let target = match direction {
            -1 if index > 0 => Some(&rows[index - 1]),
            1 if index + 1 < rows.len() => Some(&rows[index + 1]),
            _ => None,
        };
        if let Some(target) = target {
            let col = self.cursor.saturating_sub(rows[index].start);
            self.cursor = (target.start + col).min(target.end);
            return;
        }
        // At the top or bottom edge: history recall.
        match direction {
            -1 => {
                if self.history.is_empty() {
                    return;
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
            }
            _ => match self.history_pos {
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
                None => {}
            },
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
            Key::Up => {
                self.move_line(-1);
                Consumed
            }
            Key::Down => {
                self.move_line(1);
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
        }
    }

    /// Render the composer band: leading blank, then railed rows with a
    /// reverse-video cursor cell.
    pub fn render(&mut self, theme: &Theme, width: usize) -> Vec<String> {
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
        self.inner_width = Some(inner);
        let mut out = vec![String::new()];

        // Logical lines wrap at word boundaries to `inner`-wide visual rows,
        // every row carrying the rail. The cursor maps to its visual row.
        let text = if self.mask {
            "•".repeat(self.text.len())
        } else {
            self.text()
        };
        let chars: Vec<char> = text.chars().collect();
        let rows = layout_rows(&chars, inner);
        let last = rows.len() - 1;
        for (index, row) in rows.iter().enumerate() {
            let slice = &chars[row.start..row.end];
            let full_final_row = index == last
                && self.cursor == self.text.len()
                && self.cursor == row.end
                && row.end - row.start == inner;
            let cursor_here = self.cursor >= row.start && self.cursor <= row.end;
            let rendered = if cursor_here && !full_final_row {
                let at = self.cursor - row.start;
                let before: String = slice[..at].iter().collect();
                let cursor_char = slice
                    .get(at)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| " ".into());
                let after: String = slice[at..].iter().skip(1).collect();
                format!("{before}\x1b[7m{cursor_char}\x1b[27m{after}")
            } else {
                slice.iter().collect()
            };
            out.push(format!("{rail}{rendered}"));
        }
        // A cursor resting past a full final row needs one extra empty row.
        if rows.last().is_some_and(|row| {
            self.cursor == self.text.len()
                && !self.text.is_empty()
                && self.cursor == row.end
                && row.end - row.start == inner
        }) {
            out.push(format!("{rail}\x1b[7m \x1b[27m"));
        }
        out
    }
}

/// One visual row: an absolute char range in the buffer. Newline characters
/// belong to no row — they are zero-width row terminators.
struct VisualRow {
    start: usize,
    end: usize,
}

impl VisualRow {
    /// A newline-positioned cursor belongs to the row it terminates.
    fn contains(&self, cursor: usize) -> bool {
        cursor >= self.start && cursor <= self.end
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
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    KillToEnd,
    KillToStart,
    KillWord,
}
