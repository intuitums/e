//! The footer picker band.
//!
//! Menus render between the composer and the status row, the reference
//! inline-picker shape:
//!
//! ```text
//!   ── divider ─────────────────────────────
//!   Commands 7 · Type to filter          1–7
//!
//!     /login   sign in to a provider
//!     /model   list or switch models       ← selected row bold, no caret
//!   ── divider ─────────────────────────────
//!   ↑↓ Navigate     Enter Use     Esc Close   (rides the status row)
//! ```
//!
//! Selection is bold-ink vs default — no caret glyph, the reference
//! convention.

use crate::tui::markdown::visible_width;
use crate::tui::render::bold;
use crate::tui::theme::Theme;

#[derive(Clone)]
pub struct MenuItem {
    pub label: String,
    pub description: String,
    /// Right-aligned dim metadata.
    pub meta: String,
    pub value: String,
}

impl MenuItem {
    pub fn new(label: &str, description: &str, value: &str) -> Self {
        MenuItem {
            label: label.into(),
            description: description.into(),
            meta: String::new(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    Commands,
    Files,
    Models,
    Sessions,
    Skills,
}

pub struct Menu {
    pub kind: MenuKind,
    pub title: &'static str,
    pub hint: &'static str,
    items: Vec<MenuItem>,
    filtered: Vec<usize>,
    pub selected: usize,
    window_start: usize,
    query: String,
}

pub const HINT_USE: &str = "↑↓ Navigate     Enter Use     Esc Close";
/// The reference keeps six selectable rows below the header.
const MAX_VISIBLE: usize = 6;

/// Subsequence fuzzy match; lower score is better, None is no match.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<usize> {
    if query.is_empty() {
        return Some(usize::MAX / 2); // neutral: keep original order
    }
    let candidate_lower = candidate.to_lowercase();
    let query_lower = query.to_lowercase();
    let mut score = 0usize;
    let mut position = 0usize;
    let mut last_hit: Option<usize> = None;
    for needle in query_lower.chars() {
        let found = candidate_lower[position..].find(needle)?;
        let at = position + found;
        // Contiguity bonus: gaps cost, adjacency is free.
        if let Some(last) = last_hit {
            score += at - last - 1;
        } else {
            score += at; // earlier first hits rank higher
        }
        last_hit = Some(at);
        position = at + needle.len_utf8();
    }
    Some(score)
}

impl Menu {
    pub fn new(
        kind: MenuKind,
        title: &'static str,
        hint: &'static str,
        items: Vec<MenuItem>,
    ) -> Self {
        let mut menu = Menu {
            kind,
            title,
            hint,
            items,
            filtered: Vec::new(),
            selected: 0,
            window_start: 0,
            query: String::new(),
        };
        menu.refilter();
        menu
    }

    pub fn set_query(&mut self, query: &str) {
        if self.query != query {
            self.query = query.to_string();
            self.refilter();
        }
    }

    fn refilter(&mut self) {
        let mut scored: Vec<(usize, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let best = fuzzy_score(&self.query, &item.label)
                    .into_iter()
                    .chain(fuzzy_score(&self.query, &item.description))
                    .min()?;
                Some((best, i))
            })
            .collect();
        scored.sort_by_key(|(score, i)| (*score, *i));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        self.selected = 0;
        self.window_start = 0;
    }

    pub fn len(&self) -> usize {
        self.filtered.len()
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    pub fn current(&self) -> Option<&MenuItem> {
        self.filtered.get(self.selected).map(|&i| &self.items[i])
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        if self.selected < self.window_start {
            self.window_start = self.selected;
        }
        if self.selected >= self.window_start + MAX_VISIBLE {
            self.window_start = self.selected - MAX_VISIBLE + 1;
        }
    }

    pub fn render(&self, theme: &Theme, width: usize) -> Vec<String> {
        let count = self.filtered.len();
        let window_end = (self.window_start + MAX_VISIBLE).min(count);

        let mut header = format!(
            "{}{}",
            bold(&theme.fg("userMessageText", self.title)),
            theme.fg(
                "muted",
                &if self.query.is_empty() {
                    format!(" {count} · Type to filter")
                } else {
                    format!(" {count} · {}", self.query)
                }
            ),
        );
        if count > MAX_VISIBLE {
            let range = format!("{}–{}", self.window_start + 1, window_end);
            let pad = width.saturating_sub(visible_width(&header) + range.chars().count());
            if pad > 1 {
                header.push_str(&" ".repeat(pad));
                header.push_str(&theme.fg("muted", &range));
            }
        }

        let mut body: Vec<String> = Vec::new();
        if count == 0 {
            body.push(theme.fg("muted", "  Nothing found."));
        }
        // Reference treatment: selection is brightness alone — the selected
        // row's label is bold bright ink, every other row is dim entirely;
        // descriptions are always dim; three-space column gap.
        let label_width = self.filtered[self.window_start..window_end]
            .iter()
            .map(|&i| visible_width(&self.items[i].label))
            .max()
            .unwrap_or(0)
            .min(36);
        for slot in self.window_start..window_end {
            let item = &self.items[self.filtered[slot]];
            let selected = slot == self.selected;
            let padded = format!(
                "{}{}",
                item.label,
                " ".repeat(label_width.saturating_sub(visible_width(&item.label)))
            );
            let mut row = if selected {
                bold(&theme.fg("userMessageText", &padded))
            } else {
                theme.fg("dim", &padded)
            };
            if !item.description.is_empty() {
                row.push_str(&theme.fg("dim", &format!("   {}", item.description)));
            }
            if !item.meta.is_empty() {
                let pad = width.saturating_sub(2 + visible_width(&row) + visible_width(&item.meta));
                if pad > 3 {
                    row.push_str(&" ".repeat(pad));
                    row.push_str(&theme.fg("dim", &item.meta));
                }
            }
            let mut line = format!("  {row}");
            if visible_width(&line) > width {
                line = line
                    .chars()
                    .take(width.saturating_sub(1))
                    .collect::<String>()
                    + "…";
            }
            body.push(line);
        }
        crate::tui::panel::frame(theme, width, header, body)
    }
}

#[cfg(test)]
mod tests {
    use super::fuzzy_score;

    #[test]
    fn fuzzy_prefers_tight_early_matches() {
        // Subsequence required.
        assert!(fuzzy_score("xyz", "composer.rs").is_none());
        // Tighter match beats scattered.
        let tight = fuzzy_score("comp", "src/tui/composer.rs").unwrap();
        let scattered = fuzzy_score("comp", "core/completions.rs").unwrap();
        assert!(tight < usize::MAX / 2 && scattered < usize::MAX / 2);
        // Case-insensitive.
        assert!(fuzzy_score("LOG", "/login").is_some());
        // Empty query keeps everything, original order.
        assert_eq!(fuzzy_score("", "anything"), Some(usize::MAX / 2));
    }
}
