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

use crate::tui::markdown::{visible_width, wrap_styled};
use crate::tui::render::bold;
use crate::tui::theme::Theme;

pub struct TrustStage {
    pub selected: usize,
    /// First visible body row; None follows a newly selected choice.
    pub scroll: Option<usize>,
    /// The broader ancestor the middle row offers, when one exists.
    pub parent: Option<PathBuf>,
}

impl TrustStage {
    pub fn new(cwd: &std::path::Path) -> Self {
        TrustStage {
            selected: 0,
            scroll: Some(0),
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
                format!("Trust {}", safe_label(&home_relative(parent))),
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
        self.scroll = None;
    }

    /// Page through wrapped trust text without changing the pending decision.
    pub fn page(&mut self, delta: isize, width: usize, height: usize) {
        let page = height.saturating_sub(scroll_hint(width).len()).max(1);
        self.scroll = Some(
            self.scroll
                .unwrap_or(0)
                .saturating_add_signed(delta * page as isize),
        );
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

/// Paths are untrusted even before the user answers the trust question.
fn safe_label(text: &str) -> String {
    crate::core::tools::sanitize_display(text).replace('\n', " ")
}

/// Wrap prose with the reference indent, leaving enough room for wide glyphs.
fn prose(text: &str, width: usize) -> Vec<String> {
    let indent = " ".repeat(3.min(width.saturating_sub(2)));
    wrap_styled(text, width.saturating_sub(indent.len()).max(1))
        .into_iter()
        .map(|row| format!("{indent}{row}"))
        .collect()
}

/// Lay out every row before windowing so a long path is never clipped away.
fn body(
    stage: &TrustStage,
    theme: &Theme,
    width: usize,
    dir: &str,
) -> (Vec<String>, std::ops::Range<usize>) {
    let dim = |s: &str| theme.fg("dim", s);
    let choices = stage.choices();
    let label_col = choices
        .iter()
        .map(|(label, _)| visible_width(label))
        .max()
        .unwrap_or(0);
    let mut rows = vec![String::new()];
    rows.extend(
        prose(&format!("Trust {}?", safe_label(dir)), width)
            .iter()
            .map(|row| dim(row)),
    );
    rows.extend(prose("e reads the directory's AGENTS.md and .e/ skills+prompts into context, and runs tools here.", width).iter().map(|row| dim(row)));
    rows.push(String::new());
    let mut selected_range = 0..0;
    for (index, (label, description)) in choices.iter().enumerate() {
        let start = rows.len();
        let selected = stage.selected == index;
        let caret = if selected { "› " } else { "  " };
        let pad = " ".repeat(label_col - visible_width(label));
        let text = format!("{caret}{label}{pad}   {description}");
        let choice_rows = if visible_width(&text) <= width {
            vec![text]
        } else {
            let mut wrapped = wrap_styled(&format!("{caret}{label}"), width.max(2));
            wrapped.extend(prose(description, width));
            wrapped
        };
        rows.extend(choice_rows.iter().map(|row| {
            if selected {
                bold(&theme.fg("userMessageText", row))
            } else {
                dim(row)
            }
        }));
        if selected {
            selected_range = start..rows.len();
        }
    }
    rows.push(String::new());
    (rows, selected_range)
}

/// The scrolling hint is file-backed like other user-facing preferences.
fn scroll_hint(width: usize) -> Vec<String> {
    let hint = crate::core::config::settings::get_string("trust_scroll_hint")
        .unwrap_or_else(|| "↑↓ Choose · Enter Continue · PgUp/PgDn Scroll".into());
    prose(&safe_label(&hint), width)
}

/// Full trust panel, preserving the wide layout and wrapping narrow rows.
pub fn render(stage: &TrustStage, theme: &Theme, width: usize, dir: &str) -> Vec<String> {
    let (mut rows, _) = body(stage, theme, width, dir);
    rows.extend(
        prose("↑↓ Choose · Enter Continue", width)
            .iter()
            .map(|row| theme.fg("dim", row)),
    );
    rows
}

/// Fit trust text above the status row. Page keys expose every wrapped row;
/// changing the selected choice brings its label into view without clipping it.
pub fn render_view(
    stage: &mut TrustStage,
    theme: &Theme,
    width: usize,
    height: usize,
    dir: &str,
) -> Vec<String> {
    let full = render(stage, theme, width, dir);
    if full.len() <= height {
        stage.scroll = Some(0);
        return full;
    }
    if height == 0 {
        return Vec::new();
    }
    let (rows, selected) = body(stage, theme, width, dir);
    let mut hint = scroll_hint(width);
    hint.truncate(height.saturating_sub(1));
    let cap = height.saturating_sub(hint.len()).max(1);
    let offset = stage
        .scroll
        .unwrap_or_else(|| selected.end.saturating_sub(cap).min(selected.start))
        .min(rows.len().saturating_sub(cap));
    stage.scroll = Some(offset);
    let mut visible = rows.into_iter().skip(offset).take(cap).collect::<Vec<_>>();
    visible.extend(hint.iter().map(|row| theme.fg("dim", row)));
    visible
}
