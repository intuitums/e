//! A side pane beside the conversation, opened by an extension with
//! `ui.pane` and painted by e. The extension sends sections — a
//! selectable list, a unified diff, text, markdown, or themed rows — and e
//! owns everything interactive: focus, scrolling, the cursor, selection,
//! mouse, the split, and the narrow-terminal fallback. What the user does
//! goes back as data (`pane.select`, `pane.activate`, `pane.key`,
//! `pane.closed`); a selection attaches to the composer as a snapshot.
//!
//! Where the pane sits and how wide it is come from `~/.e/layout.json`
//! (`core/config/layout.rs`); the extension only proposes a side.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use serde_json::Value;

use crate::core::config::layout::{Layout, Side};
use crate::core::tools::sanitize_display;
use crate::tui::{
    markdown::{clip_styled, render_markdown, visible_width, wrap_styled},
    panel,
    render::bold,
    theme::Theme,
    transcript::diff_row_style,
};

/// Most bytes a pane's sections may carry together; past it the rest is
/// dropped and the last row says so.
pub const MAX_PANE_BYTES: usize = 256 * 1024;
/// Most rows a list section shows at once; longer lists scroll.
const LIST_ROWS: usize = 8;
/// Widest a pane title paints.
const TITLE_COLUMNS: usize = 60;

pub const HINT: &str = "↑↓ move · Enter · Tab section · Esc back";

/// One themed run of text on a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub token: Option<String>,
}

/// A row of spans from JSON: a string paints plain, an array of
/// `{text, token}` spans paints each run through the theme.
pub fn spans_of(line: &Value) -> Vec<Span> {
    match line {
        Value::String(text) => vec![Span {
            text: flat(text),
            token: None,
        }],
        Value::Array(spans) => spans
            .iter()
            .filter_map(|span| {
                let text = span
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| span.as_str())?;
                Some(Span {
                    text: flat(text),
                    token: span
                        .get("token")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            })
            .collect(),
        other => vec![Span {
            text: flat(&other.to_string()),
            token: None,
        }],
    }
}

fn flat(text: &str) -> String {
    sanitize_display(text).replace('\n', " ")
}

/// Paint one row of spans, clipped to `width`.
pub fn paint_spans(theme: &Theme, spans: &[Span], width: usize) -> String {
    let mut row = String::new();
    for span in spans {
        match &span.token {
            Some(token) => row.push_str(&theme.fg(token, &span.text)),
            None => row.push_str(&span.text),
        }
    }
    clip_styled(&row, width)
}

/// One selectable row of a list section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    List(Vec<Item>),
    /// Rows already in e's diff grammar (`diffview::from_unified`).
    Diff(Vec<String>),
    Text(String),
    Markdown(String),
    Rows(Vec<Vec<Span>>),
}

/// One section of a pane, with the interactive state e keeps for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub content: Content,
    cursor: usize,
    anchor: Option<usize>,
    scroll: usize,
    horizontal: usize,
    /// Rows painted last frame, for paging and the mouse.
    rows: usize,
    /// The frame row the section's body started on last frame.
    start: usize,
}

impl Section {
    fn from_json(value: &Value, index: usize) -> Option<Section> {
        let kind = value.get("kind").and_then(Value::as_str)?;
        let body = || {
            value
                .get("body")
                .and_then(Value::as_str)
                .map(sanitize_display)
                .unwrap_or_default()
        };
        let content = match kind {
            "list" => Content::List(
                value
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .enumerate()
                            .map(|(i, item)| {
                                let field = |key: &str| {
                                    item.get(key)
                                        .and_then(Value::as_str)
                                        .map(flat)
                                        .unwrap_or_default()
                                };
                                let label = match item.as_str() {
                                    Some(plain) => flat(plain),
                                    None => field("label"),
                                };
                                let id = match field("id") {
                                    id if id.is_empty() => {
                                        if label.is_empty() {
                                            i.to_string()
                                        } else {
                                            label.clone()
                                        }
                                    }
                                    id => id,
                                };
                                Item {
                                    id,
                                    label,
                                    detail: field("detail"),
                                    token: item
                                        .get("token")
                                        .and_then(Value::as_str)
                                        .map(str::to_string),
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            "diff" => {
                // The row grammar names the file on its first row; a
                // section titled with the same name would say it twice.
                let title = value.get("title").and_then(Value::as_str).map(flat);
                let rows: Vec<String> = crate::core::tools::diffview::from_unified(&body())
                    .lines()
                    .map(str::to_string)
                    .collect();
                let rows = match (&title, rows.first()) {
                    (Some(title), Some(first)) if first == title => rows[1..].to_vec(),
                    _ => rows,
                };
                Content::Diff(rows)
            }
            "text" => Content::Text(body()),
            "markdown" => Content::Markdown(body()),
            "rows" => Content::Rows(
                value
                    .get("lines")
                    .and_then(Value::as_array)
                    .map(|lines| lines.iter().map(spans_of).collect())
                    .unwrap_or_default(),
            ),
            _ => return None,
        };
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .map(flat)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| index.to_string());
        let mut section = Section {
            id,
            title: value
                .get("title")
                .and_then(Value::as_str)
                .map(|t| clip_plain(&flat(t), TITLE_COLUMNS))
                .unwrap_or_default(),
            content,
            cursor: 0,
            anchor: None,
            scroll: 0,
            horizontal: 0,
            rows: 1,
            start: 0,
        };
        if let (Content::List(items), Some(selected)) = (
            &section.content,
            value.get("selected").and_then(Value::as_str),
        ) {
            section.cursor = items
                .iter()
                .position(|item| item.id == selected)
                .unwrap_or(0);
        }
        Some(section)
    }

    /// How many rows the content has to move through.
    fn len(&self) -> usize {
        match &self.content {
            Content::List(items) => items.len(),
            Content::Diff(rows) => rows.len(),
            Content::Rows(rows) => rows.len(),
            // Wrapped at paint time; the row count is remembered then.
            Content::Text(_) | Content::Markdown(_) => self.rows.max(1),
        }
    }

    fn is_list(&self) -> bool {
        matches!(self.content, Content::List(_))
    }

    /// The selected list item, if this is a list with one.
    pub fn selected(&self) -> Option<&Item> {
        match &self.content {
            Content::List(items) => items.get(self.cursor),
            _ => None,
        }
    }

    /// Carry the cursor and scroll from the section this one replaces, so
    /// a refresh does not move what the user was looking at.
    fn inherit(&mut self, old: &Section) {
        self.cursor = old.cursor;
        self.scroll = old.scroll;
        self.horizontal = old.horizontal;
        self.anchor = old.anchor;
        if let (Content::List(items), Some(before)) = (&self.content, old.selected()) {
            if let Some(index) = items.iter().position(|item| item.id == before.id) {
                self.cursor = index;
            }
        }
        // Different content under a selection would attach text the user
        // never chose.
        if self.content != old.content {
            self.anchor = None;
        }
        self.clamp();
    }

    fn clamp(&mut self) {
        let last = self.len().saturating_sub(1);
        self.cursor = self.cursor.min(last);
        self.scroll = self.scroll.min(last);
    }

    /// The plain text of the rows `lo..=hi`, for an attachment.
    fn lines(&self, width: usize) -> Vec<String> {
        match &self.content {
            Content::List(items) => items
                .iter()
                .map(|item| {
                    if item.detail.is_empty() {
                        item.label.clone()
                    } else {
                        format!("{}  {}", item.label, item.detail)
                    }
                })
                .collect(),
            Content::Diff(rows) => rows.clone(),
            Content::Rows(rows) => rows
                .iter()
                .map(|spans| spans.iter().map(|s| s.text.as_str()).collect::<String>())
                .collect(),
            Content::Text(text) => wrap_styled(text, width.max(8)),
            Content::Markdown(text) => text.lines().map(str::to_string).collect(),
        }
    }
}

fn clip_plain(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let head: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// What a key or mouse event asks the app to do beyond the pane's own state.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// The user closed the pane; its owner is told.
    Close,
    /// Attach a snapshot of the selected rows to the composer.
    Attach {
        label: String,
        content: String,
    },
    /// The cursor moved to another list item.
    Select {
        section: String,
        id: String,
    },
    /// Enter on a list item.
    Activate {
        section: String,
        id: String,
    },
    /// A chord e did not use, for the owner.
    Key(String),
}

pub struct Pane {
    pub id: String,
    pub extension: String,
    pub title: String,
    pub hint: String,
    /// The side the extension proposed; the layout file may override it.
    pub side: Option<Side>,
    pub sections: Vec<Section>,
    pub focused: bool,
    focus: usize,
    /// Bytes past the cap were dropped; the last row says so.
    clipped: bool,
    close_column: usize,
}

impl Pane {
    /// A pane from a `ui.pane` request. `None` when the request carries no
    /// sections at all.
    pub fn from_request(extension: &str, params: &Value) -> Option<Pane> {
        let text = params.to_string();
        let clipped = text.len() > MAX_PANE_BYTES;
        let mut budget = MAX_PANE_BYTES;
        let mut sections = Vec::new();
        for (index, value) in params
            .get("sections")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let size = value.to_string().len();
            if size > budget {
                break;
            }
            budget -= size;
            if let Some(section) = Section::from_json(value, index) {
                sections.push(section);
            }
        }
        if sections.is_empty() {
            return None;
        }
        let id = params
            .get("id")
            .and_then(Value::as_str)
            .map(flat)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| extension.to_string());
        let title = params
            .get("title")
            .and_then(Value::as_str)
            .map(|t| clip_plain(&flat(t), TITLE_COLUMNS))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| id.clone());
        let side = match params.get("side").and_then(Value::as_str) {
            Some("left") => Some(Side::Left),
            Some("right") => Some(Side::Right),
            _ => None,
        };
        let hint = params
            .get("hint")
            .and_then(Value::as_str)
            .map(|h| clip_plain(&flat(h), 80))
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| HINT.to_string());
        Some(Pane {
            id,
            extension: extension.to_string(),
            title,
            hint,
            side,
            sections,
            focused: true,
            focus: 0,
            clipped,
            close_column: 0,
        })
    }

    /// Replace the content with a newer request's, keeping focus, cursors,
    /// and scroll for every section whose id survived.
    pub fn update(&mut self, fresh: Pane) {
        let old = std::mem::replace(&mut self.sections, fresh.sections);
        for section in &mut self.sections {
            if let Some(before) = old.iter().find(|s| s.id == section.id) {
                section.inherit(before);
            }
        }
        self.title = fresh.title;
        self.hint = fresh.hint;
        self.side = fresh.side.or(self.side);
        self.clipped = fresh.clipped;
        let focused_id = old.get(self.focus).map(|s| s.id.clone());
        self.focus = focused_id
            .and_then(|id| self.sections.iter().position(|s| s.id == id))
            .unwrap_or(0);
    }

    /// The conversation's width and the pane's, when the terminal is wide
    /// enough to split; `None` means one of them fills the screen.
    pub fn split(&self, width: usize, layout: &Layout) -> Option<(usize, usize)> {
        if width < layout.split_min {
            return None;
        }
        let placement = layout.placement(&self.id, self.side);
        let pane = width * placement.width / 100;
        Some((width.saturating_sub(pane + 3), pane))
    }

    /// The side this pane paints on, after the layout file's say.
    pub fn side(&self, layout: &Layout) -> Side {
        layout.placement(&self.id, self.side).side
    }

    fn section(&mut self) -> &mut Section {
        &mut self.sections[self.focus]
    }

    /// Move the cursor by `delta` rows in the focused section; a list
    /// reports the new selection.
    fn step(&mut self, delta: isize, extend: bool) -> Action {
        let section = self.section();
        let before = section.cursor;
        let last = section.len().saturating_sub(1);
        section.cursor = if delta < 0 {
            section.cursor.saturating_sub(delta.unsigned_abs())
        } else {
            (section.cursor + delta as usize).min(last)
        };
        if section.is_list() {
            section.anchor = None;
            if section.cursor != before {
                return Action::Select {
                    section: section.id.clone(),
                    id: section.selected().map(|i| i.id.clone()).unwrap_or_default(),
                };
            }
        } else if extend {
            section.anchor.get_or_insert(before);
        } else {
            section.anchor = None;
        }
        Action::None
    }

    /// Snapshot the selected rows (or the cursor row) for the composer.
    /// The label names the section when it has a title (a file path, say),
    /// else the pane.
    fn attach(&mut self, width: usize) -> Action {
        let pane_title = self.title.clone();
        let section = self.section();
        let lines = section.lines(width);
        if lines.is_empty() {
            return Action::None;
        }
        let anchor = section.anchor.unwrap_or(section.cursor);
        let lo = anchor.min(section.cursor).min(lines.len() - 1);
        let hi = anchor.max(section.cursor).min(lines.len() - 1);
        let name = if section.title.is_empty() {
            pane_title.clone()
        } else {
            section.title.clone()
        };
        let mut content = format!("From the {pane_title} pane");
        if !section.title.is_empty() {
            content.push_str(&format!(" ({})", section.title));
        }
        content.push_str(":\n");
        content.push_str(&lines[lo..=hi].join("\n"));
        section.anchor = None;
        self.focused = false;
        Action::Attach {
            label: format!("[{} {} lines]", clip_plain(&name, 36), hi - lo + 1),
            content,
        }
    }

    /// Keys while the pane owns focus. ctrl+c never reaches here.
    pub fn key(&mut self, key: KeyEvent, width: usize) -> Action {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page = self.section().rows.max(1) as isize;
        match key.code {
            KeyCode::Esc => {
                if self.focus > 0 {
                    self.section().anchor = None;
                    self.focus = 0;
                    Action::None
                } else {
                    Action::Close
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let n = self.sections.len();
                self.focus = if key.code == KeyCode::Tab {
                    (self.focus + 1) % n
                } else {
                    (self.focus + n - 1) % n
                };
                Action::None
            }
            KeyCode::Enter => {
                if self.section().is_list() {
                    let section = self.section();
                    let action = Action::Activate {
                        section: section.id.clone(),
                        id: section.selected().map(|i| i.id.clone()).unwrap_or_default(),
                    };
                    if self.focus + 1 < self.sections.len() {
                        self.focus += 1;
                    }
                    action
                } else {
                    self.attach(width)
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.step(-1, shift),
            KeyCode::Down | KeyCode::Char('j') => self.step(1, shift),
            KeyCode::PageUp => self.step(-page, shift),
            KeyCode::PageDown => self.step(page, shift),
            KeyCode::Home => self.step(isize::MIN / 2, shift),
            KeyCode::End => self.step(isize::MAX / 2, shift),
            KeyCode::Left if !self.section().is_list() => {
                let section = self.section();
                section.horizontal = section.horizontal.saturating_sub(8);
                Action::None
            }
            KeyCode::Right if !self.section().is_list() => {
                let section = self.section();
                section.horizontal = (section.horizontal + 8).min(4096);
                Action::None
            }
            _ => match crate::tui::app::chord_of(&key) {
                Some(chord) => Action::Key(chord),
                None => Action::None,
            },
        }
    }

    /// Mouse events with coordinates local to the pane's own rows.
    pub fn mouse(&mut self, event: MouseEvent) -> Action {
        let row = event.row as usize;
        let hit = self
            .sections
            .iter()
            .position(|s| row >= s.start && row < s.start + s.rows);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.focused = true;
                if row == 1 && event.column as usize >= self.close_column {
                    return Action::Close;
                }
                let Some(index) = hit else {
                    return Action::None;
                };
                self.focus = index;
                let section = self.section();
                let target =
                    (section.scroll + row - section.start).min(section.len().saturating_sub(1));
                let delta = target as isize - section.cursor as isize;
                let action = self.step(delta, false);
                let section = self.section();
                if !section.is_list() {
                    section.anchor = Some(section.cursor);
                }
                action
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let section = self.section();
                if section.anchor.is_none() || section.is_list() {
                    return Action::None;
                }
                let target = (section.scroll + row.saturating_sub(section.start))
                    .min(section.len().saturating_sub(1));
                section.cursor = target;
                Action::None
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let Some(index) = hit else {
                    return Action::None;
                };
                let was = self.focus;
                self.focus = index;
                let delta: isize = if event.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                };
                let action = if self.section().is_list() {
                    self.step(delta.signum(), false)
                } else {
                    let section = self.section();
                    section.scroll = if delta < 0 {
                        section.scroll.saturating_sub(3)
                    } else {
                        (section.scroll + 3).min(section.len().saturating_sub(1))
                    };
                    section.cursor = section.cursor.clamp(
                        section.scroll,
                        section.scroll + section.rows.saturating_sub(1),
                    );
                    Action::None
                };
                if !self.focused {
                    self.focus = was;
                }
                action
            }
            _ => Action::None,
        }
    }

    /// The pane at a fixed height: the shared panel frame, every section
    /// with its rows, the hint on the last row.
    pub fn render(&mut self, theme: &Theme, width: usize, height: usize) -> Vec<String> {
        let available = height.saturating_sub(5);
        // Row budget: lists take up to LIST_ROWS, every other section
        // shares what is left; a section title and a divider cost a row.
        let count = self.sections.len();
        let chrome: usize = self
            .sections
            .iter()
            .map(|s| usize::from(!s.title.is_empty()))
            .sum::<usize>()
            + count.saturating_sub(1)
            + usize::from(self.clipped);
        let body_rows = available.saturating_sub(chrome);
        let list_rows: usize = self
            .sections
            .iter()
            .filter(|s| s.is_list())
            .map(|s| s.len().clamp(1, LIST_ROWS))
            .sum();
        let scrolling = self.sections.iter().filter(|s| !s.is_list()).count();
        let each = body_rows
            .saturating_sub(list_rows)
            .checked_div(scrolling)
            .unwrap_or(0);

        let mut body: Vec<String> = Vec::new();
        let focused_index = self.focus;
        let focused = self.focused;
        for (index, section) in self.sections.iter_mut().enumerate() {
            if index > 0 {
                body.push(theme.fg("border", &"─".repeat(width)));
            }
            if !section.title.is_empty() {
                body.push(theme.fg("dim", &clip_styled(&section.title, width)));
            }
            let rows = if section.is_list() {
                section.len().clamp(1, LIST_ROWS)
            } else {
                each.max(1)
            };
            section.rows = rows;
            section.start = 3 + body.len();
            let owns = focused && index == focused_index;
            let painted = match &section.content {
                Content::List(items) => {
                    let selected = section.cursor;
                    items
                        .iter()
                        .enumerate()
                        .map(|(i, item)| {
                            let detail = &item.detail;
                            let label = clip_styled(
                                &item.label,
                                width.saturating_sub(visible_width(detail) + 1),
                            );
                            let pad =
                                width.saturating_sub(visible_width(&label) + visible_width(detail));
                            let label = if i == selected {
                                bold(&theme.fg("userMessageText", &label))
                            } else {
                                theme.fg(item.token.as_deref().unwrap_or("dim"), &label)
                            };
                            let detail = theme.fg("dim", detail);
                            format!("{label}{}{detail}", " ".repeat(pad))
                        })
                        .collect::<Vec<_>>()
                }
                Content::Diff(rows) => rows
                    .iter()
                    .map(|line| {
                        diff_row_style(theme, line)
                            .unwrap_or_else(|| theme.fg("customMessageText", line))
                    })
                    .collect(),
                Content::Text(text) => wrap_styled(text, width.max(8)),
                Content::Markdown(text) => render_markdown(theme, text, width.max(8)),
                Content::Rows(rows) => rows
                    .iter()
                    .map(|spans| paint_spans(theme, spans, width))
                    .collect(),
            };
            // Wrapped kinds learn their length at paint time.
            if matches!(section.content, Content::Text(_) | Content::Markdown(_)) {
                let len = painted.len().max(1);
                section.cursor = section.cursor.min(len - 1);
                section.scroll = section.scroll.min(len - 1);
            }
            if section.cursor < section.scroll {
                section.scroll = section.cursor;
            }
            if section.cursor >= section.scroll + rows {
                section.scroll = section.cursor + 1 - rows;
            }
            let range = section
                .anchor
                .map(|a| (a.min(section.cursor), a.max(section.cursor)));
            let mut shown = 0;
            for (i, row) in painted.iter().enumerate().skip(section.scroll).take(rows) {
                let mut text = if section.horizontal > 0 && !section.is_list() {
                    let skipped: String = crate::core::tools::strip_ansi(row)
                        .chars()
                        .skip(section.horizontal)
                        .collect();
                    theme.fg("dim", &skipped)
                } else {
                    row.clone()
                };
                text = clip_styled(&text, width);
                if range.is_some_and(|(lo, hi)| i >= lo && i <= hi) {
                    text = format!("\x1b[7m{text}\x1b[27m");
                } else if owns && !section.is_list() && i == section.cursor {
                    text = bold(&text);
                }
                body.push(text);
                shown += 1;
            }
            for _ in shown..rows {
                body.push(String::new());
            }
            // Text and markdown keep their painted length for paging.
            if matches!(section.content, Content::Text(_) | Content::Markdown(_)) {
                section.rows = rows.max(1);
                let _ = painted.len();
            }
        }
        if self.clipped {
            body.push(theme.fg("dim", "… clipped: the pane holds 256 KiB"));
        }
        body.truncate(available);
        body.resize(available, String::new());
        let title = clip_styled(&self.title, width.saturating_sub(2));
        let header = format!(
            "{title}{}×",
            " ".repeat(width.saturating_sub(visible_width(&title) + 1))
        );
        self.close_column = width.saturating_sub(1);
        let header = theme.fg(
            if self.focused {
                "userMessageText"
            } else {
                "dim"
            },
            &header,
        );
        let mut rows = panel::frame(theme, width, header, body);
        rows.push(theme.fg("dim", &clip_styled(&self.hint, width)));
        rows.truncate(height);
        rows.into_iter()
            .map(|row| clip_styled(&row, width))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn pane() -> Pane {
        Pane::from_request(
            "diff",
            &json!({
                "id": "diff", "title": "Changes", "side": "left",
                "sections": [
                    {"kind": "list", "id": "files", "items": [
                        {"id": "a.rs", "label": "a.rs", "detail": "+1 -1"},
                        {"id": "b.rs", "label": "b.rs", "detail": "+2 -0"}
                    ]},
                    {"kind": "diff", "id": "patch", "body": "--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n"}
                ]
            }),
        )
        .unwrap()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn moving_through_a_list_reports_the_selection_and_enter_moves_on() {
        let mut pane = pane();
        assert_eq!(
            pane.key(key(KeyCode::Down), 40),
            Action::Select {
                section: "files".into(),
                id: "b.rs".into()
            }
        );
        assert_eq!(
            pane.key(key(KeyCode::Down), 40),
            Action::None,
            "already last"
        );
        assert_eq!(
            pane.key(key(KeyCode::Enter), 40),
            Action::Activate {
                section: "files".into(),
                id: "b.rs".into()
            }
        );
        assert_eq!(pane.focus, 1, "Enter on a list focuses the next section");
        assert_eq!(pane.key(key(KeyCode::Esc), 40), Action::None);
        assert_eq!(pane.focus, 0, "Esc returns to the first section");
        assert_eq!(pane.key(key(KeyCode::Esc), 40), Action::Close);
    }

    #[test]
    fn a_shift_selection_attaches_diff_rows_and_clears() {
        let mut pane = pane();
        pane.focus = 1;
        let theme = crate::tui::theme::resolve("dark", false);
        pane.render(&theme, 40, 20);
        for _ in 0..2 {
            assert_eq!(
                pane.key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT), 40),
                Action::None
            );
        }
        assert_eq!(pane.sections[1].anchor, Some(0));
        let Action::Attach { label, content } = pane.key(key(KeyCode::Enter), 40) else {
            panic!("expected an attachment");
        };
        // The file row, the context row, and the removed row.
        assert_eq!(label, "[Changes 3 lines]");
        assert!(
            content.starts_with("From the Changes pane:\na.rs\n"),
            "{content}"
        );
        assert!(
            content.contains("  one") && content.contains("- two"),
            "{content}"
        );
        assert!(!pane.focused, "attaching hands focus back to the composer");
    }

    #[test]
    fn an_update_keeps_the_cursor_on_the_same_item_and_a_missing_id_resets() {
        let mut pane = pane();
        pane.key(key(KeyCode::Down), 40);
        let fresh = Pane::from_request(
            "diff",
            &json!({"id": "diff", "sections": [
                {"kind": "list", "id": "files", "items": [
                    {"id": "c.rs", "label": "c.rs"}, {"id": "b.rs", "label": "b.rs"}
                ]},
                {"kind": "diff", "id": "patch", "body": "@@ -1 +1 @@\n-x\n+y\n"}
            ]}),
        )
        .unwrap();
        pane.update(fresh);
        assert_eq!(
            pane.sections[0].selected().map(|i| i.id.as_str()),
            Some("b.rs")
        );
        assert_eq!(pane.title, "diff", "no title falls back to the id");
        let gone = Pane::from_request(
            "diff",
            &json!({"sections": [{"kind": "text", "id": "note", "body": "nothing here"}]}),
        )
        .unwrap();
        pane.update(gone);
        assert_eq!(pane.focus, 0);
        assert_eq!(pane.sections.len(), 1);
    }

    #[test]
    fn unknown_keys_go_to_the_owner_and_the_split_obeys_the_layout() {
        let mut pane = pane();
        assert_eq!(
            pane.key(key(KeyCode::Char('x')), 40),
            Action::Key("x".into())
        );
        let layout = crate::core::config::layout::parse(
            r#"{"panes":{"diff":{"side":"right","width":30}},"split_min":100}"#,
        )
        .unwrap();
        assert_eq!(pane.split(120, &layout), Some((81, 36)));
        assert_eq!(pane.split(90, &layout), None);
        assert_eq!(
            pane.side(&layout),
            Side::Right,
            "the file outranks the request"
        );
        assert_eq!(
            pane.side(&Layout::default()),
            Side::Left,
            "without a file entry the extension's side stands"
        );
    }

    #[test]
    fn the_frame_shows_every_section_with_the_selected_row_bright() {
        let mut pane = pane();
        let theme = crate::tui::theme::resolve("dark", false);
        let rows = pane.render(&theme, 40, 14);
        assert_eq!(rows.len(), 14);
        let plain: Vec<String> = rows
            .iter()
            .map(|r| crate::core::tools::strip_ansi(r))
            .collect();
        assert!(
            plain[1].starts_with("Changes") && plain[1].ends_with('×'),
            "{:?}",
            plain[1]
        );
        assert!(
            plain[3].starts_with("a.rs") && plain[3].ends_with("+1 -1"),
            "{:?}",
            plain[3]
        );
        assert!(rows[3].contains("\x1b[1m"), "the selected item is bold");
        assert!(plain.iter().any(|r| r.contains("- two")), "{plain:?}");
        assert!(plain.iter().any(|r| r.contains("+ three")), "{plain:?}");
        assert_eq!(plain[13], HINT);
    }

    #[test]
    fn oversized_and_empty_requests_are_refused_or_clipped() {
        assert!(Pane::from_request("x", &json!({"sections": []})).is_none());
        assert!(Pane::from_request("x", &json!({"sections": [{"kind": "bogus"}]})).is_none());
        let big = "x".repeat(MAX_PANE_BYTES);
        let pane = Pane::from_request(
            "x",
            &json!({"sections": [{"kind": "text", "body": "small"}, {"kind": "text", "body": big}]}),
        )
        .unwrap();
        assert_eq!(
            pane.sections.len(),
            1,
            "the section past the cap is dropped"
        );
        assert!(pane.clipped);
    }
}
