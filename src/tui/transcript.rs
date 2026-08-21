//! The transcript: ordered blocks and the blank rows between them.
//!
//! Spacing is decided in exactly one place: one blank row between blocks,
//! runs of tool rows contiguous. Blocks render (and cache) their final lines
//! at a given width; streaming touches only the tail block.

use crate::tui::markdown::{render_markdown, wrap_styled};
use crate::tui::render::{bold, dim};
use crate::tui::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Banner,
    User,
    Assistant,
    Reasoning,
    Tool,
    /// A `!` shell passthrough: `$ cmd` header, output tail below.
    Shell,
    Summary,
    Notice,
}

pub struct Block {
    pub kind: Kind,
    pub text: String,
    /// Tool rows carry extra state.
    pub done: bool,
    pub is_error: bool,
    pub detail: Option<String>,
    cache: Option<(usize, Vec<String>)>,
}

impl Block {
    pub fn new(kind: Kind, text: impl Into<String>) -> Self {
        Block {
            kind,
            text: text.into(),
            done: false,
            is_error: false,
            detail: None,
            cache: None,
        }
    }

    pub fn touch(&mut self) {
        self.cache = None;
    }

    fn lines(&mut self, theme: &Theme, width: usize) -> &[String] {
        let valid = matches!(&self.cache, Some((w, _)) if *w == width);
        if !valid {
            let lines = self.render(theme, width);
            self.cache = Some((width, lines));
        }
        &self.cache.as_ref().unwrap().1
    }

    fn render(&self, theme: &Theme, width: usize) -> Vec<String> {
        match self.kind {
            // `𝑒 v0.2.0 · Run /help for commands` — name bold, rest dim.
            Kind::Banner => vec![format!(
                "{}{}",
                bold(&theme.fg("userMessageText", "𝑒")),
                theme.fg(
                    "muted",
                    &format!(" v{} · Run /help for commands", self.text)
                )
            )],
            Kind::User => {
                let rail = format!("{} ", theme.fg("userMessageText", "┃"));
                let mut rows = Vec::new();
                for line in self.text.split('\n') {
                    if line.trim().is_empty() {
                        rows.push(theme.fg("userMessageText", "┃"));
                        continue;
                    }
                    for row in wrap_styled(line, width.saturating_sub(2).max(8)) {
                        rows.push(format!("{rail}{}", bold(&row)));
                    }
                }
                rows
            }
            Kind::Assistant => {
                let text = self.text.trim();
                if text.is_empty() {
                    return Vec::new();
                }
                render_markdown(theme, text, width.saturating_sub(2).max(8))
                    .into_iter()
                    .map(|l| if l.is_empty() { l } else { format!("  {l}") })
                    .collect()
            }
            Kind::Tool => {
                let marker = if self.done {
                    "●".to_string()
                } else {
                    dim("●")
                };
                let rows = vec![match &self.detail {
                    Some(target) if !target.is_empty() => {
                        format!("  {marker} {} {}", self.text, theme.fg("muted", target))
                    }
                    _ => format!("  {marker} {}", self.text),
                }];
                rows
            }
            Kind::Shell => {
                // The reference look: the command in the bash-mode color, the
                // output tail muted beneath it.
                let header = if self.done {
                    theme.fg(
                        "bashMode",
                        &crate::tui::render::bold(&format!("$ {}", self.text)),
                    )
                } else {
                    dim(&format!("$ {}", self.text))
                };
                let mut rows = vec![format!("  {header}")];
                if let Some(output) = &self.detail {
                    for line in output.lines() {
                        rows.push(format!("  {}", theme.fg("muted", line)));
                    }
                }
                rows
            }
            Kind::Reasoning => {
                let text = self.text.trim();
                if text.is_empty() {
                    return Vec::new();
                }
                // Dimmed, gutter-indented, italicized thinking.
                crate::tui::markdown::wrap_styled(text, width.saturating_sub(2).max(8))
                    .into_iter()
                    .map(|l| format!("  {}", theme.fg("dim", &crate::tui::render::italic(&l))))
                    .collect()
            }
            Kind::Summary => vec![theme.fg("dim", &format!("  {}", self.text))],
            Kind::Notice => wrap_styled(&self.text, width.saturating_sub(2).max(8))
                .into_iter()
                .map(|l| format!("  {l}"))
                .collect(),
        }
    }
}

fn gap(prev: Kind, next: Kind) -> usize {
    if prev == Kind::Tool && next == Kind::Tool {
        0
    } else {
        1
    }
}

// (reasoning uses the default one-row gap before the reply that follows it)

#[derive(Default)]
pub struct Transcript {
    pub blocks: Vec<Block>,
}

impl Transcript {
    pub fn push(&mut self, block: Block) -> usize {
        self.blocks.push(block);
        self.blocks.len() - 1
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
    }

    /// Drop every block's line cache (after a theme change).
    pub fn invalidate(&mut self) {
        for b in &mut self.blocks {
            b.touch();
        }
    }

    pub fn render(&mut self, theme: &Theme, width: usize) -> Vec<String> {
        let mut out = Vec::new();
        let mut prev: Option<Kind> = None;
        for block in &mut self.blocks {
            let kind = block.kind;
            let lines = block.lines(theme, width);
            if lines.is_empty() {
                continue;
            }
            if let Some(p) = prev {
                for _ in 0..gap(p, kind) {
                    out.push(String::new());
                }
            }
            out.extend_from_slice(lines);
            prev = Some(kind);
        }
        out
    }
}
