//! The transcript: ordered blocks and the blank rows between them.
//!
//! Spacing is decided in exactly one place: one blank row between blocks,
//! runs of tool rows contiguous. Blocks render (and cache) their final lines
//! at a given width; streaming touches only the tail block.

use crate::core::tools::ToolOutcome;
use crate::tui::markdown::{render_markdown, wrap_styled};
use crate::tui::render::{bold, dim};
use crate::tui::theme::Theme;

/// One stable child of a provider-issued tool batch.
pub struct ToolChild {
    pub id: u64,
    pub category: String,
    pub running: String,
    pub completed: String,
    pub target: String,
    pub state: ToolState,
    pub result: Option<String>,
    pub output: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolState {
    Pending,
    Running,
    Completed,
    Failed,
    TimedOut,
    Blocked,
    Cancelled,
}

impl ToolChild {
    pub fn pending(
        id: u64,
        category: String,
        running: String,
        completed: String,
        target: String,
    ) -> Self {
        Self {
            id,
            category,
            running,
            completed,
            target,
            state: ToolState::Pending,
            result: None,
            output: String::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Banner,
    User,
    Assistant,
    /// One assistant turn's streamed thinking — drawn live while the burst
    /// runs, then collapsed to a single dim `Thought for Ns` row when the
    /// burst ends (reply text, tools, retry, steer, turn commit).
    Thinking,
    Tool,
    /// One provider-issued batch: a tallied header over stable lifecycle
    /// children, including a single call in minimal mode.
    ToolGroup,
    /// A `!` shell passthrough: `$ cmd` header, output tail below.
    Shell,
    Summary,
    Notice,
    /// A turn-level failure: the reference's error color, persisted in the
    /// transcript — an ending you can see, not a status blip.
    Error,
    /// A system lifecycle fact in the reference grammar: `● System: …`.
    System,
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
    /// Legacy collapsed children, retained for restored/static rows.
    pub children: Vec<String>,
    /// Live lifecycle children. A batch creates these before execution.
    pub tool_children: Vec<ToolChild>,
    /// The turn was interrupted while this tool ran — the reference's `■`.
    pub cancelled: bool,
    /// Command rows: the first output lines, shown as `│` rows beneath.
    pub preview: Vec<String>,
    /// Output lines beyond the preview (drives the elision row).
    pub more: usize,
    cache: Option<(usize, bool, Vec<String>)>,
}

impl Block {
    pub fn new(kind: Kind, text: impl Into<String>) -> Self {
        Block {
            kind,
            // Block text is source text, not markup: model output, extension
            // notices, and pasted content must render inert, never smuggle
            // terminal control sequences into the paint stream.
            text: crate::core::tools::sanitize_display(&text.into()),
            done: false,
            is_error: false,
            detail: None,
            result: None,
            children: Vec::new(),
            tool_children: Vec::new(),
            cancelled: false,
            preview: Vec::new(),
            more: 0,
            cache: None,
        }
    }

    pub fn touch(&mut self) {
        self.cache = None;
    }

    /// Build a live group whose header and child order are known up front.
    pub fn tool_group(children: Vec<ToolChild>) -> Self {
        let mut block = Block::new(Kind::ToolGroup, "");
        block.tool_children = children;
        block.refresh_tool_header();
        block
    }

    pub fn start_tool(&mut self, id: u64) {
        if let Some(child) = self.tool_children.iter_mut().find(|child| child.id == id) {
            child.state = ToolState::Running;
            self.touch();
        }
    }

    pub fn append_tool_output(&mut self, id: u64, chunk: &str) {
        const DISPLAY_CAP: usize = 64 * 1024;
        if let Some(child) = self.tool_children.iter_mut().find(|child| child.id == id) {
            let room = DISPLAY_CAP.saturating_sub(child.output.len());
            if room > 0 {
                let mut take = room.min(chunk.len());
                while take > 0 && !chunk.is_char_boundary(take) {
                    take -= 1;
                }
                child.output.push_str(&chunk[..take]);
            }
            self.touch();
        }
    }

    pub fn finish_tool(&mut self, id: u64, outcome: ToolOutcome, summary: String, content: &str) {
        if let Some(child) = self.tool_children.iter_mut().find(|child| child.id == id) {
            // A detached task's late result must not resurrect a row Esc
            // already settled.
            if child.state == ToolState::Cancelled {
                return;
            }
            child.state = match outcome {
                ToolOutcome::Completed => ToolState::Completed,
                ToolOutcome::Failed => ToolState::Failed,
                ToolOutcome::TimedOut => ToolState::TimedOut,
                ToolOutcome::Blocked => ToolState::Blocked,
                ToolOutcome::Cancelled => ToolState::Cancelled,
            };
            child.result = Some(summary);
            // Command output is the only thing the live preview draws; keep
            // it captured for a bash tool still running when a late finish
            // lands.
            if child.output.is_empty() && child.category == "command" {
                child.output = crate::core::tools::sanitize_display(content);
            }
        }
        self.refresh_tool_header();
        self.touch();
    }

    pub fn cancel_unfinished_tools(&mut self) {
        for child in &mut self.tool_children {
            if matches!(child.state, ToolState::Pending | ToolState::Running) {
                child.state = ToolState::Cancelled;
                child.result = Some("cancelled".into());
            }
        }
        self.refresh_tool_header();
        self.touch();
    }

    fn refresh_tool_header(&mut self) {
        let mut counts: Vec<(String, usize)> = Vec::new();
        let mut failed = 0usize;
        let mut cancelled = 0usize;
        for child in &self.tool_children {
            if matches!(
                child.state,
                ToolState::Failed | ToolState::TimedOut | ToolState::Blocked
            ) {
                failed += 1;
            } else if child.state == ToolState::Cancelled {
                cancelled += 1;
            }
            if let Some((_, count)) = counts.iter_mut().find(|(kind, _)| *kind == child.category) {
                *count += 1;
            } else {
                counts.push((child.category.clone(), 1));
            }
        }
        let count = self.tool_children.len();
        let mut header = format!("{count} tool call{}", if count == 1 { "" } else { "s" });
        for (category, count) in counts {
            header.push_str(&format!(" · {}", category_tally(&category, count)));
        }
        if failed > 0 {
            header.push_str(&format!(" · {failed} failed"));
        }
        if cancelled > 0 {
            header.push_str(&format!(" · {cancelled} cancelled"));
        }
        self.text = header;
    }

    /// Render for tests: the same rows lines() caches, without the cache.
    pub fn lines_for_test(&self, theme: &Theme, width: usize) -> Vec<String> {
        self.render(theme, width, true)
    }

    fn lines(&mut self, theme: &Theme, width: usize, blink_on: bool) -> &[String] {
        // Only a running tool row renders differently across blink phases.
        // Pin every other block to one phase so the blink tick can't
        // invalidate the whole transcript's caches during a turn.
        let phase = blink_on && self.animates();
        let valid = matches!(&self.cache, Some((w, p, _)) if *w == width && *p == phase);
        if !valid {
            let lines = self.render(theme, width, phase);
            self.cache = Some((width, phase, lines));
        }
        &self.cache.as_ref().unwrap().2
    }

    /// Whether this block's rendering depends on the blink phase.
    fn animates(&self) -> bool {
        self.tool_children
            .iter()
            .any(|child| child.state == ToolState::Running)
    }

    fn render(&self, theme: &Theme, width: usize, blink_on: bool) -> Vec<String> {
        match self.kind {
            // `𝑒 dogfood · Run /help for commands` — name bold, rest dim.
            Kind::Banner => vec![format!(
                "{}{}",
                bold(&theme.fg("userMessageText", "𝑒")),
                theme.fg("muted", &format!(" {} · Run /help for commands", self.text))
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
            Kind::Thinking => {
                let text = self.text.trim();
                if text.is_empty() {
                    return Vec::new();
                }
                // Live thinking wears the palette's thinkingText; a collapsed
                // burst's summary row dims (`dim` on light is one step
                // further toward the background than statusline).
                let color = if self.done { "dim" } else { "thinkingText" };
                wrap_styled(text, width.saturating_sub(4).max(8))
                    .into_iter()
                    .map(|l| theme.fg(color, &format!("  · {l}")))
                    .collect()
            }
            Kind::Tool => {
                // The reference shape: a finished row is just the row — no
                // "(done)". Failure turns the marker to the error token and
                // adds a `│ <outcome>` continuation beneath.
                let marker = if self.cancelled {
                    theme.fg("warning", "■")
                } else if !self.done {
                    dim("●")
                } else if self.is_error {
                    theme.fg("error", "●")
                } else {
                    "●".to_string()
                };
                let mut rows = vec![match &self.detail {
                    Some(target) if !target.is_empty() => {
                        format!("{marker} {} {}", self.text, theme.fg("muted", target))
                    }
                    _ => format!("{marker} {}", self.text),
                }];
                if self.done {
                    // The reference's command-output shape: the first lines
                    // as `│` rows, an elision row for the rest, and an exit
                    // line ("│ exit code 7") when the command failed.
                    for line in &self.preview {
                        rows.push(theme.fg("muted", &format!("│ {line}")));
                    }
                    if self.is_error {
                        if let Some(result) = &self.result {
                            let shown = display_outcome(result);
                            rows.push(theme.fg("muted", &format!("│ {shown}")));
                        }
                    }
                    if self.more > 0 {
                        rows.push(theme.fg(
                            "muted",
                            &format!("│ … {} lines more (ctrl o to view)", self.more),
                        ));
                    }
                }
                rows
            }
            Kind::ToolGroup => {
                // The reference grammar runs the tool family flush left — the
                // same column as the user rail — never indented.
                let marker = bold(&theme.fg("userMessageText", "●"));
                let mut rows = vec![format!("{marker} {}", theme.fg("statusline", &self.text))];
                if self.tool_children.is_empty() {
                    for (i, child) in self.children.iter().enumerate() {
                        let connector = if i + 1 == self.children.len() {
                            "└"
                        } else {
                            "├"
                        };
                        rows.push(format!("{connector} {}", theme.fg("statusline", child)));
                    }
                    return rows;
                }
                for (index, child) in self.tool_children.iter().enumerate() {
                    if child.state == ToolState::Pending {
                        continue;
                    }
                    let connector = if index + 1 == self.tool_children.len() {
                        "└"
                    } else {
                        "├"
                    };
                    let connector = match child.state {
                        ToolState::Running if blink_on => theme.fg("userMessageText", connector),
                        ToolState::Running => theme.fg("dim", connector),
                        ToolState::Failed | ToolState::TimedOut | ToolState::Blocked => {
                            theme.fg("error", connector)
                        }
                        ToolState::Cancelled => theme.fg("warning", connector),
                        _ => theme.fg("muted", connector),
                    };
                    let action = match child.state {
                        ToolState::Running => child.running.as_str(),
                        ToolState::Completed => child.completed.as_str(),
                        ToolState::Failed => "Failed",
                        ToolState::TimedOut => "Timed out",
                        ToolState::Blocked => "Blocked",
                        ToolState::Cancelled => "Cancelled",
                        ToolState::Pending => unreachable!(),
                    };
                    let suffix = if child.state == ToolState::Completed
                        && matches!(child.category.as_str(), "edit" | "write")
                    {
                        child
                            .result
                            .as_deref()
                            .map(|result| format!("  {result}"))
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    let available =
                        width.saturating_sub(2 + action.chars().count() + suffix.chars().count());
                    let target = clip_plain(&child.target, available);
                    let label = if target.is_empty() {
                        format!("{action}{suffix}")
                    } else {
                        format!("{action} {target}{suffix}")
                    };
                    rows.push(format!("{connector} {}", theme.fg("statusline", &label)));
                    append_tool_preview(&mut rows, child, theme);
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
            Kind::Summary => vec![theme.fg("dim", &format!("  {}", self.text))],
            Kind::Notice => wrap_styled(&self.text, width.saturating_sub(2).max(8))
                .into_iter()
                .map(|l| format!("  {l}"))
                .collect(),
            Kind::Error => wrap_styled(&self.text, width.saturating_sub(2).max(8))
                .into_iter()
                .map(|l| theme.fg("error", &format!("  {l}")))
                .collect(),
            Kind::System => vec![theme.fg("dim", &format!("● System: {}", self.text))],
        }
    }
}

fn category_tally(category: &str, count: usize) -> String {
    if category == "command" && count > 1 {
        format!("{count} commands")
    } else {
        format!("{count} {category}")
    }
}

fn display_outcome(result: &str) -> String {
    match result.strip_prefix("exit ") {
        Some(code) => format!("exit code {code}"),
        None => result.to_string(),
    }
}

fn clip_plain(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".into();
    }
    let mut clipped: String = text.chars().take(width - 1).collect();
    clipped.push('…');
    clipped
}

/// Pipe rows appear only while the command owns execution focus — the
/// reference grammar. Completion withdraws them; full output lives behind
/// ctrl+o, never inline. Live rows are a shell thing: a write or edit shows
/// in the tree as its one action row, never as the file's content streaming
/// beneath it.
fn append_tool_preview(rows: &mut Vec<String>, child: &ToolChild, theme: &Theme) {
    if child.state != ToolState::Running {
        return;
    }
    if child.category != "command" {
        return;
    }
    let output: Vec<&str> = child
        .output
        .lines()
        .filter(|line| !line.starts_with("… [killed:") && *line != "… [cancelled]")
        .collect();
    const LIVE_BUDGET: usize = 5;
    for line in output.iter().take(LIVE_BUDGET) {
        let styled = if line.starts_with('+') && !line.starts_with("+++") {
            theme.fg("toolDiffAdded", line)
        } else if line.starts_with('-') && !line.starts_with("---") {
            theme.fg("toolDiffRemoved", line)
        } else {
            theme.fg("muted", line)
        };
        rows.push(format!("{} {styled}", theme.fg("muted", "│")));
    }
    let more = output.len().saturating_sub(LIVE_BUDGET);
    if more > 0 {
        rows.push(theme.fg("muted", &format!("│ … {more} lines more (ctrl o to view)")));
    }
}

/// Collapse legacy restored rows. Live batches use `tool_children` instead.
fn flush_run(out: &mut Vec<Block>, run: &mut Vec<Block>) {
    if run.is_empty() {
        return;
    }
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut failed = 0usize;
    let mut cancelled = 0usize;
    let mut children = Vec::with_capacity(run.len());
    for block in run.iter() {
        if block.cancelled {
            cancelled += 1;
        } else if block.is_error {
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
    if cancelled > 0 {
        header.push_str(&format!(" · {cancelled} cancelled"));
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

    /// Convert legacy standalone finished rows into minimal-mode groups.
    /// Live session events create groups directly and never call this.
    pub fn collapse_tools(&mut self) {
        let mut out: Vec<Block> = Vec::with_capacity(self.blocks.len());
        let mut run: Vec<Block> = Vec::new();
        for block in self.blocks.drain(..) {
            if block.kind == Kind::Tool && (block.done || block.cancelled) {
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
        self.render_animated(theme, width, true)
    }

    pub fn render_animated(&mut self, theme: &Theme, width: usize, blink_on: bool) -> Vec<String> {
        let mut out = Vec::new();
        let mut prev: Option<Kind> = None;
        for block in &mut self.blocks {
            let kind = block.kind;
            let lines = block.lines(theme, width, blink_on);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        crate::tui::theme::load_bundled(false).unwrap()
    }

    /// The blink phase must not invalidate finished blocks: during a turn the
    /// tick flips the phase twice a second, and re-rendering the whole
    /// transcript's markdown on each flip is what made streaming lag.
    #[test]
    fn blink_flip_keeps_static_block_cache() {
        let theme = theme();
        let mut block = Block::new(Kind::Assistant, "some **finished** reply");
        block.lines(&theme, 80, true);
        let cached = block.cache.as_ref().unwrap().2.as_ptr();
        block.lines(&theme, 80, false);
        assert_eq!(
            block.cache.as_ref().unwrap().2.as_ptr(),
            cached,
            "a blink flip re-rendered a block with no running tool"
        );
    }

    /// A running tool row genuinely blinks, so its block must re-render
    /// across phases — and settle again once the tool finishes.
    #[test]
    fn running_tool_block_animates_until_done() {
        let theme = theme();
        let mut block = Block::tool_group(vec![ToolChild::pending(
            1,
            "command".into(),
            "Running".into(),
            "Ran".into(),
            "true".into(),
        )]);
        block.start_tool(1);
        let on = block.lines(&theme, 80, true).to_vec();
        let off = block.lines(&theme, 80, false).to_vec();
        assert_ne!(on, off, "a running tool row must blink");
        block.finish_tool(1, ToolOutcome::Completed, "done".into(), "");
        block.lines(&theme, 80, true);
        let cached = block.cache.as_ref().unwrap().2.as_ptr();
        block.lines(&theme, 80, false);
        assert_eq!(block.cache.as_ref().unwrap().2.as_ptr(), cached);
    }

    /// Thinking streams live in thinkingText; once its burst ends, the
    /// collapsed block is a single dim summary row.
    #[test]
    fn thinking_streams_live_then_collapses_to_one_dim_row() {
        // Distinct colors so the live/collapsed shift is observable.
        let theme = Theme::from_json(
            r#"{"vars":{"a":250,"b":240},"colors":{"thinkingText":"a","dim":"b"}}"#,
        )
        .unwrap();
        let mut block = Block::new(Kind::Thinking, "let me look at this\nstep two");
        let live = block.lines_for_test(&theme, 40);
        assert!(live.len() >= 2, "thinking rows render");
        for row in &live {
            assert!(row.contains("·"), "thinking rows carry a marker");
            assert!(
                row.contains(theme.fg_prefix("thinkingText")),
                "live thinking wears thinkingText"
            );
        }
        // Burst end: events.rs swaps in the summary text and marks the
        // block done; only the paint contract is pinned here.
        block.text = "Thought for 12s".into();
        block.done = true;
        block.touch();
        let collapsed = block.lines_for_test(&theme, 40);
        assert_eq!(collapsed.len(), 1, "a collapsed burst is one row");
        assert!(collapsed[0].contains("Thought for 12s"));
        assert!(
            collapsed[0].contains(theme.fg_prefix("dim")),
            "the collapsed row dims"
        );
    }

    /// Block text stays inert even for the thinking surface.
    #[test]
    fn thinking_text_is_sanitized() {
        let theme = theme();
        let block = Block::new(Kind::Thinking, "ho \x1b[2Jho and \x1b]52;c;x\x07 tail");
        for row in block.lines_for_test(&theme, 40) {
            // The theme's own styling escapes remain; the injected ones go.
            assert!(
                !row.contains("\x1b[2J"),
                "an erase sequence leaked: {row:?}"
            );
            assert!(!row.contains("\x1b]"), "an OSC sequence leaked: {row:?}");
        }
    }
}
