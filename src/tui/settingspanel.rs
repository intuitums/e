//! The settings panel — fx's settings-screen shape.
//!
//! `Header`, then dim category labels, then one row per setting: the label at
//! the left, its options at a value column with the current one bright and the
//! rest dim. `←→` change the selected row's value; `↑↓` move; `Esc` closes —
//! fx's `↑↓ Navigate  ←→ Change  Esc Close`, no Enter.

use crate::core::settings::Choice;
use crate::render_width::visible_width;
use crate::tui::render::bold;
use crate::tui::theme::Theme;

/// A setting row and the category it falls under.
pub struct Row {
    pub category: &'static str,
    pub choice: &'static Choice,
}

/// e's settings, in category order.
pub fn rows() -> Vec<Row> {
    use crate::core::settings::{EFFORT, THEME};
    vec![
        Row { category: "Interface", choice: &THEME },
        Row { category: "Agent", choice: &EFFORT },
    ]
}

pub struct SettingsPanel {
    rows: Vec<Row>,
    pub selected: usize,
}

const VALUE_COL: usize = 20;
pub const HINT: &str = "↑↓ Navigate     ←→ Change     Esc Close";

impl SettingsPanel {
    pub fn new() -> Self {
        SettingsPanel { rows: rows(), selected: 0 }
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.rows.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    /// Change the selected setting's value; persisted immediately.
    pub fn change(&mut self, dir: i32) {
        if let Some(row) = self.rows.get(self.selected) {
            row.choice.cycle(dir);
        }
    }

    pub fn render(&self, theme: &Theme, width: usize) -> Vec<String> {
        let header = bold(&theme.fg("userMessageText", "Settings"));
        let mut body: Vec<String> = Vec::new();
        let mut last_category = "";
        for (i, row) in self.rows.iter().enumerate() {
            if row.category != last_category {
                if !body.is_empty() {
                    body.push(String::new());
                }
                body.push(theme.fg("dim", row.category));
                last_category = row.category;
            }
            let selected = i == self.selected;
            let label = if selected {
                bold(&theme.fg("userMessageText", row.choice.label))
            } else {
                theme.fg("dim", row.choice.label)
            };
            let mut line = format!("  {label}");
            let pad = VALUE_COL.saturating_sub(visible_width(&line));
            line.push_str(&" ".repeat(pad));
            let current = row.choice.current();
            let opts: Vec<String> = row
                .choice
                .options
                .iter()
                .map(|option| {
                    if *option == current {
                        bold(&theme.fg("userMessageText", option))
                    } else {
                        theme.fg("dim", option)
                    }
                })
                .collect();
            line.push_str(&opts.join("  "));
            body.push(line);
        }
        crate::tui::panel::frame(theme, width, header, body)
    }
}
