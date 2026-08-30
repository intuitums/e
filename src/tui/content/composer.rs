//! The composer: a `┃`-railed editor at column zero, no rules, no tint.
//!
//! Fixed v1 key set: insert/delete, arrows (line movement in wrapped or
//! multi-line drafts, history at the edges), home/end, word-left/right,
//! ctrl+a/e/k/u/w, shift+enter (or alt+enter) newline. Anything else waits
//! until asked for.

use unicode_width::UnicodeWidthChar;

use crate::tui::theme::Theme;

pub struct Editor {
    /// Render `•` per character — for secret entry (/login keys).
    pub mask: bool,
    /// Live paste placeholders: (token, full text), expanded on submit.
    pastes: Vec<(String, String)>,
    paste_seq: usize,
    /// Pastes longer than this many codepoints collapse to a placeholder
    /// token; `0` disables collapsing. A user preference (see
    /// `settings::paste_placeholder`), not a constant.
    paste_limit: usize,
    text: Vec<char>,
    cursor: usize,
    /// Shift-arrow selection anchor: the fixed end while the cursor
    /// extends; None when nothing is selected.
    selection_anchor: Option<usize>,
    history: Vec<String>,
    history_pos: Option<usize>,
    draft: String,
    /// Inner width from the latest render; vertical arrow keys need the
    /// visual layout to know whether a line movement is possible.
    inner_width: Option<usize>,
    /// First visible visual row when the draft outgrows the composer's
    /// share of the frame — the window follows the cursor.
    scroll: usize,
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

/// Wrap one buffer into visual rows of at most `inner` display columns
/// (CJK counts two, combining marks zero — a terminal row is columns, not
/// chars). Breaks at word boundaries whenever the row has one — a word
/// that would cross the edge comes down whole; only space-less runs
/// hard-break mid-word.
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
        while j < chars.len() && chars[j] != '\n' {
            let w = chars[j].width().unwrap_or(0);
            if col + w > inner {
                break;
            }
            if chars[j].is_whitespace() {
                brk = Some(j + 1);
            }
            col += w;
            j += 1;
        }
        if j < chars.len() && chars[j] != '\n' {
            // Row full with more line remaining: prefer the word boundary.
            // A zero-progress corner (one char wider than the row) still
            // advances by that char rather than looping forever.
            let end = brk.filter(|&b| b > i).unwrap_or(j).max(i + 1);
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

/// The row owning a cursor index. A wrap boundary index is shared by two
/// adjacent rows; it belongs to the lower one (where the cell actually
/// renders), so exactly one row ever claims the cursor.
fn row_of(rows: &[VisualRow], cursor: usize) -> Option<usize> {
    rows.iter().enumerate().position(|(index, row)| {
        let wraps_on = rows
            .get(index + 1)
            .is_some_and(|next| next.start == row.end);
        cursor >= row.start && (cursor < row.end || (cursor == row.end && !wraps_on))
    })
}

/// Display width of a row's slice.
fn row_width(chars: &[char], row: &VisualRow) -> usize {
    chars[row.start..row.end]
        .iter()
        .map(|c| c.width().unwrap_or(0))
        .sum()
}

impl Editor {
    pub fn new() -> Self {
        Editor {
            mask: false,
            text: Vec::new(),
            cursor: 0,
            selection_anchor: None,
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
            pastes: Vec::new(),
            paste_seq: 0,
            paste_limit: crate::core::config::settings::paste_placeholder() as usize,
            inner_width: None,
            scroll: 0,
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
        self.selection_anchor = None;
        self.history_pos = None;
    }

    /// The active selection as an ordered char range, None when empty.
    fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.selection_anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    /// Remove the selected range, leaving the cursor at its start.
    /// Returns true when something was deleted.
    fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            self.selection_anchor = None;
            return false;
        };
        self.text.drain(start..end);
        self.cursor = start;
        self.selection_anchor = None;
        true
    }

    /// Extend (or begin) the selection while moving the cursor to `to` —
    /// the reference's shift-motion rule: the anchor plants at the cursor
    /// on first extension and stays put after.
    fn extend_to(&mut self, to: usize) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor);
        }
        self.cursor = to.min(self.text.len());
        if self.selection_anchor == Some(self.cursor) {
            self.selection_anchor = None;
        }
    }

    /// A plain motion with a selection active collapses to the range edge
    /// instead of moving — the reference's arrow-collapse rule. Returns
    /// true when a collapse consumed the motion.
    fn collapse_selection(&mut self, to_end: bool) -> bool {
        let Some((start, end)) = self.selection() else {
            self.selection_anchor = None;
            return false;
        };
        self.cursor = if to_end { end } else { start };
        self.selection_anchor = None;
        true
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

    /// A paste becomes a placeholder token when it runs past the
    /// configured threshold (codepoints, line count regardless; the token
    /// expands back on submit). Anything smaller inserts literally.
    pub fn insert_paste(&mut self, text: &str) {
        self.delete_selection();
        let lines = text.lines().count().max(1);
        if text.chars().count() <= self.paste_limit {
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

    /// Replace every live placeholder with its pasted content, retiring all
    /// mappings: the draft that owned them is gone once submitted, and a
    /// token typed later must not resurrect an old payload.
    pub fn expand_pastes(&mut self, text: &str) -> String {
        let mut out = text.to_string();
        for (token, content) in self.pastes.drain(..) {
            out = out.replace(&token, &content);
        }
        out
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert(c);
        }
    }

    /// Where a vertical motion would land: the same column in the visual
    /// row above/below, None at the draft's edge.
    fn line_target(&self, direction: isize) -> Option<usize> {
        let inner = self.inner_width?;
        let rows = layout_rows(&self.text, inner);
        let index = row_of(&rows, self.cursor)?;
        let target = match direction {
            -1 if index > 0 => &rows[index - 1],
            1 if index + 1 < rows.len() => &rows[index + 1],
            _ => return None,
        };
        let col = self.cursor.saturating_sub(rows[index].start);
        Some((target.start + col).min(target.end))
    }

    /// Vertical arrows: move between visual rows of the draft when there is
    /// one above/below — preserving the column where possible — otherwise
    /// fall through to input history recall, the single-line behavior.
    fn move_line(&mut self, direction: isize) {
        if let Some(target) = self.line_target(direction) {
            self.cursor = target;
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
                // Typing replaces an active selection — the reference rule.
                self.delete_selection();
                self.insert(c);
                Consumed
            }
            Key::Enter => {
                let text = self.text();
                self.text.clear();
                self.cursor = 0;
                self.selection_anchor = None;
                self.history_pos = None;
                Submit(text)
            }
            Key::Newline => {
                self.delete_selection();
                self.insert('\n');
                Consumed
            }
            Key::Backspace => {
                if !self.delete_selection() && self.cursor > 0 {
                    self.cursor -= 1;
                    self.text.remove(self.cursor);
                }
                Consumed
            }
            Key::Delete => {
                if !self.delete_selection() && self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
                Consumed
            }
            Key::Left => {
                if !self.collapse_selection(false) {
                    self.cursor = self.cursor.saturating_sub(1);
                }
                Consumed
            }
            Key::Right => {
                if !self.collapse_selection(true) {
                    self.cursor = (self.cursor + 1).min(self.text.len());
                }
                Consumed
            }
            Key::WordLeft => {
                if !self.collapse_selection(false) {
                    self.cursor = self.word_left();
                }
                Consumed
            }
            Key::WordRight => {
                if !self.collapse_selection(true) {
                    self.cursor = self.word_right();
                }
                Consumed
            }
            Key::Up => {
                self.selection_anchor = None;
                self.move_line(-1);
                Consumed
            }
            Key::Down => {
                self.selection_anchor = None;
                self.move_line(1);
                Consumed
            }
            Key::Home => {
                self.selection_anchor = None;
                self.cursor = 0;
                Consumed
            }
            Key::End => {
                self.selection_anchor = None;
                self.cursor = self.text.len();
                Consumed
            }
            Key::SelectLeft => {
                self.extend_to(self.cursor.saturating_sub(1));
                Consumed
            }
            Key::SelectRight => {
                self.extend_to((self.cursor + 1).min(self.text.len()));
                Consumed
            }
            Key::SelectWordLeft => {
                self.extend_to(self.word_left());
                Consumed
            }
            Key::SelectWordRight => {
                self.extend_to(self.word_right());
                Consumed
            }
            Key::SelectHome => {
                self.extend_to(0);
                Consumed
            }
            Key::SelectEnd => {
                self.extend_to(self.text.len());
                Consumed
            }
            Key::SelectUp => {
                if let Some(target) = self.line_target(-1) {
                    self.extend_to(target);
                }
                Consumed
            }
            Key::SelectDown => {
                if let Some(target) = self.line_target(1) {
                    self.extend_to(target);
                }
                Consumed
            }
            Key::KillToEnd => {
                self.selection_anchor = None;
                self.text.truncate(self.cursor);
                Consumed
            }
            Key::KillToStart => {
                self.selection_anchor = None;
                self.text.drain(..self.cursor);
                self.cursor = 0;
                Consumed
            }
            Key::KillWord => {
                if !self.delete_selection() {
                    let start = self.word_left();
                    self.text.drain(start..self.cursor);
                    self.cursor = start;
                }
                Consumed
            }
        }
    }

    /// Render the composer band: a leading blank (the reference paints its
    /// top divider only when the composer is hidden or queued banners sit
    /// above it — a plain visible composer gets none), then railed rows
    /// with a reverse-video cursor cell. At most `max_body_rows` draft rows
    /// show; a longer draft scrolls behind a cursor-following window whose
    /// first row wears `┃↑` when rows hide above.
    pub fn render(&mut self, theme: &Theme, width: usize, max_body_rows: usize) -> Vec<String> {
        // A draft starting with `!` is a shell command: the rail turns the
        // bash-mode color — the whole indicator, no words.
        let rail_token =
            if !self.mask && self.text.iter().find(|c| !c.is_whitespace()) == Some(&'!') {
                "bashMode"
            } else {
                "userMessageText"
            };
        let rail = format!("{} ", theme.fg(rail_token, "┃"));
        let inner = width.saturating_sub(2).max(1);
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
        let cursor_row = row_of(&rows, self.cursor);
        let last = rows.len() - 1;
        // While a selection is live the range itself is the highlight —
        // reverse video across its rows, no separate cursor cell.
        let selection = self.selection();
        let mut body: Vec<String> = Vec::with_capacity(rows.len() + 1);
        for (index, row) in rows.iter().enumerate() {
            let slice = &chars[row.start..row.end];
            let full_final_row = index == last
                && self.cursor == self.text.len()
                && self.cursor == row.end
                && row_width(&chars, row) >= inner;
            let cursor_here = cursor_row == Some(index);
            let rendered = if let Some((start, end)) = selection
                .map(|(a, b)| (a.max(row.start), b.min(row.end)))
                .filter(|(a, b)| a < b)
            {
                let before: String = chars[row.start..start].iter().collect();
                let span: String = chars[start..end].iter().collect();
                let after: String = chars[end..row.end].iter().collect();
                format!("{before}\x1b[7m{span}\x1b[27m{after}")
            } else if selection.is_some() {
                slice.iter().collect()
            } else if cursor_here && !full_final_row {
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
            body.push(rendered);
        }
        // A cursor resting past a full final row needs one extra empty row.
        let trailing_cursor = rows.last().is_some_and(|row| {
            self.cursor == self.text.len()
                && !self.text.is_empty()
                && self.cursor == row.end
                && row_width(&chars, row) >= inner
        });
        if trailing_cursor {
            body.push("\x1b[7m \x1b[27m".to_string());
        }

        // The cursor-following window: the draft keeps its share of the
        // frame, older rows scroll behind a `┃↑` marker on the first
        // visible row instead of shoving the transcript off screen.
        let cap = max_body_rows.max(1);
        let focus = if trailing_cursor {
            body.len() - 1
        } else {
            cursor_row.unwrap_or(0)
        };
        if body.len() <= cap {
            self.scroll = 0;
        } else {
            if self.scroll > focus {
                self.scroll = focus;
            }
            if focus >= self.scroll + cap {
                self.scroll = focus + 1 - cap;
            }
            if self.scroll + cap > body.len() {
                self.scroll = body.len() - cap;
            }
        }
        for (i, rendered) in body.iter().enumerate().skip(self.scroll).take(cap) {
            if i == self.scroll && self.scroll > 0 {
                out.push(format!("{}{rendered}", theme.fg(rail_token, "┃↑")));
            } else {
                out.push(format!("{rail}{rendered}"));
            }
        }
        out
    }
}

/// One visual row: an absolute char range in the buffer. Newline characters
/// belong to no row — they are zero-width row terminators. Cursor ownership
/// is resolved by `row_of`, never per-row: a wrap boundary index would
/// otherwise belong to two rows and paint two cursors.
struct VisualRow {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
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
    SelectLeft,
    SelectRight,
    SelectWordLeft,
    SelectWordRight,
    SelectHome,
    SelectEnd,
    SelectUp,
    SelectDown,
    KillToEnd,
    KillToStart,
    KillWord,
}
