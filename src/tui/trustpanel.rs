//! The first-visit trust panel — the auth surface's shape (three-space prose,
//! `› ` caret on the selected choice) asking whether e may load this
//! directory's own instructions. Shown once per directory; the answer
//! persists in ~/.e/trust.json.

use crate::tui::authpanel::choice_row;
use crate::tui::theme::Theme;

pub struct TrustStage {
    pub selected: usize,
}

pub fn render(stage: &TrustStage, theme: &Theme, width: usize, dir: &str) -> Vec<String> {
    let dim = |s: &str| theme.fg("dim", s);
    vec![
        String::new(),
        dim(&format!("   Trust {dir}?")),
        dim("   e reads the directory's AGENTS.md into context and runs tools here."),
        String::new(),
        choice_row(
            theme,
            stage.selected == 0,
            "Trust this directory",
            "remembered in ~/.e/trust.json",
            width,
        ),
        choice_row(
            theme,
            stage.selected == 1,
            "Not now",
            "work here without its instructions",
            width,
        ),
        String::new(),
        dim("   ↑↓ Choose · Enter Continue"),
    ]
}
