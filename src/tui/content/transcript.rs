//! The transcript: ordered blocks and the blank rows between them.
//!
//! Spacing is decided in exactly one place: one blank row between blocks,
//! runs of tool rows contiguous. Blocks render (and cache) their final lines
//! at a given width; streaming touches only the tail block.

use crate::core::tools::ToolOutcome;
use crate::tui::markdown::{render_markdown, wrap_styled};
use crate::tui::render::{bold, dim};
use crate::tui::theme::Theme;
use unicode_width::UnicodeWidthChar;

/// Review details are either a retained result or a running child's buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolDetail {
    Stored(u64),
    Live(usize),
}

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
    pub output_truncated: bool,
    /// The stored full output's id, for the ctrl+o review screen.
    pub detail: Option<u64>,
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
            category: crate::core::tools::sanitize_display(&category),
            running: crate::core::tools::sanitize_display(&running),
            completed: crate::core::tools::sanitize_display(&completed),
            target: crate::core::tools::sanitize_display(&target),
            state: ToolState::Pending,
            result: None,
            output: String::new(),
            output_truncated: false,
            detail: None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Banner,
    User,
    Assistant,
    /// Streamed thinking remains expanded after its burst ends. Legacy
    /// summary blocks are marked done and may be absorbed by a tool tree.
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

/// One rendered block version. Streaming blocks may briefly retain a stale
/// version so full-document Markdown parsing stays within a fixed work rate.
struct RenderCache {
    width: usize,
    phase: bool,
    generation: u64,
    rendered_at: std::time::Instant,
    lines: Vec<String>,
}

/// Pace complete-source rendering to at most 4 MiB of source per second.
/// Small replies retain the 33 ms frame cadence; larger ones update less
/// often instead of consuming progressively more work on every frame.
fn stream_render_interval(source_bytes: usize) -> std::time::Duration {
    const BYTES_PER_SECOND: u128 = 4 * 1024 * 1024;
    const FRAME_NANOS: u128 = 33_000_000;
    let nanos = (source_bytes as u128)
        .saturating_mul(1_000_000_000)
        .div_ceil(BYTES_PER_SECOND)
        .max(FRAME_NANOS)
        .min(u64::MAX as u128);
    std::time::Duration::from_nanos(nanos as u64)
}

pub struct Block {
    pub kind: Kind,
    pub text: String,
    /// Tool completion, or a legacy collapsed thinking summary. Finishing a
    /// live thinking burst does not mark it done or remove its source text.
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
    /// More output rows than the preview shows (drives the elision row).
    pub more: usize,
    /// Maximum wrapped rows in a live command preview.
    pub live_preview_rows: usize,
    cache: Option<RenderCache>,
    /// True while provider deltas are appending to this text block.
    streaming: bool,
    /// Bumped on every touch — the review screen's cache key folds these
    /// into one fingerprint so its projection rebuilds only when a block
    /// actually changed.
    generation: u64,
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
            live_preview_rows: 5,
            cache: None,
            streaming: false,
            generation: 0,
        }
    }

    pub fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.cache = None;
    }

    /// Append one untrusted provider delta without copying the accumulated
    /// response. Rendering is paced separately from event ingestion.
    pub fn append_streaming(&mut self, delta: &str) {
        self.text
            .push_str(&crate::core::tools::sanitize_display(delta));
        self.generation = self.generation.wrapping_add(1);
        self.streaming = true;
    }

    /// End a streamed segment and force its complete source through the next
    /// render, regardless of the live parsing budget.
    pub fn finish_streaming(&mut self) {
        if self.streaming {
            self.streaming = false;
            self.cache = None;
        }
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
            let chunk = crate::core::tools::sanitize_display(chunk);
            child.output.push_str(&chunk);
            if child.output.len() > DISPLAY_CAP {
                let mut remove = child.output.len() - DISPLAY_CAP;
                while !child.output.is_char_boundary(remove) {
                    remove += 1;
                }
                child.output.drain(..remove);
                child.output_truncated = true;
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
            child.result = Some(crate::core::tools::sanitize_display(&summary));
            // Streaming chunks may be dropped under backpressure. The final
            // retained result is authoritative once the command finishes.
            if child.category == "command" {
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

    /// Close a restored group: no more children arrive, and any child still
    /// pending never reported — the header says so instead of silently
    /// tallying rows that don't render.
    pub fn seal(&mut self) {
        self.done = true;
        self.refresh_tool_header();
        self.touch();
    }

    fn refresh_tool_header(&mut self) {
        let mut counts: Vec<(String, usize)> = Vec::new();
        let mut unreported = 0usize;
        let mut timed_out = 0usize;
        let mut failed = 0usize;
        let mut denied = 0usize;
        let mut cancelled = 0usize;
        for child in &self.tool_children {
            match child.state {
                // Live groups still have work in flight; a sealed group's
                // pending child is a recorded call whose result never came.
                ToolState::Pending if self.done => unreported += 1,
                ToolState::TimedOut => timed_out += 1,
                ToolState::Failed => failed += 1,
                ToolState::Blocked => denied += 1,
                ToolState::Cancelled => cancelled += 1,
                _ => {}
            }
            if let Some((_, count)) = counts.iter_mut().find(|(kind, _)| *kind == child.category) {
                *count += 1;
            } else {
                counts.push((child.category.clone(), 1));
            }
        }
        // The reference orders category tallies by descending count (a stable
        // sort keeps first-seen order for ties), then the outcome tallies in
        // its fixed grammar: unreported · timed out · failed · denied ·
        // cancelled.
        counts.sort_by_key(|a| std::cmp::Reverse(a.1));
        let count = self.tool_children.len();
        let mut header = format!("{count} tool call{}", if count == 1 { "" } else { "s" });
        for (category, count) in counts {
            header.push_str(&format!(" · {}", category_tally(&category, count)));
        }
        for (count, label) in [
            (unreported, "unreported"),
            (timed_out, "timed out"),
            (failed, "failed"),
            (denied, "denied"),
            (cancelled, "cancelled"),
        ] {
            if count > 0 {
                header.push_str(&format!(" · {count} {label}"));
            }
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
        let geometry_matches = self
            .cache
            .as_ref()
            .is_some_and(|cache| cache.width == width && cache.phase == phase);
        let current = self
            .cache
            .as_ref()
            .is_some_and(|cache| cache.generation == self.generation);
        let within_stream_budget = self.streaming
            && geometry_matches
            && self.cache.as_ref().is_some_and(|cache| {
                cache.rendered_at.elapsed() < stream_render_interval(self.text.len())
            });
        if !geometry_matches || (!current && !within_stream_budget) {
            let lines = self.render(theme, width, phase);
            self.cache = Some(RenderCache {
                width,
                phase,
                generation: self.generation,
                rendered_at: std::time::Instant::now(),
                lines,
            });
        }
        match &self.cache {
            Some(cache) => &cache.lines,
            // Unreachable: the cache was just filled above. An empty slice
            // renders as nothing rather than panicking if that ever breaks.
            None => &[],
        }
    }

    /// The review screen's projection of this block: every row, with child
    /// rows carrying their stored-detail id so the screen can splice the
    /// full output beneath. Details attach to the final wrapped action row.
    pub fn review_lines(
        &mut self,
        theme: &Theme,
        width: usize,
    ) -> Vec<(String, Option<ToolDetail>)> {
        if self.kind != Kind::ToolGroup || self.tool_children.is_empty() {
            // The cached path: the projection pays only for blocks whose
            // content actually changed, sharing the main transcript's cache.
            return self
                .lines(theme, width, false)
                .iter()
                .cloned()
                .map(|row| (row, None))
                .collect();
        }
        let marker = theme.fg("muted", "●");
        let header = clip_plain(&self.text, width.saturating_sub(2));
        let mut rows = vec![(format!("{marker} {}", theme.fg("muted", &header)), None)];
        let last_visible = self
            .tool_children
            .iter()
            .rposition(|child| child.state != ToolState::Pending || self.done);
        for (index, child) in self.tool_children.iter().enumerate() {
            let connector = if Some(index) == last_visible {
                "└"
            } else {
                "├"
            };
            if child.state == ToolState::Pending && !self.done {
                continue;
            }
            if child.state == ToolState::Pending {
                rows.extend(
                    tree_rows(
                        theme,
                        width,
                        connector,
                        &theme.fg("muted", "Tool completion was not reported"),
                    )
                    .into_iter()
                    .map(|row| (row, None)),
                );
                continue;
            }
            let wrapped = child_rows(theme, width, child, connector);
            let last = wrapped.len().saturating_sub(1);
            rows.extend(wrapped.into_iter().enumerate().map(|(i, row)| {
                (
                    row,
                    if i == last {
                        if child.state == ToolState::Running && child.category == "command" {
                            Some(ToolDetail::Live(index))
                        } else {
                            child.detail.map(ToolDetail::Stored)
                        }
                    } else {
                        None
                    },
                )
            }));
        }
        rows
    }

    /// Tool status is steady; only the separate activity row blinks.
    fn animates(&self) -> bool {
        false
    }

    fn render(&self, theme: &Theme, width: usize, _blink_on: bool) -> Vec<String> {
        match self.kind {
            // `𝑒 {VERSION} · Run /help for commands` — name bold ink, the rest
            // in the reference's dim (247 on light, one step lighter than the
            // statusline gray).
            Kind::Banner => vec![format!(
                "{}{}",
                bold(&theme.fg("userMessageText", "𝑒")),
                theme.fg("dim", &format!(" {} · Run /help for commands", self.text))
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
                // Reasoning needs a stable streaming renderer. A general
                // markdown parser exposes an opening `**` until the closing
                // marker arrives, then reparses and shifts the live frame.
                // Treat strong markers as styling even while unmatched, and
                // leave every other character inert. The whole row remains in
                // thinkingText, distinct from white assistant TextDelta rows.
                let styled = reasoning_style(text);
                wrap_styled(&styled, width.saturating_sub(2).max(8))
                    .into_iter()
                    .map(|line| {
                        if line.is_empty() {
                            line
                        } else {
                            theme.fg("thinkingText", &format!("  {line}"))
                        }
                    })
                    .collect()
            }
            Kind::Tool => {
                // The reference shape: a finished row is just the row — no
                // "(done)". Failure turns the marker to the error token and
                // adds a `│ <outcome>` continuation beneath. A finished row's
                // `●` wears the system-notice text gray; a cancelled row
                // brightens its summary and asks what to do differently.
                let marker = if self.cancelled {
                    theme.fg("warning", "■")
                } else if !self.done {
                    dim("●")
                } else if self.is_error {
                    theme.fg("error", "●")
                } else {
                    theme.fg("customMessageText", "●")
                };
                let mut rows = vec![if self.cancelled {
                    let plain = match &self.detail {
                        Some(target) if !target.is_empty() => {
                            format!("{} {}", self.text, target)
                        }
                        _ => self.text.clone(),
                    };
                    format!(
                        "{marker} {} · What can e do differently?",
                        theme.fg("userMessageText", &plain)
                    )
                } else {
                    match &self.detail {
                        Some(target) if !target.is_empty() => {
                            format!("{marker} {} {}", self.text, theme.fg("muted", target))
                        }
                        _ => format!("{marker} {}", self.text),
                    }
                }];
                if self.done {
                    // The reference's command-output shape: the first lines
                    // as `│` rows, an exit line ("│ exit code 7") when the
                    // command failed, and an elision row for the rest.
                    for line in &self.preview {
                        rows.push(theme.fg("dim", &format!("│ {line}")));
                    }
                    if self.is_error {
                        if let Some(result) = &self.result {
                            let shown =
                                clip_plain(&display_outcome(result), width.saturating_sub(2));
                            rows.push(theme.fg("dim", &format!("│ {shown}")));
                        }
                    }
                    if self.more > 0 {
                        rows.push(theme.fg("dim", &elision_row(self.more, width)));
                    }
                }
                rows
            }
            Kind::ToolGroup => {
                // The tool family runs flush left at the user rail's column.
                // Markers, tallies, branches, and labels stay muted. Diff
                // counts retain their added/removed colors.
                let marker = theme.fg("muted", "●");
                let header = clip_plain(&self.text, width.saturating_sub(2));
                let mut rows = vec![format!("{marker} {}", theme.fg("muted", &header))];
                if self.tool_children.is_empty() {
                    for (i, child) in self.children.iter().enumerate() {
                        let connector = if i + 1 == self.children.len() {
                            "└"
                        } else {
                            "├"
                        };
                        rows.push(format!(
                            "{} {}",
                            theme.fg("muted", connector),
                            theme.fg("muted", child)
                        ));
                    }
                    return rows;
                }
                let visible: Vec<_> = self
                    .tool_children
                    .iter()
                    .filter(|child| child.state != ToolState::Pending || self.done)
                    .collect();
                for (index, child) in visible.iter().enumerate() {
                    if child.state == ToolState::Pending {
                        // Mid-run, a pending call has no row yet. In a sealed
                        // group the call is on record and its result never
                        // came — say so, the reference's own fallback line.
                        if !self.done {
                            continue;
                        }
                        let last = index + 1 == visible.len();
                        let connector = if last { "└" } else { "├" };
                        rows.extend(tree_rows(
                            theme,
                            width,
                            connector,
                            &theme.fg("muted", "Tool completion was not reported"),
                        ));
                        continue;
                    }
                    let last = index + 1 == visible.len();
                    let live_command =
                        child.state == ToolState::Running && child.category == "command";
                    let connector = if last && !live_command { "└" } else { "├" };
                    rows.extend(child_rows(theme, width, child, connector));
                    if live_command {
                        append_tool_preview(
                            &mut rows,
                            child,
                            theme,
                            width,
                            self.live_preview_rows,
                            last,
                        );
                    }
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
            // The reference notice grammar: `● Topic: body` — the marker and
            // label in the tone's style (red for an error), the body in the
            // system-notice text gray, continuations indented two columns.
            Kind::Error => notice_rows(theme, "error", false, "Error", &self.text, width),
            Kind::System => notice_rows(
                theme,
                "customMessageLabel",
                true,
                "System",
                &self.text,
                width,
            ),
        }
    }
}

/// Render `**strong**` reasoning without ever exposing a partial marker.
/// An unmatched opener styles the text received so far; when its closer
/// arrives the visible cells do not change, so streaming cannot reflow merely
/// because markdown became complete.
fn reasoning_style(text: &str) -> String {
    let mut out = String::new();
    let mut strong = false;
    for (index, part) in text.split("**").enumerate() {
        if index > 0 {
            strong = !strong;
        }
        if strong {
            out.push_str(&bold(part));
        } else {
            out.push_str(part);
        }
    }
    out
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

/// Visible cell width of plain text (no SGR).
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// The width-1 prefix of `text` by display cells.
fn prefix_by_width(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out
}

/// Clip by display cells the reference way: fits → unchanged; one cell →
/// a bare ellipsis; otherwise a width-1 prefix plus `…`.
fn clip_plain(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "…".into();
    }
    let mut clipped = prefix_by_width(text, width - 1);
    clipped.push('…');
    clipped
}

/// The command-output elision row, degrading with the frame the reference
/// way: full wording, then `(ctrl o)`, then the bare count, then a hard
/// prefix clip.
fn elision_row(more: usize, width: usize) -> String {
    let noun = if more == 1 { "line" } else { "lines" };
    let candidates = [
        format!("│ … {more} {noun} more (ctrl o to view)"),
        format!("│ … {more} more (ctrl o)"),
        format!("│ … {more} more"),
    ];
    for candidate in &candidates {
        if display_width(candidate) <= width {
            return candidate.clone();
        }
    }
    prefix_by_width(&candidates[candidates.len() - 1], width)
}

/// `● Topic: body` in the reference notice grammar: the marker and label in
/// the tone's token (bold for the information tone), the body in the
/// system-notice text gray, continuations indented two columns.
fn notice_rows(
    theme: &Theme,
    tone: &str,
    bold_label: bool,
    topic: &str,
    body: &str,
    width: usize,
) -> Vec<String> {
    let plain = format!("● {topic}:");
    let label = if bold_label {
        bold(&theme.fg(tone, &plain))
    } else {
        theme.fg(tone, &plain)
    };
    let label_width = display_width(&plain);
    if width <= label_width + 1 {
        return wrap_styled(
            &format!("{label} {}", theme.fg("customMessageText", body)),
            width.max(1),
        );
    }
    // Reserve the first row's label while letting later rows use the full
    // hanging-indent width. Otherwise the painter clips the first row's tail.
    let padding = " ".repeat(label_width.saturating_sub(1));
    let mut rows = Vec::new();
    for line in wrap_styled(&format!("{padding}{body}"), width.saturating_sub(2)) {
        if rows.is_empty() {
            let first = line.get(padding.len()..).unwrap_or_default();
            rows.push(format!("{label} {}", theme.fg("customMessageText", first)));
        } else {
            rows.push(format!("  {}", theme.fg("customMessageText", &line)));
        }
    }
    if rows.is_empty() {
        rows.push(label);
    }
    rows
}

/// The reference's edit/write stat suffix: ` +N / -M` with the diff-marker
/// hue on each count and a dim slash; one-sided edits drop the slash, and a
/// summary that isn't a `+N -M` pair rides muted unchanged.
fn diff_stat_suffix(theme: &Theme, result: &str) -> String {
    let mut adds = None;
    let mut dels = None;
    for part in result.split_whitespace() {
        if let Some(n) = part.strip_prefix('+').and_then(|v| v.parse::<usize>().ok()) {
            adds = Some(n);
        } else if let Some(n) = part.strip_prefix('-').and_then(|v| v.parse::<usize>().ok()) {
            dels = Some(n);
        }
    }
    let added = |n| theme.fg(Theme::diff_marker_token(true), &format!("+{n}"));
    let removed = |n| theme.fg(Theme::diff_marker_token(false), &format!("-{n}"));
    let suffix = match (adds, dels) {
        (Some(a), Some(d)) if a > 0 && d > 0 => {
            format!("{} {} {}", added(a), theme.fg("dim", "/"), removed(d))
        }
        (Some(a), _) if a > 0 => added(a),
        (_, Some(d)) if d > 0 => removed(d),
        (Some(_), Some(_)) => return String::new(),
        _ => theme.fg("muted", result),
    };
    format!(" {suffix}")
}

/// Wrap one tree label without repeating its branch on continuation rows.
/// Styling and display-cell widths are shared by action rows and previews.
pub(crate) fn tree_rows(theme: &Theme, width: usize, connector: &str, label: &str) -> Vec<String> {
    let gutter = if width >= 3 { 2 } else { 0 };
    wrap_styled(label, width.saturating_sub(gutter).max(1))
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let marker = if i == 0 { connector } else { "│" };
            let prefix = if gutter == 2 {
                format!("{marker} ")
            } else {
                String::new()
            };
            format!("{}{line}", theme.fg("muted", &prefix))
        })
        .collect()
}

/// One child's wrapped action, shared by the transcript and review screen.
fn child_rows(theme: &Theme, width: usize, child: &ToolChild, connector: &str) -> Vec<String> {
    // Labels and connectors stay neutral; diff counts carry their own hues.
    // State belongs in the verb and result, not in selected-looking branches.
    let action = match child.state {
        ToolState::Pending | ToolState::Running => child.running.as_str(),
        ToolState::Completed => child.completed.as_str(),
        ToolState::Failed if child.category == "command" => child.completed.as_str(),
        ToolState::Failed => "Failed",
        ToolState::TimedOut => "Timed out",
        ToolState::Blocked => "Denied",
        ToolState::Cancelled => "Cancelled",
    };
    let suffix = if child.state == ToolState::Completed
        && matches!(child.category.as_str(), "edit" | "write")
    {
        child
            .result
            .as_deref()
            .map(|result| diff_stat_suffix(theme, result))
            .unwrap_or_default()
    } else {
        String::new()
    };
    // The reference's failed rows name the reason: `Failed path: preflight
    // failed`. A generic "error" summary adds nothing and stays off the row.
    let target_plain = match (&child.state, child.result.as_deref()) {
        (ToolState::Failed, Some(reason))
            if child.category != "command" && !reason.is_empty() && reason != "error" =>
        {
            format!("{}: {reason}", child.target)
        }
        _ => child.target.clone(),
    };
    let label = if target_plain.is_empty() {
        action.to_string()
    } else {
        format!("{action} {target_plain}")
    };
    tree_rows(
        theme,
        width,
        connector,
        &format!("{}{suffix}", theme.fg("muted", &label)),
    )
}

/// Show the live tail in the tool's own branch, closing with an output hint.
/// The budget counts wrapped display rows, not source lines.
fn append_tool_preview(
    rows: &mut Vec<String>,
    child: &ToolChild,
    theme: &Theme,
    width: usize,
    budget: usize,
    last: bool,
) {
    let output: Vec<String> = child
        .output
        .lines()
        .filter(|line| !line.starts_with("… [killed:") && *line != "… [cancelled]")
        .flat_map(|line| tree_rows(theme, width, "│", &theme.fg("dim", line)))
        .collect();
    let more = output.len().saturating_sub(budget);
    rows.extend(output.into_iter().skip(more));
    let hint = if child.output_truncated {
        "earlier output omitted · ctrl+o to view".into()
    } else if more > 0 {
        format!(
            "{more} more {} · ctrl+o to view",
            if more == 1 { "row" } else { "rows" }
        )
    } else {
        "ctrl+o to view".into()
    };
    rows.extend(tree_rows(
        theme,
        width,
        if last { "└" } else { "│" },
        &theme.fg("muted", &hint),
    ));
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
        "Searched" => "read",
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
    /// Everything block state can contribute to a rendering, as one
    /// number: the block count and each block's touch generation. The
    /// review screen folds this into its cache key.
    pub fn fingerprint(&self) -> u64 {
        self.blocks
            .iter()
            .fold(self.blocks.len() as u64, |acc, block| {
                acc.rotate_left(1) ^ block.generation
            })
    }

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

    /// The tree a new batch should continue: the last block, walking back
    /// over collapsed thinking summaries, when it is a live tool group.
    /// Expanded thinking, replies, user messages, notices, and errors
    /// separate trees.
    fn open_tool_group(&self) -> Option<usize> {
        for (index, block) in self.blocks.iter().enumerate().rev() {
            match block.kind {
                Kind::Thinking if block.done => continue,
                Kind::ToolGroup if !block.done => return Some(index),
                _ => return None,
            }
        }
        None
    }

    /// Continue the open tree when no reply or expanded thinking separates
    /// batches. Only legacy collapsed summaries may be absorbed; live and
    /// completed reasoning keep their own blocks and start a new tree.
    pub fn extend_tool_group(&mut self, children: Vec<ToolChild>) -> usize {
        if let Some(idx) = self.open_tool_group() {
            // Everything after the group is absorbed thinking: drop it so
            // the continued rows sit directly under their tree.
            self.blocks.truncate(idx + 1);
        }
        let idx = self
            .open_tool_group()
            .unwrap_or_else(|| self.push(Block::tool_group(Vec::new())));
        let block = &mut self.blocks[idx];
        block.tool_children.extend(children);
        block.refresh_tool_header();
        block.touch();
        idx
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
    /// Notice wrapping preserves the first character, including multibyte text.
    #[test]
    fn notices_keep_the_first_body_character() {
        let theme = crate::tui::theme::load_bundled(false).unwrap();
        for body in ["First character stays.", "界 first character stays."] {
            let rows = super::notice_rows(&theme, "dim", false, "Notice", body, 80);
            assert_eq!(
                crate::core::tools::strip_ansi(&rows.join("\n")),
                format!("● Notice: {body}")
            );
        }
    }

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
        let cached = block.cache.as_ref().unwrap().lines.as_ptr();
        block.lines(&theme, 80, false);
        assert_eq!(
            block.cache.as_ref().unwrap().lines.as_ptr(),
            cached,
            "a blink flip re-rendered a block with no running tool"
        );
    }

    #[test]
    fn streaming_render_work_has_a_fixed_source_byte_budget() {
        assert_eq!(
            stream_render_interval(1),
            std::time::Duration::from_millis(33)
        );
        assert_eq!(
            stream_render_interval(4 * 1024 * 1024),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            stream_render_interval(8 * 1024 * 1024),
            std::time::Duration::from_secs(2)
        );
    }

    #[test]
    fn a_stream_keeps_its_cache_until_the_segment_is_finished() {
        let theme = theme();
        let mut block = Block::new(Kind::Assistant, "");
        block.append_streaming("first");
        block.lines(&theme, 80, true);
        let rendered_generation = block.cache.as_ref().unwrap().generation;

        block.append_streaming(" second");
        assert!(block.cache.is_some(), "a delta discarded the paced cache");
        assert_ne!(rendered_generation, block.generation);

        block.finish_streaming();
        assert!(block.cache.is_none(), "the final render was not forced");
        assert!(block.lines(&theme, 80, true).join("\n").contains("second"));
    }

    /// Running calls remain attached and blink ticks do not invalidate their cache.
    #[test]
    fn running_tool_paints_as_an_attached_steady_row() {
        let theme = theme();
        let mut block = Block::tool_group(vec![ToolChild::pending(
            1,
            "command".into(),
            "Running".into(),
            "Ran".into(),
            "true".into(),
        )]);
        block.start_tool(1);
        let tree = block.lines_for_test(&theme, 80);
        assert_eq!(tree.len(), 3);
        assert!(tree[1].contains("Running true"));
        assert!(tree[1].contains('├'));
        assert!(tree[2].contains('└'));
        // The block's own cache is phase-stable.
        block.lines(&theme, 80, true);
        let cached = block.cache.as_ref().unwrap().lines.as_ptr();
        block.lines(&theme, 80, false);
        assert_eq!(block.cache.as_ref().unwrap().lines.as_ptr(), cached);

        block.finish_tool(1, ToolOutcome::Completed, "done".into(), "");
        let done = block.lines_for_test(&theme, 80);
        assert_eq!(done.len(), 2);
        assert!(done[1].contains("Ran true"));
    }

    #[test]
    fn tool_tree_keeps_labels_neutral_and_colors_diff_counts() {
        let theme = Theme::from_json(
            r#"{"vars":{"m":240,"a":250,"e":196,"d":34},"colors":{"muted":"m","dim":"m","accent":"a","error":"e","toolDiffAddedMarker":"d","toolDiffRemovedMarker":"e","toolDiffAddedMarkerFallback":"d","toolDiffRemovedMarkerFallback":"e"}}"#,
        )
        .unwrap();
        let mut block = Block::tool_group(vec![
            ToolChild::pending(
                1,
                "edit".into(),
                "Editing".into(),
                "Edited".into(),
                "a.rs".into(),
            ),
            ToolChild::pending(
                2,
                "read".into(),
                "Reading".into(),
                "Read".into(),
                "missing.rs".into(),
            ),
        ]);
        block.start_tool(1);
        assert!(block
            .lines_for_test(&theme, 80)
            .iter()
            .all(|row| !row.contains(theme.fg_prefix("accent"))));
        block.finish_tool(1, ToolOutcome::Completed, "+2 -1".into(), "");
        block.finish_tool(2, ToolOutcome::Failed, "not found".into(), "");
        block.seal();
        let rows = block.lines_for_test(&theme, 80);
        assert!(rows[1].contains(&theme.fg(Theme::diff_marker_token(true), "+2")));
        assert!(rows[1].contains(&theme.fg(Theme::diff_marker_token(false), "-1")));
        assert!(
            !rows[2].contains(theme.fg_prefix("error")),
            "failure labels stay neutral"
        );
        assert!(
            rows.iter()
                .all(|row| row.contains(theme.fg_prefix("muted"))),
            "every tool row keeps its neutral connector"
        );
    }

    #[test]
    fn diff_suffix_colors_each_side_and_omits_zero_counts() {
        let theme = crate::tui::theme::load_bundled(false).unwrap();
        let green = theme.fg(Theme::diff_marker_token(true), "+12");
        let red = theme.fg(Theme::diff_marker_token(false), "-3");
        assert_eq!(
            diff_stat_suffix(&theme, "+12 -3"),
            format!(" {green} {} {red}", theme.fg("dim", "/"))
        );
        assert_eq!(diff_stat_suffix(&theme, "+12 -0"), format!(" {green}"));
        assert_eq!(diff_stat_suffix(&theme, "+0 -3"), format!(" {red}"));
        assert_eq!(diff_stat_suffix(&theme, "+0 -0"), "");
        assert_eq!(
            diff_stat_suffix(&theme, "done"),
            format!(" {}", theme.fg("muted", "done"))
        );
    }

    /// Thinking renders its full streamed text in thinkingText, one wrapped
    /// row per line, with no `·` marker and no collapse to a summary — ending
    /// a burst leaves the thought expanded where it sits.
    #[test]
    fn thinking_renders_expanded_in_thinking_text_without_collapse() {
        let theme = Theme::from_json(
            r#"{"vars":{"a":250,"b":240},"colors":{"thinkingText":"a","dim":"b"}}"#,
        )
        .unwrap();
        let mut block = Block::new(Kind::Thinking, "**let me look");
        let partial = block.lines_for_test(&theme, 40);
        assert!(
            partial.iter().all(|row| !row.contains("**")),
            "an unmatched streaming marker must never become visible"
        );
        block.text = "**let me look at this**\nstep two".into();
        block.touch();
        let rows = block.lines_for_test(&theme, 40);
        assert!(
            rows.iter().all(|row| !row.contains("**")),
            "complete thinking markers are rendered, not shown literally"
        );
        assert!(rows.len() >= 2, "every thought line renders");
        for row in &rows {
            assert!(!row.contains("·"), "thinking carries no dot marker");
            assert!(
                row.contains(theme.fg_prefix("thinkingText")),
                "thinking wears thinkingText, never the dim summary color"
            );
        }
        // A finished burst is never rewritten to a one-line summary: growing
        // the text keeps growing the rendered rows.
        block.text = "let me look at this\nstep two\nand a third".into();
        block.touch();
        assert!(
            block.lines_for_test(&theme, 40).len() >= 3,
            "the thought stays expanded, not collapsed to one row"
        );
    }

    #[test]
    fn final_command_output_replaces_a_partial_streaming_preview() {
        let mut block = Block::tool_group(vec![ToolChild::pending(
            1,
            "command".into(),
            "Running".into(),
            "Ran".into(),
            "command".into(),
        )]);
        block.tool_children[0].output = "first chunk\n".into();
        block.finish_tool(1, ToolOutcome::Completed, "done".into(), "retained tail\n");
        assert_eq!(block.tool_children[0].output, "retained tail\n");
    }

    #[test]
    fn failed_tool_summary_is_sanitized_before_rendering() {
        let mut block = Block::tool_group(vec![ToolChild::pending(
            1,
            "edit".into(),
            "Editing".into(),
            "Edited".into(),
            "file.rs".into(),
        )]);
        block.finish_tool(
            1,
            ToolOutcome::Failed,
            "bad \x1b[2Jreason\x1b]52;c;x\x07".into(),
            "",
        );
        assert_eq!(block.tool_children[0].result.as_deref(), Some("bad reason"));
        for row in block.lines_for_test(&theme(), 80) {
            assert!(
                !row.contains("\x1b[2J"),
                "an erase sequence leaked: {row:?}"
            );
            assert!(!row.contains("\x1b]"), "an OSC sequence leaked: {row:?}");
        }
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
