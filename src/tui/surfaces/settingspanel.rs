//! The settings panel — the reference settings-screen shape, over
//! `settings::all()`.
//!
//! Rows and their options come from `~/.e/`-aware settings, so a user-editable
//! set (theme) shows every file they've dropped in. `←→` change the selected
//! row's value; `↑↓` move; framing and the hint come from the shared panel.

use crate::core::config::settings::{self, Setting};
use crate::tui::markdown::visible_width;
use crate::tui::render::bold;
use crate::tui::theme::Theme;

pub struct SettingsPanel {
    settings: Vec<Setting>,
    pub selected: usize,
}

const VALUE_COL: usize = 20;
pub const HINT: &str = "↑↓ Navigate     ←→ Change     Esc Close";

impl SettingsPanel {
    /// `effort_levels`: the current model's declared effort levels, so the
    /// panel cycles exactly what the model supports.
    pub fn new(effort_levels: Vec<String>) -> Self {
        SettingsPanel {
            settings: settings::all(effort_levels),
            selected: 0,
        }
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.settings.len() as isize;
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    /// Change the selected setting's value; persisted immediately. Reloads the
    /// option sets so a value that adds choices (never today) stays coherent.
    pub fn change(&mut self, dir: i32) -> std::io::Result<()> {
        if let Some(setting) = self.settings.get(self.selected) {
            setting.cycle(dir)?;
        }
        Ok(())
    }

    pub fn render(&self, theme: &Theme, width: usize) -> Vec<String> {
        let header = bold(&theme.fg("userMessageText", "Settings"));
        let mut body: Vec<String> = Vec::new();
        let mut last_category = "";
        for (i, setting) in self.settings.iter().enumerate() {
            if setting.category != last_category {
                if !body.is_empty() {
                    body.push(String::new());
                }
                body.push(theme.fg("dim", setting.category));
                last_category = setting.category;
            }
            let selected = i == self.selected;
            let label = if selected {
                bold(&theme.fg("userMessageText", &setting.label))
            } else {
                theme.fg("dim", &setting.label)
            };
            let mut line = format!("  {label}");
            let pad = VALUE_COL.saturating_sub(visible_width(&line));
            line.push_str(&" ".repeat(pad));
            let current = setting.current();
            let opts: Vec<String> = setting
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
