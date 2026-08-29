//! The question panel — the ask tool's footer surface.
//!
//! Framed like every picker: divider, question, blank, options, divider,
//! with the nav hint on the status row. Selection is brightness alone —
//! the reference's question panel explicitly uses no caret. A freeform
//! slot, when allowed, takes a typed answer.

use crate::tui::markdown::{visible_width, wrap_styled};
use crate::tui::render::bold;
use crate::tui::theme::Theme;

pub struct Question {
    pub id: u64,
    pub question: String,
    /// (label, description) pairs, in the model's order.
    pub options: Vec<(String, String)>,
    pub allow_freeform: bool,
    pub selected: usize,
    pub freeform: String,
}

impl Question {
    pub fn new(
        id: u64,
        question: String,
        options: Vec<(String, String)>,
        allow_freeform: bool,
    ) -> Self {
        Question {
            id,
            question,
            options,
            allow_freeform,
            selected: 0,
            freeform: String::new(),
        }
    }

    fn slots(&self) -> usize {
        self.options.len() + usize::from(self.allow_freeform)
    }

    pub fn freeform_selected(&self) -> bool {
        self.allow_freeform && self.selected == self.options.len()
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.slots() as isize;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    /// Jump to option `n` (1-based). True when it exists.
    pub fn choose(&mut self, n: usize) -> bool {
        if n >= 1 && n <= self.options.len() {
            self.selected = n - 1;
            true
        } else {
            false
        }
    }

    /// The answer the current selection would submit; None when the
    /// freeform slot is selected but still empty.
    pub fn answer(&self) -> Option<String> {
        if self.freeform_selected() {
            let text = self.freeform.trim();
            (!text.is_empty()).then(|| text.to_string())
        } else {
            self.options
                .get(self.selected)
                .map(|(label, _)| label.clone())
        }
    }

    /// The status-row hint for the current selection, degraded to the
    /// frame's width the way every picker hint is.
    pub fn hint(&self, width: usize) -> String {
        let head = if self.freeform_selected() {
            "Type answer    ".to_string()
        } else if self.options.len() > 1 {
            // 1–9 covers the digit grammar's whole reach.
            format!("1–{} Choose now    ", self.options.len())
        } else {
            String::new()
        };
        let candidates = [
            format!("{head}↑↓ Options    Enter Answer    Esc Cancel"),
            "↑↓ Options    Enter Answer    Esc Cancel".to_string(),
            "↑↓ Options  Enter Answer  Esc Cancel".to_string(),
            "Enter Answer  Esc Cancel".to_string(),
        ];
        candidates
            .iter()
            .find(|c| c.chars().count() <= width)
            .unwrap_or(&candidates[candidates.len() - 1])
            .clone()
    }

    pub fn render(&self, theme: &Theme, width: usize) -> Vec<String> {
        // The question itself, bold with a two-column indent; the first
        // wrapped row rides the frame's header slot.
        let mut question_rows = wrap_styled(&self.question, width.saturating_sub(2).max(8))
            .into_iter()
            .map(|row| format!("  {}", bold(&row)));
        let header = question_rows.next().unwrap_or_default();
        let mut body: Vec<String> = question_rows.collect();

        // `  n) label` rows; the description column clears the widest label.
        // Measured in display cells, not chars — wide glyphs must clear
        // the description column too.
        let description_col = 2
            + self
                .options
                .iter()
                .map(|(label, _)| visible_width(label) + 3)
                .max()
                .unwrap_or(0)
            + 3;
        for (i, (label, description)) in self.options.iter().enumerate() {
            let plain = format!("{}) {label}", i + 1);
            let styled = if i == self.selected {
                bold(&theme.fg("userMessageText", &plain))
            } else {
                theme.fg("dim", &plain)
            };
            let mut row = format!("  {styled}");
            if !description.is_empty() && width > description_col {
                let pad = description_col.saturating_sub(2 + visible_width(&plain));
                row.push_str(&theme.fg("dim", &format!("{}{description}", " ".repeat(pad))));
            }
            body.push(crate::tui::markdown::clip_styled(&row, width));
        }
        if self.allow_freeform {
            let n = self.options.len() + 1;
            let row = if self.freeform_selected() {
                let typed = &self.freeform;
                format!(
                    "  {} {}\x1b[7m \x1b[27m",
                    bold(&theme.fg("userMessageText", &format!("{n})"))),
                    typed
                )
            } else if self.freeform.is_empty() {
                format!("  {}", theme.fg("dim", &format!("{n}) Type an answer…")))
            } else {
                format!("  {}", theme.fg("dim", &format!("{n}) {}", self.freeform)))
            };
            body.push(crate::tui::markdown::clip_styled(&row, width));
        }
        crate::tui::panel::frame(theme, width, header, body)
    }
}
