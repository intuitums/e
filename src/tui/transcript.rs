//! The transcript: ordered blocks and the blank rows between them.
//!
//! Spacing is decided in exactly one place: one blank row between blocks,
//! runs of tool rows contiguous. Blocks render (and cache) their final lines
//! at a given width; streaming touches only the tail block.

use crate::tui::markdown::{render_markdown, wrap_styled};
use crate::tui::render::{bold, dim};
use crate::tui::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Banner,
    User,
    Assistant,
    Reasoning,
    Tool,
    /// Finished consecutive tool calls, collapsed to the reference group
    /// shape: a tallied header, `├` children, `└` last.
    ToolGroup,
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
    /// A finished tool's outcome summary ("exit 7", "timeout 5s"); rendered
    /// as a continuation line only on failure — the reference convention.
    pub result: Option<String>,
    /// ToolGroup children, plain "Verb target" per call, in order.
    pub children: Vec<String>,
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
            result: None,
            children: Vec::new(),
            cache: None,
        }
    }

    pub fn touch(&mut self) {
        self.cache = None;
    }

    /// Render for tests: the same rows lines() caches, without the cache.
    pub fn lines_for_test(&self, theme: &Theme, width: usize) -> Vec<String> {
        self.render(theme, width)
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
                // The reference shape: a finished row is just the row — no
                // "(done)". Failure turns the marker to the error token and
                // adds a `│ <outcome>` continuation beneath.
                let marker = if !self.done {
                    dim("●")
                } else if self.is_error {
                    theme.fg("error", "●")
                } else {
                    "●".to_string()
                };
                let mut rows = vec![match &self.detail {
                    Some(target) if !target.is_empty() => {
                        format!("  {marker} {} {}", self.text, theme.fg("muted", target))
                    }
                    _ => format!("  {marker} {}", self.text),
                }];
                if self.done && self.is_error {
                    if let Some(result) = &self.result {
                        rows.push(format!("    {}", theme.fg("muted", &format!("│ {result}"))));
                    }
                }
                rows
            }
            Kind::ToolGroup => {
                let mut rows = vec![format!("  ● {}", self.text)];
                for (i, child) in self.children.iter().enumerate() {
                    let connector = if i + 1 == self.children.len() {
                        "└"
                    } else {
                        "├"
                    };
                    rows.push(format!("  {} {child}", theme.fg("muted", connector)));
                }
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
                // Dimmed, gutter-indented, italicized thinking. Summaries
                // arrive as markdown (bold titles, code spans) — render the
                // inline spans instead of showing literal asterisks.
                let styled = crate::tui::markdown::inline_spans(theme, text);
                crate::tui::markdown::wrap_styled(&styled, width.saturating_sub(2).max(8))
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

/// The reference's tally label for a verb; only "command" pluralizes
/// ("2 read", "1 edit", "3 commands" — the reference's own literals).
fn flush_run(out: &mut Vec<Block>, run: &mut Vec<Block>) {
    if run.len() < 2 {
        out.append(run);
        return;
    }
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut failed = 0usize;
    let mut children = Vec::with_capacity(run.len());
    for block in run.iter() {
        if block.is_error {
            failed += 1;
        }
        let verb = block.text.clone();
        if let Some(entry) = counts.iter_mut().find(|(v, _)| *v == verb) {
            entry.1 += 1;
        } else {
            counts.push((verb.clone(), 1));
        }
        children.push(match &block.detail {
            Some(target) if !target.is_empty() => format!("{verb} {target}"),
            _ => verb,
        });
    }
    let mut header = format!(
        "{} tool call{}",
        run.len(),
        if run.len() == 1 { "" } else { "s" }
    );
    for (verb, count) in &counts {
        header.push_str(&format!(" · {}", tally_label(verb, *count)));
    }
    if failed > 0 {
        header.push_str(&format!(" · {failed} failed"));
    }
    let mut group = Block::new(Kind::ToolGroup, header);
    group.done = true;
    group.children = children;
    out.push(group);
    run.clear();
}

fn tally_label(verb: &str, count: usize) -> String {
    let label = match verb {
        "Read" => "read",
        "Wrote" => "write",
        "Edited" => "edit",
        "Ran" | "Running" => "command",
        "Searched" => "search",
        "Listed" => "list",
        other => return format!("{count} {}", other.to_lowercase()),
    };
    if label == "command" && count > 1 {
        format!("{count} commands")
    } else {
        format!("{count} {label}")
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

    /// Collapse every run of two or more finished tool rows into the
    /// reference group: `● N tool calls · tallies [· N failed]` over `├`/`└`
    /// children. Idempotent — groups never re-collapse; a still-running row
    /// ends its run and stays live.
    pub fn collapse_tools(&mut self) {
        let mut out: Vec<Block> = Vec::with_capacity(self.blocks.len());
        let mut run: Vec<Block> = Vec::new();
        for block in self.blocks.drain(..) {
            if block.kind == Kind::Tool && block.done {
                run.push(block);
                continue;
            }
            flush_run(&mut out, &mut run);
            out.push(block);
        }
        flush_run(&mut out, &mut run);
        self.blocks = out;
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
