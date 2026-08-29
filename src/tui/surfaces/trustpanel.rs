//! The first-visit trust panel — three-space prose and a `› ` caret on the
//! selected choice, asking whether e may load this directory's own
//! instructions. Shown once per directory; the answer persists in
//! ~/.e/trust.json. When a broader ancestor makes sense (the top-most
//! directory under home that contains the workspace — `~/code` for
//! `~/code/clones/e-1`), a middle choice trusts it wholesale, covering
//! every workspace inside. Unlike the auth panel's wide value column, the
//! descriptions here sit right beside the choices — the question reads as
//! one block, not a table spanning the frame.

use std::path::PathBuf;

use crate::tui::markdown::clip_styled;
use crate::tui::render::bold;
use crate::tui::theme::Theme;

pub struct TrustStage {
    pub selected: usize,
    /// The broader ancestor the middle row offers, when one exists.
    pub parent: Option<PathBuf>,
}

impl TrustStage {
    pub fn new(cwd: &std::path::Path) -> Self {
        TrustStage {
            selected: 0,
            parent: crate::core::config::trust::parent_option(cwd),
        }
    }

    /// The selector rows, top to bottom: this directory, the broader
    /// ancestor (when offered), decline.
    pub fn choices(&self) -> Vec<(String, String)> {
        let mut rows = vec![(
            "Trust this directory".to_string(),
            "remembered in ~/.e/trust.json".to_string(),
        )];
        if let Some(parent) = &self.parent {
            rows.push((
                format!("Trust {}", home_relative(parent)),
                "everything inside it, this directory included".to_string(),
            ));
        }
        rows.push((
            "Not now".to_string(),
            "work here without its instructions".to_string(),
        ));
        rows
    }

    pub fn row_count(&self) -> usize {
        if self.parent.is_some() {
            3
        } else {
            2
        }
    }

    pub fn step(&mut self, delta: isize) {
        let n = self.row_count() as isize;
        self.selected = ((self.selected as isize + delta).rem_euclid(n)) as usize;
    }

    /// What Enter on the current row means: the directory to record and
    /// whether it is trusted (None = the workspace itself declined).
    pub fn choice(&self) -> (Option<PathBuf>, bool) {
        match (&self.parent, self.selected) {
            (Some(parent), 1) => (Some(parent.clone()), true),
            _ => (None, self.selected == 0),
        }
    }
}

/// `~`-relative display for a path under home, the workspace label's rule.
fn home_relative(path: &std::path::Path) -> String {
    let shown = path.to_string_lossy().into_owned();
    match crate::core::config::home::user_home() {
        Some(home) => {
            let home = home.to_string_lossy().into_owned();
            if shown.starts_with(&home) {
                format!("~{}", &shown[home.len()..])
            } else {
                shown
            }
        }
        None => shown,
    }
}

pub fn render(stage: &TrustStage, theme: &Theme, width: usize, dir: &str) -> Vec<String> {
    let dim = |s: &str| theme.fg("dim", s);
    let choices = stage.choices();
    let label_col = choices
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);
    let choice = |index: usize| {
        let (label, description) = &choices[index];
        let selected = stage.selected == index;
        let caret = if selected { "› " } else { "  " };
        let pad = " ".repeat(label_col - label.chars().count());
        let text = format!("{caret}{label}{pad}   {description}");
        let row = if selected {
            bold(&theme.fg("userMessageText", &text))
        } else {
            dim(&text)
        };
        clip_styled(&row, width)
    };
    let mut rows = vec![
        String::new(),
        dim(&format!("   Trust {dir}?")),
        dim("   e reads the directory's AGENTS.md and .e/ skills+prompts into context, and runs tools here."),
        String::new(),
    ];
    for index in 0..choices.len() {
        rows.push(choice(index));
    }
    rows.push(String::new());
    rows.push(dim("   ↑↓ Choose · Enter Continue"));
    rows
}
