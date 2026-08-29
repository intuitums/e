//! The interactive frame: App state, key handling, and the paint loop.
//!
//! The binary (`main.rs`) owns CLI dispatch (`auth`, `ask`, `docs`, …) and
//! hands off here once a session should open.

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, EventStream, KeyCode,
    KeyEvent, KeyModifiers, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};
use futures::StreamExt;
use std::io::Write;
use std::time::{Duration, Instant};

use crate::core::agent::{Agent, AgentOptions, SessionEvent};
use crate::core::output::{format_duration, format_tokens};
use crate::core::providers::catalog::{self as model, Model};
use crate::tui::authpanel::{self, AuthStage};
use crate::tui::background::stdout_is_tty;
use crate::tui::composer::{Editor, EditorResult, Key};
use crate::tui::menu::{
    Menu, MenuItem, MenuKind, HINT_MODELS, HINT_SCOPED, HINT_SESSIONS, HINT_SKILLS, HINT_USE,
};
use crate::tui::screen::Painter;
use crate::tui::statusline::{
    format_elapsed, statusline, RecoveredStatus, RetryStatus, StatusData, Turn, TurnPhase,
    RECOVERED_VISIBLE_MS,
};
use crate::tui::theme::Theme;
use crate::tui::transcript::{Block, Kind, Transcript};
use crate::tui::trustpanel::{self, TrustStage};

mod events;
mod login;
mod menus;

/// Per-turn frontend bookkeeping; the engine state lives in the Agent.
struct ActiveTurn {
    /// The current assistant text block, if one is streaming.
    block: Option<usize>,
    text: String,
    /// The live thinking block for the current burst, if reasoning has
    /// streamed. Earlier bursts from this turn collapsed where they ended —
    /// this index is only the open segment.
    thinking_block: Option<usize>,
    thinking: String,
    /// When the open burst started, for the collapsed row's duration.
    thinking_started: Option<Instant>,
    turn: Turn,
    started: Instant,
    error: Option<String>,
    /// tool id → stable group block, so lifecycle events update in place.
    tool_blocks: std::collections::HashMap<u64, usize>,
    /// Batch members not yet terminal, including serially pending calls.
    pending_tools: usize,
    /// Set when the turn was stopped because the device slept past the
    /// resume window: the stop line is already in the transcript, so the
    /// cancelled row is suppressed at TurnEnd.
    sleep_stopped: bool,
    /// Accumulated provider-billed estimate for this turn, when the model
    /// declares rates. Unlike the context gauge, every request step counts.
    cost_usd: Option<f64>,
}

/// The ctrl+o full-detail screen: one stored output at a time, scrollable,
/// ←/→ switching between outputs — the reference surface, e-sized.
/// The ctrl+o review screen: the whole transcript with tool details
/// spliced in, at one of the reference's two depths — Review folds each
/// detail to three lines behind a `→ to expand` hint, Full shows all.
#[derive(Clone, Copy)]
struct Viewer {
    full: bool,
    scroll: usize,
}

/// The queued-prompt review's working state: a snapshot of the paused
/// queue (oldest first), which entry the composer holds, and whether a
/// draft is showing at all (↓ past the newest hides it while the queue
/// stays paused).
struct QueueReview {
    entries: Vec<String>,
    dirty: Vec<bool>,
    selected: usize,
    visible: bool,
}

/// Asynchronous work landing back in the frame loop.
enum AppJob {
    /// A line for the transcript (login progress, extension notify…).
    Notice(String),
    /// A prompt an extension command asked to submit as the user.
    Prompt { text: String, epoch: u64 },
    /// An input hook's verdict on a submitted line: consume/replace/notice.
    /// `images` rides along only for the initial launch prompt (`-i`); a
    /// hook never sees or handles them, but they still attach once the
    /// verdict lands on whatever text is actually submitted.
    InputVerdict {
        sequence: u64,
        text: String,
        images: Option<Vec<crate::core::providers::ImageInput>>,
        verdict: crate::core::api::InputVerdict,
    },
    /// An extension named the session (command result). Tagged with the
    /// session epoch the command started in.
    Rename { name: String, epoch: u64 },
    /// A finished /compact: the summary and the recent messages kept verbatim.
    Compacted {
        summary: String,
        kept: Vec<crate::core::providers::ChatMessage>,
        epoch: u64,
    },
    /// A /compact that didn't produce a summary.
    CompactFailed { message: String, epoch: u64 },
    /// A finished `!` shell command: what ran and what it printed. Tagged
    /// with the session epoch it started in.
    Shell {
        cmd: String,
        output: crate::core::tools::ToolOutput,
        epoch: u64,
    },
    /// A /reload finished: the restarted extension host.
    Reloaded(std::sync::Arc<crate::core::api::ExtensionHost>),
    /// The background updater installed a new version.
    Updated(String),
    /// A provider model-list refresh finished; rebuild an open picker.
    CatalogRefreshed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputRoute {
    ApiKey,
    Hook,
    Direct,
}

fn input_route(awaiting_api_key: bool, has_input_hook: bool) -> InputRoute {
    match (awaiting_api_key, has_input_hook) {
        (true, _) => InputRoute::ApiKey,
        (false, true) => InputRoute::Hook,
        (false, false) => InputRoute::Direct,
    }
}

/// Input hooks run concurrently so a slow extension does not block the frame,
/// but their verdicts must be applied in submission order. Otherwise a fast
/// second line can overtake a slow first one and reverse the conversation.
type InputVerdictItem = (
    String,
    Option<Vec<crate::core::providers::ImageInput>>,
    crate::core::api::InputVerdict,
);

#[derive(Default)]
struct PendingInputVerdicts {
    next_sequence: u64,
    next_to_apply: u64,
    ready: std::collections::BTreeMap<u64, InputVerdictItem>,
}

impl PendingInputVerdicts {
    fn reserve(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        sequence
    }

    fn complete(
        &mut self,
        sequence: u64,
        text: String,
        images: Option<Vec<crate::core::providers::ImageInput>>,
        verdict: crate::core::api::InputVerdict,
    ) -> Vec<InputVerdictItem> {
        self.ready.insert(sequence, (text, images, verdict));
        let mut ordered = Vec::new();
        while let Some(item) = self.ready.remove(&self.next_to_apply) {
            ordered.push(item);
            self.next_to_apply += 1;
        }
        ordered
    }
}

struct ActiveLogin {
    flow_id: u64,
    cancellation: crate::core::auth::login::LoginCancellation,
    task: tokio::task::JoinHandle<()>,
    wait_for_callback: bool,
}

impl Drop for ActiveLogin {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
        if self.wait_for_callback {
            crate::core::auth::login::wait_for_callback_release();
        }
    }
}

struct App {
    theme: Theme,
    /// The composer's chord overrides from `~/.e/keybindings.json`. Reread
    /// alongside the theme — startup, /settings close, /reload — never
    /// mid-keystroke.
    keymap: crate::core::config::keybindings::Keymap,
    transcript: Transcript,
    editor: Editor,
    agent: Agent,
    active: Option<ActiveTurn>,
    overlay: Option<String>,
    armed_at: Option<Instant>,
    should_quit: bool,
    /// Prompt-side tokens of the latest request ≈ current context size.
    context_tokens: u64,
    /// A provider awaiting a pasted API key; the next submit is the secret.
    pending_key: Option<String>,
    /// The open picker, if any — commands, files, models.
    menu: Option<Menu>,
    /// The sign-in panel, when /login is active.
    auth: Option<AuthStage>,
    /// The settings panel, when /settings is active.
    settings: Option<crate::tui::settingspanel::SettingsPanel>,
    /// Whether streamed thinking is drawn (the `show_thinking` setting,
    /// default off). Gating only the drawing — the ↓ token estimate always
    /// counts reasoning.
    show_thinking: bool,
    /// Background job narration (login flows) into the transcript.
    jobs: tokio::sync::mpsc::Sender<String>,
    /// How a login flow ended; control flow reads this, never the notices.
    logins: tokio::sync::mpsc::Sender<crate::core::auth::login::Outcome>,
    /// The owned OAuth task; dropping it cancels polling and callback waits.
    login_task: Option<ActiveLogin>,
    /// Monotonic identity used to ignore a canceled flow's queued outcome.
    login_sequence: u64,
    /// Extension host; commands and prompts come back on `results`.
    host: std::sync::Arc<crate::core::api::ExtensionHost>,
    results: tokio::sync::mpsc::Sender<AppJob>,
    /// Completed input-hook calls waiting for earlier submissions to finish.
    input_verdicts: PendingInputVerdicts,
    /// A /compact summary is being generated; cleared when it lands or fails.
    compacting: bool,
    /// Identity of the in-flight compaction. Session changes and a later
    /// compaction invalidate older results before they can replace history.
    compaction_epoch: u64,
    /// /compact was asked for mid-turn; runs when the turn ends (the
    /// reference behavior — compaction never touches a running turn).
    compact_requested: bool,
    /// Messages typed while compacting; submitted once the swap lands.
    held_prompts: Vec<String>,
    /// First visit to this directory: the trust question, until answered.
    trust: Option<TrustStage>,
    /// A command-line prompt held until the first-visit trust choice is
    /// persisted, so its system prompt reflects that choice.
    pending_initial: Option<String>,
    pending_initial_images: Vec<crate::core::providers::ImageInput>,
    /// Transcript index of the running `!` block, updated on completion.
    shell_block: Option<usize>,
    /// A /reload is restarting the extension host; prompts are held.
    reloading: bool,
    /// Transcript index of the reload notice, replaced when reload finishes.
    reload_block: Option<usize>,
    /// Full tool outputs for the ctrl+o review screen: (id, title, content),
    /// newest last, capped; ids link tool children to their details.
    outputs: Vec<(u64, String, String)>,
    output_seq: u64,
    /// The ctrl+o full-detail viewer, when open.
    viewer: Option<Viewer>,
    /// The review screen's projected rows, cached between frames: the
    /// projection only rebuilds when the transcript or the output store
    /// changed (the cache's fingerprint), or the width or depth moved —
    /// not on every 33ms paint.
    viewer_cache: Option<(u64, usize, bool, Vec<String>)>,
    /// The ask tool's open question, and any batch-mates queued behind it.
    question: Option<crate::tui::questionpanel::Question>,
    question_queue: std::collections::VecDeque<crate::tui::questionpanel::Question>,
    /// The queued-prompt review: ↑ on an empty composer while prompts wait
    /// pauses the queue and loads the newest into the composer for editing.
    queue_review: Option<QueueReview>,
    /// Bumped whenever session identity changes (/new, resume). Async work
    /// launched in one epoch may not mutate a later one: a late extension
    /// command or shell result carries the epoch it started in and is
    /// dropped on mismatch.
    session_epoch: u64,
    /// A new version is installed on disk; /reload switches to it.
    update_installed: Option<String>,
    /// Exit the loop and exec the (updated) binary with -c.
    relaunch: bool,
    /// OSC-11 background detection, probed once at startup before the
    /// event stream owns stdin. Re-probing mid-session would block the
    /// loop and swallow keystrokes, so a changed terminal background
    /// applies on restart.
    light_background: bool,
    /// Cached statusline inputs. Deriving them reads `~/.e/auth.json` and
    /// `~/.e/settings.json`; doing that per frame stalls streaming, so
    /// they refresh only via `refresh_status_cache`.
    signed_in: bool,
    status_effort: Option<String>,
}

impl App {
    /// The review screen's cached body: the projected transcript rows,
    /// rebuilt only when the cache's fingerprint no longer matches. The
    /// fingerprint covers everything the projection reads — block state
    /// (each block's generation, bumped on every touch), the block count,
    /// and the output store's seq (new details and eviction).
    fn viewer_rows(&mut self, width: usize, full: bool) -> &[String] {
        let fingerprint = self.viewer_fingerprint();
        let current = match &self.viewer_cache {
            Some((cached_fp, cached_width, cached_full, _)) => {
                *cached_fp == fingerprint && *cached_width == width && *cached_full == full
            }
            None => false,
        };
        if !current {
            let rows = self.project_rows(width, full);
            self.viewer_cache = Some((fingerprint, width, full, rows));
        }
        &self.viewer_cache.as_ref().unwrap().3
    }

    /// Everything the review projection reads, as one number.
    fn viewer_fingerprint(&self) -> u64 {
        self.transcript.fingerprint() ^ self.output_seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    /// The projection itself, pure over the current transcript: the whole
    /// transcript with each child row's stored detail railed beneath it —
    /// folded to three lines behind the reference's `→ to expand` hint at
    /// Review depth, whole at Full. Non-tool blocks render through the
    /// per-block cache, so a rebuild pays only for blocks that changed.
    fn project_rows(&mut self, width: usize, full: bool) -> Vec<String> {
        const REVIEW_DETAIL_LINES: usize = 3;
        let mut rows: Vec<String> = Vec::new();
        for block in &mut self.transcript.blocks {
            let lines = block.review_lines(&self.theme, width);
            if lines.is_empty() {
                continue;
            }
            if !rows.is_empty() {
                rows.push(String::new());
            }
            for (row, detail) in lines {
                rows.push(crate::tui::markdown::clip_styled(&row, width));
                let Some(id) = detail else { continue };
                let Some(body) = Self::output_body(&self.outputs, id) else {
                    rows.push(self.theme.fg("dim", "│  Full saved result unavailable."));
                    continue;
                };
                let body_lines: Vec<&str> = body.lines().collect();
                let shown = if full {
                    body_lines.len()
                } else {
                    body_lines.len().min(REVIEW_DETAIL_LINES)
                };
                let mut clipped_any = false;
                for line in &body_lines[..shown] {
                    let railed = match Self::diff_row_color(&self.theme, line) {
                        Some(colored) => {
                            format!("{} {colored}", self.theme.fg("dim", "│"))
                        }
                        None => self.theme.fg("dim", &format!("│ {line}")),
                    };
                    if crate::tui::markdown::visible_width(&railed) > width {
                        clipped_any = true;
                    }
                    rows.push(crate::tui::markdown::clip_styled(&railed, width));
                }
                let hidden = body_lines.len() - shown;
                if hidden > 0 {
                    let noun = if hidden == 1 { "line" } else { "lines" };
                    rows.push(
                        self.theme
                            .fg("dim", &format!("│  {hidden} more {noun} · → to expand")),
                    );
                } else if !full && clipped_any {
                    rows.push(self.theme.fg("dim", "│  line clipped · → to expand"));
                }
            }
        }
        rows
    }

    /// The ctrl+o review screen: a scroll window over the projected
    /// transcript, the `┃ Review …` navigation row at the bottom — the
    /// reference's own wording per depth.
    fn viewer_frame(&mut self, width: usize, height: usize) -> Vec<String> {
        let Some(viewer) = self.viewer else {
            return Vec::new();
        };
        let body = self.viewer_rows(width, viewer.full);
        let window = height.saturating_sub(1).max(1);
        // The body can shrink under a deep scroll (the output store evicts;
        // details disappear) — clamp so the screen never renders blank.
        let scroll = viewer.scroll.min(body.len().saturating_sub(1));
        let mut rows: Vec<String> = body.iter().skip(scroll).take(window).cloned().collect();
        while rows.len() < window {
            rows.push(String::new());
        }
        let nav = if viewer.full {
            "Full detail · ←/→ switch · ctrl o close · PgUp/PgDn scroll · Esc close"
        } else {
            "Review · ←/→ switch · ctrl o close · PgUp/PgDn scroll · Esc close"
        };
        rows.push(format!(
            "{} {}",
            self.theme.fg("userMessageText", "┃"),
            self.theme.fg("muted", nav)
        ));
        // Persist the clamp once `body`'s borrow is done, so ↑/↓ arithmetic
        // starts from a scroll the body can actually show.
        if let Some(viewer) = self.viewer.as_mut() {
            viewer.scroll = scroll;
        }
        rows
    }

    /// Color a detail-viewer row shaped like a diff row: the number-and-sign
    /// column takes the diff-marker hue (`+` green, `-` red), context and
    /// `⋯` elision rows dim, anything else passes through untouched — the
    /// reference keeps the changed text itself neutral.
    fn diff_row_color(theme: &Theme, line: &str) -> Option<String> {
        if line.trim() == "⋯" && line.starts_with("      ") {
            return Some(theme.fg("dim", line));
        }
        let field = line.get(..5)?;
        let number = field.trim_start();
        if number.is_empty()
            || !number.bytes().all(|b| b.is_ascii_digit())
            || *field != format!("{number:>5}")
        {
            return None;
        }
        let rest = &line[5..];
        if rest.is_empty() || rest.starts_with("   ") {
            return Some(theme.fg("dim", line));
        }
        let added = if rest == " +" || rest.starts_with(" + ") {
            true
        } else if rest == " -" || rest.starts_with(" - ") {
            false
        } else {
            return None;
        };
        let token = crate::tui::theme::Theme::diff_marker_token(added);
        Some(format!("{}{}", theme.fg(token, &line[..7]), &line[7..]))
    }

    fn frame(&mut self, width: usize, height: usize) -> Vec<String> {
        let blink_on = self
            .active
            .as_ref()
            .map(|turn| (turn.started.elapsed().as_millis() / 500) % 2 == 0)
            .unwrap_or(true);
        let mut lines = self
            .transcript
            .render_animated(&self.theme, width, blink_on);
        // The transient running-tool row: the focused call leaves its tree
        // and paints directly below the transcript (no gap), its marker
        // steady; the activity row follows one blank further down.
        if self.active.is_some() {
            if let Some(group) = self
                .transcript
                .blocks
                .iter()
                .rev()
                .find(|b| b.kind == Kind::ToolGroup && !b.done)
            {
                lines.extend(group.overlay_rows(&self.theme, width));
            }
        }
        if let Some(s) = &self.active {
            if let Some(label) = s.turn.label(s.started.elapsed().as_secs()) {
                lines.push(String::new());
                if s.turn.recovered.is_some() {
                    // A brief, non-blinking confirmation — not an ongoing
                    // wait, so no dot animation.
                    lines.push(self.theme.fg("success", &format!("✓ {label}")));
                } else if s.turn.phase == TurnPhase::Retrying {
                    // Same blinking dot as Thinking — still an in-progress
                    // wait — toned as a warning so a struggling provider
                    // reads distinctly from ordinary thinking.
                    let dot = if blink_on { "•" } else { " " };
                    lines.push(self.theme.fg("warning", &format!("{dot} {label}")));
                } else if matches!(
                    s.turn.phase,
                    TurnPhase::Thinking | TurnPhase::Tool | TurnPhase::AssistantText
                ) {
                    // The activity dot runs on the same column as the user
                    // rail — flush left, no indent — and the row persists
                    // through every streaming phase: thinking, tool trees,
                    // and reply text alike. The dot keeps its accent
                    // presence-blink (shows and hides, no half-state); the
                    // label — verb, elapsed, and token tail — reads in one
                    // dim tone beside it.
                    let dot = if blink_on {
                        self.theme.fg("accent", "•")
                    } else {
                        " ".to_string()
                    };
                    lines.push(format!("{dot} {}", self.theme.fg("dim", &label)));
                } else {
                    lines.push(label);
                }
            }
        }
        let entering_key = matches!(self.auth, Some(AuthStage::ApiKey { .. }));
        if !entering_key {
            // The reference caps the composer at half the frame plus one
            // row; a longer draft scrolls behind the ┃↑ marker.
            let cap = (height / 2 + 1).max(3);
            let mut composer = self.editor.render(&self.theme, width, cap);
            // The queued banner band: the collapsed summary (ink-bright),
            // a paused hint when the review holds the queue, a gap row —
            // and with chrome above it the composer trades its leading
            // blank for its top divider, the reference's rule.
            let steering = self.agent.queued_count();
            let total = steering + self.held_prompts.len();
            if total > 0 && self.active.is_some() {
                let paused = self.queue_review.is_some();
                // Held prompts (compaction, a `!` command) can't be edited
                // into the composer — only queued steering prompts can — so
                // the affordance appears only when the edit target exists.
                let affordance = if paused || steering == 0 {
                    ""
                } else {
                    " · ↑ to edit"
                };
                let ordinary = total - steering;
                let label = if ordinary == 0 && steering == 1 {
                    format!("1 steering message{affordance}")
                } else if ordinary == 0 {
                    format!("{steering} steering messages{affordance}")
                } else if steering > 0 {
                    format!("{total} pending messages · {steering} steering{affordance}")
                } else if total == 1 {
                    format!("1 queued message{affordance}")
                } else {
                    format!("{total} queued messages{affordance}")
                };
                lines.push(self.theme.fg("userMessageText", &label));
                if let Some(review) = &self.queue_review {
                    let hint = if !review.visible {
                        "steering paused · enter to apply"
                    } else if self.editor.is_empty() {
                        "paused · delete again to remove queued prompt · enter to send unchanged"
                    } else {
                        "paused · enter to send"
                    };
                    lines.push(self.theme.fg("dim", hint));
                }
                lines.push(String::new());
                composer[0] = self.theme.fg("border", &"─".repeat(width));
            }
            lines.extend(composer);
        }
        if let Some(stage) = &self.trust {
            let dir = self.agent.cwd().to_string_lossy().into_owned();
            lines.extend(trustpanel::render(stage, &self.theme, width, &dir));
        } else if let Some(question) = &self.question {
            lines.extend(question.render(&self.theme, width));
        } else if let Some(stage) = &self.auth {
            lines.extend(authpanel::render(
                stage,
                &self.theme,
                width,
                self.editor.text().chars().count(),
            ));
        } else if let Some(panel) = &self.settings {
            lines.extend(panel.render(&self.theme, width));
        } else if let Some(menu) = &self.menu {
            lines.extend(menu.render(&self.theme, width));
        }
        let window = self.agent.model.context_window.max(1);
        // Nothing is signed in for the current model — it's a bootstrap
        // placeholder, not something the user chose, so don't show it.
        let data = StatusData {
            model: self.signed_in.then(|| self.agent.model_slug()),
            context_used: self.context_tokens,
            context_total: Some(window),
        };
        let question_hint = self.question.as_ref().map(|q| q.hint(width));
        let hint = question_hint.as_deref().or_else(|| {
            self.settings
                .as_ref()
                .map(|_| crate::tui::settingspanel::HINT)
                .or_else(|| self.menu.as_ref().map(|m| m.hint))
                .map(|h| crate::tui::menu::degrade_hint(h, width))
        });
        // A framed surface's bottom divider sits directly above the hint
        // row — the blank spacer belongs only to the bare-composer layout.
        let panel_open = self.trust.is_some()
            || self.question.is_some()
            || self.auth.is_some()
            || self.settings.is_some()
            || self.menu.is_some();
        lines.extend(statusline(
            &self.theme,
            &data,
            self.overlay.as_deref(),
            hint,
            panel_open,
            width,
        ));
        lines
    }

    /* ---------- pickers ---------- */

    /// ctrl+p / ctrl+shift+p: cycle through the scope (or all available
    /// models when no scope is set), persisting the switch. The statusline is
    /// the feedback — it shows the new model immediately.
    fn cycle_model(&mut self, forward: bool) {
        let pool = model::cycle_pool();
        if pool.len() <= 1 {
            let scoped = model::scope().map(|s| !s.is_empty()).unwrap_or(false);
            self.notice(
                if scoped {
                    "only one model in scope"
                } else {
                    "only one model available"
                }
                .into(),
            );
            return;
        }
        let current = self.agent.model_slug();
        let idx = pool
            .iter()
            .position(|m| model::slug(m) == current)
            .unwrap_or(0);
        let next = if forward {
            (idx + 1) % pool.len()
        } else {
            (idx + pool.len() - 1) % pool.len()
        };
        if let Err(error) = persist_model(&pool[next]) {
            self.notice(format!("could not save model choice: {error}"));
            return;
        }
        self.agent.model = pool[next].clone();
        self.refresh_status_cache();
    }

    /// Queued-prompt review keys, the reference's grammar: ↑ on an empty
    /// composer while prompts wait opens the newest for editing (queue
    /// draining pauses); ↑/↓ step older/newer, ↓ past the newest hides the
    /// draft; Enter commits edits back to the queue and resumes — an empty
    /// draft leaves its entry unchanged; Backspace on an emptied draft
    /// deletes the entry. Returns true when the key was consumed.
    fn queue_review_key(&mut self, code: KeyCode) -> bool {
        if self.trust.is_some()
            || self.question.is_some()
            || self.auth.is_some()
            || self.settings.is_some()
            || self.menu.is_some()
        {
            return false;
        }
        let Some(mut review) = self.queue_review.take() else {
            if code == KeyCode::Up && self.editor.is_empty() && self.active.is_some() {
                let entries = self.agent.pause_queue();
                let Some(selected) = entries.len().checked_sub(1) else {
                    self.agent.resume_queue();
                    return false;
                };
                let dirty = vec![false; entries.len()];
                self.editor.set_text(&entries[selected]);
                self.queue_review = Some(QueueReview {
                    entries,
                    dirty,
                    selected,
                    visible: true,
                });
                return true;
            }
            return false;
        };
        let stash = |review: &mut QueueReview, text: String| {
            if review.entries[review.selected] != text {
                review.entries[review.selected] = text;
                review.dirty[review.selected] = true;
            }
        };
        let consumed = match code {
            KeyCode::Up => {
                if review.visible {
                    stash(&mut review, self.editor.text());
                    if review.selected > 0 {
                        review.selected -= 1;
                        self.editor.set_text(&review.entries[review.selected]);
                    }
                    true
                } else if self.editor.is_empty() {
                    review.visible = true;
                    self.editor.set_text(&review.entries[review.selected]);
                    true
                } else {
                    false
                }
            }
            KeyCode::Down if review.visible => {
                stash(&mut review, self.editor.text());
                if review.selected + 1 < review.entries.len() {
                    review.selected += 1;
                    self.editor.set_text(&review.entries[review.selected]);
                } else {
                    self.editor.set_text("");
                    review.visible = false;
                }
                true
            }
            // Enter with the draft hidden and new text typed is a fresh
            // prompt: fall through so the ordinary submit takes it (and
            // closes the review).
            KeyCode::Enter if review.visible || self.editor.is_empty() => {
                // The visible draft commits only when it holds text — an
                // emptied draft sends its entry unchanged.
                if review.visible && !self.editor.is_empty() {
                    stash(&mut review, self.editor.text());
                }
                if review.dirty.iter().any(|d| *d) {
                    // Only edited entries rewrite: a trim drops an entry that
                    // emptied, and leaves untouched entries verbatim — a
                    // multi-line prompt's trailing newline is not the user's
                    // doing.
                    let entries: Vec<String> = review
                        .entries
                        .iter()
                        .zip(&review.dirty)
                        .filter_map(|(entry, dirty)| {
                            if !dirty {
                                Some(entry.clone())
                            } else {
                                let trimmed = entry.trim().to_string();
                                (!trimmed.is_empty()).then_some(trimmed)
                            }
                        })
                        .collect();
                    self.agent.set_queued(entries);
                }
                self.editor.set_text("");
                self.agent.resume_queue();
                return true;
            }
            KeyCode::Backspace if review.visible && self.editor.is_empty() => {
                review.entries.remove(review.selected);
                review.dirty.remove(review.selected);
                self.agent.set_queued(review.entries.clone());
                if review.entries.is_empty() {
                    self.agent.resume_queue();
                    return true;
                }
                review.selected = review.selected.min(review.entries.len() - 1);
                self.editor.set_text(&review.entries[review.selected]);
                true
            }
            _ => false,
        };
        self.queue_review = Some(review);
        consumed
    }

    /// Close the review without committing the visible draft. The draft is
    /// discarded with the review, and the queue resumes as it stands.
    fn close_queue_review(&mut self) {
        if self.queue_review.take().is_some() {
            self.editor.set_text("");
            self.agent.resume_queue();
        }
    }

    fn open_resume_menu(&mut self) {
        // Both checks: `active` covers a turn whose TurnStart has been seen,
        // `is_streaming` covers the gap between submit and that event.
        if self.active.is_some() || self.agent.is_streaming() {
            self.notice("a turn is running — press Esc to stop it, then /resume".into());
            return;
        }
        let cwd = crate::core::session::normalized_cwd(&self.agent.cwd());
        // Every workspace's sessions, with a Tab-cycled scope filter that
        // opens on the current workspace. Each row carries the reference's
        // dim right cluster: `workspace · age · N turns`. A workspace's own
        // directory name stands in for it when unique across the list; two
        // `proj` directories fall back to the full `~`-collapsed path so
        // the rows stay distinguishable.
        let listed = crate::core::session::list_all();
        let mut tail_counts = std::collections::HashMap::<String, usize>::new();
        for info in &listed {
            if let Some(tail) = info.cwd.file_name() {
                *tail_counts
                    .entry(tail.to_string_lossy().into_owned())
                    .or_default() += 1;
            }
        }
        let items: Vec<MenuItem> = listed
            .into_iter()
            .map(|info| {
                let mut item = MenuItem::new(
                    if info.title.is_empty() {
                        "(untitled)"
                    } else {
                        &info.title
                    },
                    "",
                    &info.path.to_string_lossy(),
                );
                let tail = info
                    .cwd
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let workspace = if tail_counts.get(&tail).copied().unwrap_or(0) > 1 {
                    collapse_home(&info.cwd)
                } else {
                    tail
                };
                let turns = format!(
                    "{} {}",
                    info.user_turns,
                    if info.user_turns == 1 {
                        "turn"
                    } else {
                        "turns"
                    }
                );
                item.meta = format!("{workspace} · {} · {turns}", ago(info.modified));
                // Tab index space: 0 = current workspace, 1 = the
                // "All workspaces" tab itself. Tagging other workspaces 1
                // works because `tab_admits` checks all_tab before item
                // tags — reordering these tabs breaks that silently.
                item.tab = Some(if crate::core::session::normalized_cwd(&info.cwd) == cwd {
                    0
                } else {
                    1
                });
                item
            })
            .collect();
        if items.is_empty() {
            self.notice("no saved sessions".into());
            return;
        }
        self.menu = Some(
            Menu::new(MenuKind::Sessions, "Sessions", HINT_SESSIONS, items).with_tabs(
                vec!["Current workspace".into(), "All workspaces".into()],
                Some(1),
                0,
                "",
            ),
        );
    }

    fn resume_recent(&mut self) {
        let cwd = self.agent.cwd();
        match crate::core::session::most_recent(&cwd) {
            Some(path) => self.resume_path(path),
            None => self.notice("no saved sessions for this workspace".into()),
        }
    }

    /// Rebuild the transcript from a linear message history: clear what's
    /// showing and replay `messages` as blocks, reconstructing tool-call
    /// groups from their recorded outcomes. Shared by /resume (the whole
    /// file) and /tree (the path from root to a rewind point) — both end up
    /// wanting exactly the same replay, just fed a different list. A resumed
    /// transcript carries no welcome banner — the reference reserves it for
    /// a fresh session.
    fn rebuild_transcript(&mut self, messages: &[crate::core::providers::ChatMessage]) {
        self.transcript.clear();
        self.outputs.clear();
        self.viewer = None;
        let mut restored_calls = std::collections::HashMap::<String, (usize, u64)>::new();
        let mut restored_id = 0u64;
        // Consecutive tool batches with no assistant voice between them were
        // one growing tree live — the replay keeps them one tree.
        let mut open_group: Option<usize> = None;
        for m in messages {
            match m.role.as_str() {
                "user" => {
                    open_group = None;
                    let mut content = m.content.clone();
                    if !m.images.is_empty() {
                        content.push_str(&format!(
                            "\n[attached {} image{}]",
                            m.images.len(),
                            if m.images.len() == 1 { "" } else { "s" }
                        ));
                    }
                    self.transcript.push(Block::new(Kind::User, content));
                }
                "assistant" => {
                    if !m.content.trim().is_empty() {
                        open_group = None;
                        self.transcript
                            .push(Block::new(Kind::Assistant, m.content.clone()));
                    }
                    if !m.tool_calls.is_empty() {
                        let mut children = Vec::with_capacity(m.tool_calls.len());
                        let mut ids = Vec::with_capacity(m.tool_calls.len());
                        for call in &m.tool_calls {
                            restored_id += 1;
                            let args = serde_json::from_str(&call.arguments)
                                .unwrap_or(serde_json::Value::Null);
                            let shown = crate::core::tools::present(&call.name, &args);
                            children.push(crate::tui::transcript::ToolChild::pending(
                                restored_id,
                                shown.category,
                                shown.running,
                                shown.completed,
                                shown.target,
                            ));
                            ids.push((call.id.clone(), restored_id));
                        }
                        let block = match open_group {
                            Some(idx) => {
                                if let Some(group) = self.transcript.blocks.get_mut(idx) {
                                    group.tool_children.extend(children);
                                    group.touch();
                                }
                                idx
                            }
                            None => self.transcript.push(Block::tool_group(children)),
                        };
                        open_group = Some(block);
                        for (call_id, id) in ids {
                            restored_calls.insert(call_id, (block, id));
                        }
                    }
                }
                "tool" => {
                    let Some(call_id) = m.tool_call_id.as_ref() else {
                        continue;
                    };
                    let Some(&(block, id)) = restored_calls.get(call_id) else {
                        continue;
                    };
                    let (outcome, summary) = m
                        .tool_meta
                        .as_ref()
                        .map(|meta| (meta.outcome, meta.summary.clone()))
                        .unwrap_or((crate::core::tools::ToolOutcome::Completed, "done".into()));
                    let mut title = None;
                    if let Some(group) = self.transcript.blocks.get_mut(block) {
                        group.start_tool(id);
                        if let Some(child) = group.tool_children.iter().find(|child| child.id == id)
                        {
                            title = Some(if child.target.is_empty() {
                                child.completed.clone()
                            } else {
                                format!("{} {}", child.completed, child.target)
                            });
                        }
                        group.finish_tool(id, outcome, summary, &m.content);
                    }
                    // Recorded results come back to the ctrl+o review
                    // screen, the same store the live session fills.
                    if !m.content.trim().is_empty() {
                        let detail = self.remember_output(
                            title.unwrap_or_else(|| "tool output".into()),
                            crate::core::tools::sanitize_display(&m.content),
                        );
                        if let Some(group) = self.transcript.blocks.get_mut(block) {
                            if let Some(child) =
                                group.tool_children.iter_mut().find(|child| child.id == id)
                            {
                                child.detail = Some(detail);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Seal every restored group: no more results are coming, so a child
        // still pending renders (and tallies) as unreported instead of
        // silently vanishing, and a later live batch starts its own tree
        // instead of splicing into a restored one.
        for block in &mut self.transcript.blocks {
            if block.kind == Kind::ToolGroup {
                block.seal();
            }
        }
        // Seed the context gauge from the restored history so the statusline
        // and the auto-compact check don't see an empty context until the
        // first real usage report lands.
        self.context_tokens =
            crate::core::agent::compact::estimate_request_tokens(&system_prompt(), messages);
    }

    fn resume_path(&mut self, path: std::path::PathBuf) {
        // The picker can already be open when a turn starts (queued prompt);
        // re-check here so a selection can never splice a running turn's
        // output into the resumed session. `is_streaming` also covers the
        // gap between a submit and its TurnStart event.
        if self.active.is_some() || self.agent.is_streaming() {
            self.notice("a turn is running — press Esc to stop it, then resume".into());
            return;
        }
        let messages = match crate::core::session::Session::load(&path) {
            Ok(m) => m,
            Err(e) => {
                self.notice(format!("could not open session: {e}"));
                self.release_initial_prompt();
                return;
            }
        };
        // Ownership first: a session another e is appending to must not be
        // replayed into a second, diverging history.
        let session = match crate::core::session::Session::reopen(&path) {
            Ok(s) => s,
            Err(e) => {
                self.notice(format!("could not resume session: {e}"));
                self.release_initial_prompt();
                return;
            }
        };
        // The old transcript's shell block index and held prompts die with
        // it; a still-running `!` command's result is epoch-discarded, and a
        // /compact still summarizing the old session must not land its swap
        // on the resumed one.
        self.shell_block = None;
        self.held_prompts.clear();
        self.compacting = false;
        self.compaction_epoch += 1;
        self.compact_requested = false;
        self.rebuild_transcript(&messages);
        self.agent.load_history(messages);
        self.agent.set_session(Some(session));
        // Identity travels together: the resumed log's persisted name
        // replaces whatever the previous session was called.
        self.agent
            .adopt_session_name(crate::core::session::name_of(&path));
        self.session_epoch += 1;
        self.notice(format!(
            "resumed {}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        // A -r launch prompt waits for this selection; deliver it against
        // the loaded history.
        if let Some(initial) = self.pending_initial.take() {
            self.submit_initial(initial);
        }
    }

    /// /tree: list every earlier user turn in the active session as a
    /// rewind point. Picking one means "go back to just before this and try
    /// something different" — the new branch replaces it, not extends it.
    fn open_tree_menu(&mut self) {
        if self.active.is_some() || self.agent.is_streaming() {
            self.notice("a turn is running — press Esc to stop it, then /tree".into());
            return;
        }
        let Some(path) = self.agent.session_path() else {
            self.notice("nothing to rewind yet — send a message first".into());
            return;
        };
        let nodes = match crate::core::session::Session::nodes(&path) {
            Ok(n) => n,
            Err(e) => {
                self.notice(format!("could not read session: {e}"));
                return;
            }
        };
        let items: Vec<MenuItem> = tree_items(&nodes)
            .into_iter()
            .map(|(id, preview, branched)| {
                let mut item = MenuItem::new(
                    if preview.is_empty() {
                        "(empty)"
                    } else {
                        &preview
                    },
                    "",
                    &id,
                );
                if branched {
                    item.meta = "⑂ branch point".into();
                }
                item
            })
            .collect();
        if items.is_empty() {
            self.notice("nothing to rewind to yet".into());
            return;
        }
        self.menu = Some(Menu::new(MenuKind::Tree, "Rewind to", HINT_USE, items));
    }

    /// Apply a /tree choice: rewind to just before the chosen user message,
    /// restore its text in the composer, and replay everything before it
    /// into the transcript and agent history. The file stays untouched. The
    /// edited or resent message grows a sibling branch beside the old tail.
    fn rewind_to_node(&mut self, node_id: &str) {
        if self.active.is_some() || self.agent.is_streaming() {
            self.notice("a turn is running — press Esc to stop it, then /tree".into());
            return;
        }
        let Some(path) = self.agent.session_path() else {
            return;
        };
        let nodes = match crate::core::session::Session::nodes(&path) {
            Ok(n) => n,
            Err(e) => {
                self.notice(format!("could not read session: {e}"));
                return;
            }
        };
        let Some((head, messages, prompt)) = rewind_target(&nodes, node_id) else {
            self.notice("that point no longer exists".into());
            return;
        };
        self.shell_block = None;
        self.held_prompts.clear();
        self.compacting = false;
        self.compaction_epoch += 1;
        self.compact_requested = false;
        self.rebuild_transcript(&messages);
        self.agent.rewind_to(head, messages);
        self.editor.set_text(&prompt);
        self.session_epoch += 1;
        self.notice("edit or resend the restored prompt to branch".into());
    }

    fn open_settings(&mut self) {
        self.menu = None;
        self.settings = Some(crate::tui::settingspanel::SettingsPanel::new(
            self.agent.efforts(),
        ));
    }

    fn dispatch_command(&mut self, command: String) {
        match command.as_str() {
            "/login" => self.open_login_menu(),
            "/models" | "/model" => self.open_model_menu(),
            "/scoped-models" => self.open_scoped_menu(),
            "/reload" => self.reload(),
            "/settings" => self.open_settings(),
            "/resume" => self.open_resume_menu(),
            "/copy" => self.copy_last(),
            other => self.submit(other.to_string()),
        }
    }

    fn submit(&mut self, text: String) {
        let text = self.editor.expand_pastes(&text);
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }

        let route = input_route(self.pending_key.is_some(), self.host.has_input_hook());
        // API keys are consumed before extension dispatch, matching the
        // documented boundary that secrets never reach input hooks.
        if route == InputRoute::ApiKey {
            self.submit_api_key(&trimmed);
            return;
        }

        // An input hook can consume or rewrite the line before anything else
        // sees it. Completed calls are applied in submission order below.
        if route == InputRoute::Hook {
            let host = self.host.clone();
            let results = self.results.clone();
            let sequence = self.input_verdicts.reserve();
            tokio::spawn(async move {
                let verdict = host.hook_input(&trimmed).await;
                let _ = results
                    .send(AppJob::InputVerdict {
                        sequence,
                        text,
                        images: None,
                        verdict,
                    })
                    .await;
            });
            return;
        }
        self.submit_direct(trimmed);
    }

    fn apply_input_verdict(
        &mut self,
        text: String,
        images: Option<Vec<crate::core::providers::ImageInput>>,
        verdict: crate::core::api::InputVerdict,
    ) {
        if let Some(notice) = verdict.notice.filter(|n| !n.trim().is_empty()) {
            self.notice(notice);
        }
        if verdict.consume {
            // Swallowed entirely — nothing reaches the agent. Images that
            // rode along with a consumed initial prompt are dropped with
            // it; there is no accepted text left to attach them to.
        } else if let Some(replace) = verdict.replace {
            // The extension rewrote the line; it already saw the original, so
            // no second hook pass.
            match images {
                Some(images) if !images.is_empty() => {
                    self.submit_initial_with_images(replace, images)
                }
                _ => self.submit_direct(replace),
            }
        } else {
            // Allowed through — the hook already saw the text, so submit
            // directly. Re-running submit() here would loop through the hook.
            match images {
                Some(images) if !images.is_empty() => self.submit_initial_with_images(text, images),
                _ => self.submit_direct(text),
            }
        }
    }

    /// The real submit flow, after the input hook (if any) has had its say.
    fn submit_direct(&mut self, text: String) {
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return;
        }
        self.editor.push_history(text);

        // `!cmd` runs in the shell directly; the output lands in the
        // transcript and in history, so the model sees what the user did.
        if let Some(cmd) = trimmed.strip_prefix('!').map(str::trim) {
            if !cmd.is_empty() {
                self.run_shell(cmd.to_string());
                return;
            }
        }

        if let Some(rest) = command_arg(&trimmed, "/login") {
            let provider = rest.trim().to_string();
            if provider.is_empty() {
                self.open_login_menu();
            } else {
                self.login(provider);
            }
            return;
        }

        if trimmed == "/scoped-models" {
            self.open_scoped_menu();
            return;
        }
        let model_rest =
            command_arg(&trimmed, "/models").or_else(|| command_arg(&trimmed, "/model"));
        if let Some(rest) = model_rest {
            let query = rest.trim();
            if query.is_empty() {
                self.open_model_menu();
            } else if let Some(found) = model::resolve(query) {
                if let Err(error) = persist_model(&found) {
                    self.notice(format!("could not save model choice: {error}"));
                    return;
                }
                self.notice(format!("model set to {}", model::slug(&found)));
                self.agent.model = found;
                self.refresh_status_cache();
            } else {
                self.notice(format!(
                    "no available model matches {query:?} — sign in to its provider with /login"
                ));
            }
            return;
        }
        match trimmed.as_str() {
            "/quit" | "/exit" => self.should_quit = true,
            "/version" => self.notice(format!("e {}", crate::VERSION)),
            "/help" => {
                // The reference help surface is the commands picker itself —
                // browse, filter, Enter to use — not a wall of text. The
                // non-command shortcuts ride the transcript as one notice so
                // they stay discoverable.
                self.notice(
                    "! <cmd> runs a shell command (the model sees the output) · \
                     shift+tab cycles reasoning effort · ctrl+o opens full tool detail"
                        .into(),
                );
                self.menu = Some(
                    crate::tui::menu::Menu::new(
                        crate::tui::menu::MenuKind::Commands,
                        "Commands",
                        crate::tui::menu::HINT_USE,
                        self.command_items(),
                    )
                    .without_trigger(),
                );
            }
            "/new" | "/clear" => {
                // A running turn owns the history and session log; replacing
                // them mid-turn would commit its reply into the wrong
                // session. `is_streaming` also covers the gap between a
                // submit and its TurnStart event.
                if self.active.is_some() || self.agent.is_streaming() {
                    self.notice("a turn is running — press Esc to stop it, then /new".into());
                    return;
                }
                self.compacting = false;
                self.compaction_epoch += 1;
                self.compact_requested = false;
                self.held_prompts.clear();
                self.shell_block = None;
                self.reload_block = None;
                self.context_tokens = 0;
                self.agent.clear();
                self.agent.clear_session_name();
                self.agent.set_session(None);
                // The name is part of session identity: a fresh session must
                // not inherit the old one's.
                self.agent.adopt_session_name(None);
                self.session_epoch += 1;
                self.transcript.clear();
                self.transcript
                    .push(Block::new(Kind::Banner, crate::VERSION));
                set_tab_title(&tab_title(&title_path(), None));
            }
            "/resume" => self.open_resume_menu(),
            "/tree" => self.open_tree_menu(),
            "/settings" => self.open_settings(),
            "/copy" => self.copy_last(),
            "/compact" => self.compact_now(),
            "/reload" => self.reload(),
            "/trust" => match crate::core::config::trust::set(&self.agent.cwd(), true) {
                Ok(()) => self.notice(
                    "directory trusted — its AGENTS.md and .e skills/prompts now load".into(),
                ),
                Err(e) => self.notice(format!("trust: {e}")),
            },
            _ if trimmed.starts_with('/') => {
                let (name, args) = trimmed[1..].split_once(' ').unwrap_or((&trimmed[1..], ""));
                if let Some(template) =
                    crate::core::resources::prompts::find(name, &self.agent.cwd())
                {
                    let expanded =
                        crate::core::resources::prompts::substitute(&template.content, args);
                    self.prompt(expanded);
                } else if self.host.has_command(name) {
                    let host = self.host.clone();
                    let results = self.results.clone();
                    let (name, args) = (name.to_string(), args.to_string());
                    let epoch = self.session_epoch;
                    tokio::spawn(async move {
                        let out = host.run_command(&name, &args).await;
                        if let Some(notice) = out.notice {
                            let _ = results.send(AppJob::Notice(notice)).await;
                        }
                        if let Some(text) = out.prompt {
                            let _ = results.send(AppJob::Prompt { text, epoch }).await;
                        }
                        if let Some(name) = out.session_name.filter(|n| !n.trim().is_empty()) {
                            let _ = results.send(AppJob::Rename { name, epoch }).await;
                        }
                    });
                } else {
                    self.notice(format!("unknown command {trimmed}"));
                }
            }
            _ => self.prompt(trimmed),
        }
    }

    /// Deliver a held -r launch prompt into the current session — the pick
    /// it was waiting for fell through (declined, or the resume failed), and
    /// stranding it would silently drop typed text. The trust question, if
    /// still open, keeps holding it.
    fn release_initial_prompt(&mut self) {
        if self.trust.is_none() {
            if let Some(initial) = self.pending_initial.take() {
                self.submit_initial(initial);
            }
        }
    }

    fn submit_initial(&mut self, text: String) {
        if self.pending_initial_images.is_empty() {
            self.submit(text);
            return;
        }
        // Images travel with the initial launch prompt only, so this can't
        // just call submit(): the hook contract is "sees the text before
        // anything else does," and a plain submit() has no way to carry
        // images through to the eventual submission. Route the text through
        // the same hook decision submit() makes, and attach the images to
        // whatever text is actually accepted (see apply_input_verdict).
        let route = input_route(self.pending_key.is_some(), self.host.has_input_hook());
        if route == InputRoute::Hook {
            let host = self.host.clone();
            let results = self.results.clone();
            let sequence = self.input_verdicts.reserve();
            let images = std::mem::take(&mut self.pending_initial_images);
            tokio::spawn(async move {
                let verdict = host.hook_input(&text).await;
                let _ = results
                    .send(AppJob::InputVerdict {
                        sequence,
                        text,
                        images: Some(images),
                        verdict,
                    })
                    .await;
            });
            return;
        }
        let images = std::mem::take(&mut self.pending_initial_images);
        self.submit_initial_with_images(text, images);
    }

    fn submit_initial_with_images(
        &mut self,
        text: String,
        images: Vec<crate::core::providers::ImageInput>,
    ) {
        let count = images.len();
        let held = self.agent.submit_message(
            crate::core::providers::ChatMessage::user_with_images(text.clone(), images),
            system_prompt(),
        );
        if !held {
            self.transcript.push(Block::new(
                Kind::User,
                format!(
                    "{text}\n[attached {count} image{}]",
                    if count == 1 { "" } else { "s" }
                ),
            ));
        }
    }

    fn prompt(&mut self, text: String) {
        // A fresh prompt closes the queued-prompt review and resumes the
        // queue — the reference's resume-after-new-prompt.
        self.close_queue_review();
        // While compacting, reloading, or a `!` shell command is running,
        // hold the message; it submits (and displays) when the block lifts —
        // a turn must not start without the shell output it was promised.
        if self.compacting || self.reloading || self.shell_block.is_some() {
            self.held_prompts.push(text);
            return;
        }
        // While a turn runs the message is held and steered in (echoed later
        // via Steered); idle, it begins a turn now.
        let held = self.agent.submit(text.clone(), system_prompt());
        if !held {
            self.transcript.push(Block::new(Kind::User, text));
        }
    }

    /// /compact: mid-turn it is deferred to TurnEnd (compaction never touches
    /// a running turn); idle it starts now.
    fn compact_now(&mut self) {
        if self.agent.is_streaming() {
            if !self.compact_requested {
                self.compact_requested = true;
                self.notice("will compact when this turn ends".into());
            }
            return;
        }
        if self.shell_block.is_some() {
            self.notice("a shell command is running — compact after it finishes".into());
            return;
        }
        self.start_compaction(false);
    }

    /// Summarize everything before the keep-recent cut, off-task; the swap
    /// lands via AppJob::Compacted. `auto` softens the too-small notice.
    fn start_compaction(&mut self, auto: bool) {
        if self.compacting {
            if !auto {
                self.notice("already compacting".into());
            }
            return;
        }
        if self.shell_block.is_some() {
            if !auto {
                self.notice("a shell command is running — compact after it finishes".into());
            }
            return;
        }
        let history = self.agent.history_snapshot();
        if history.is_empty() {
            self.notice("nothing to compact yet".into());
            return;
        }
        let (to_summarize, kept) =
            crate::core::agent::compact::split(&history, self.agent.model.context_window);
        if to_summarize.is_empty() {
            if !auto {
                self.notice("recent context already fits — nothing to compact".into());
            }
            return;
        }
        self.compacting = true;
        self.compaction_epoch += 1;
        let epoch = self.compaction_epoch;
        self.notice("compacting…".into());
        let model = self.agent.model.clone();
        let results = self.results.clone();
        tokio::spawn(async move {
            let job = match crate::core::agent::compact::summarize(model, &to_summarize).await {
                Ok(summary) => AppJob::Compacted {
                    summary,
                    kept,
                    epoch,
                },
                Err(message) => AppJob::CompactFailed { message, epoch },
            };
            let _ = results.send(job).await;
        });
    }

    fn compaction_is_current(&self, epoch: u64) -> bool {
        self.compacting && epoch == self.compaction_epoch
    }

    /// `!cmd`: run it through the bash tool off-task; the result arrives as
    /// AppJob::Shell. Idle only — mid-turn the history is the model's.
    fn run_shell(&mut self, cmd: String) {
        if self.agent.is_streaming() || self.compacting {
            self.notice("busy — run shell commands between turns".into());
            return;
        }
        if self.shell_block.is_some() {
            self.notice("a shell command is still running".into());
            return;
        }
        self.transcript.push(Block::new(Kind::Shell, cmd.clone()));
        self.shell_block = Some(self.transcript.blocks.len() - 1);
        let results = self.results.clone();
        let cwd = self.agent.cwd();
        let epoch = self.session_epoch;
        tokio::spawn(async move {
            let shell_cmd = cmd.clone();
            let output = tokio::task::spawn_blocking(move || {
                crate::core::tools::run_shell(&shell_cmd, &cwd)
            })
            .await
            .unwrap_or(crate::core::tools::ToolOutput {
                content: "shell command panicked".into(),
                outcome: crate::core::tools::ToolOutcome::Failed,
                summary: "error".into(),
                display: None,
            });
            let _ = results.send(AppJob::Shell { cmd, output, epoch }).await;
        });
    }

    /// Store a full tool output for the review screen, returning its stable
    /// id. Eviction under the budget leaves a dangling id behind — the
    /// screen then says "Full saved result unavailable.", honestly.
    fn remember_output(&mut self, title: String, content: String) -> u64 {
        const OUTPUT_BUDGET: usize = 4 * 1024 * 1024;
        self.output_seq += 1;
        let id = self.output_seq;
        self.outputs.push((id, title, content));
        let mut bytes: usize = self.outputs.iter().map(|(_, _, body)| body.len()).sum();
        while self.outputs.len() > 1 && (self.outputs.len() > 50 || bytes > OUTPUT_BUDGET) {
            let removed = self.outputs.remove(0).2.len();
            bytes = bytes.saturating_sub(removed);
        }
        id
    }

    fn output_body(outputs: &[(u64, String, String)], id: u64) -> Option<&str> {
        outputs
            .iter()
            .find(|(stored, _, _)| *stored == id)
            .map(|(_, _, body)| body.as_str())
    }

    /// /reload, the reference behavior: refresh what a session caches. In e
    /// that is the extension host (restarted) and the theme (re-resolved) —
    /// skills, prompts, AGENTS.md, settings, and models.json are read fresh
    /// on every use already.
    fn reload(&mut self) {
        if self.agent.is_streaming() {
            self.notice("wait for the turn to finish before /reload".into());
            return;
        }
        if self.compacting {
            self.notice("wait for compaction to finish before /reload".into());
            return;
        }
        if self.reloading {
            return;
        }
        // With a freshly installed update on disk, /reload becomes the
        // switch: exit through the normal cleanup and exec the new binary
        // with -c, which resumes this session.
        if self.update_installed.is_some() {
            self.relaunch = true;
            self.should_quit = true;
            return;
        }
        self.reloading = true;
        self.reload_block = Some(self.transcript.push(Block::new(Kind::Notice, "reloading…")));
        let old = self.host.clone();
        let jobs = self.jobs.clone();
        let results = self.results.clone();
        tokio::spawn(async move {
            old.shutdown().await;
            let host = crate::core::api::ExtensionHost::start(jobs).await;
            let _ = results.send(AppJob::Reloaded(host)).await;
        });
    }

    fn copy_last(&mut self) {
        let last = self
            .agent
            .history_snapshot()
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.trim().is_empty())
            .map(|m| m.content.clone());
        match last {
            Some(text) => {
                // OSC 52: the terminal-native clipboard, no helper binary,
                // works over ssh too. Terminals without it silently ignore
                // the sequence.
                use base64::Engine;
                let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
                let ok = write!(std::io::stdout(), "\x1b]52;c;{encoded}\x07").is_ok();
                let _ = std::io::stdout().flush();
                self.notice(if ok {
                    "copied the last reply".into()
                } else {
                    "copy failed".into()
                });
            }
            None => self.notice("nothing to copy yet".into()),
        }
    }

    /// Reload the theme from settings, using the startup background probe.
    fn apply_theme(&mut self) {
        self.theme = crate::tui::theme::resolve(
            &crate::core::config::settings::theme(),
            self.light_background,
        );
        self.transcript.invalidate();
    }

    /// Re-read `~/.e/keybindings.json`. A malformed or missing file fails
    /// open to no overrides — never an error that blocks typing.
    fn apply_keymap(&mut self) {
        self.keymap = crate::core::config::keybindings::load();
    }

    /// Re-derive the cached sign-in and effort state from disk. Call after
    /// anything that can change them: sign-in, model switch, effort cycle,
    /// settings changes, /reload.
    fn refresh_status_cache(&mut self) {
        self.signed_in =
            crate::core::auth::signed_in(&crate::core::auth::load(), &self.agent.model.provider);
        self.status_effort = self.agent.effort();
    }

    fn notice(&mut self, text: String) {
        self.transcript.push(Block::new(Kind::Notice, text));
    }
}

/// Replace /reload's in-progress notice, or append the result if that block
/// disappeared when another command cleared the transcript.
fn finish_reload_notice(transcript: &mut Transcript, reload_block: Option<usize>) {
    const STARTED: &str = "reloading…";
    const FINISHED: &str = "reloaded extensions, themes, and config — skills, prompts, and AGENTS.md are always read fresh";

    if let Some(block) = reload_block
        .and_then(|index| transcript.blocks.get_mut(index))
        .filter(|block| block.kind == Kind::Notice && block.text == STARTED)
    {
        block.text = FINISHED.into();
        block.touch();
    } else {
        transcript.push(Block::new(Kind::Notice, FINISHED));
    }
}

/// The terminal tab title: the custom glyph, a dot, then the session name
/// or the working directory (the reference prefers the session name and
/// falls back to the workspace path).
fn tab_title(path: &str, session_name: Option<&str>) -> String {
    let label = session_name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or(path);
    format!("𝑒 · {label}")
}

fn set_tab_title(title: &str) {
    // Escape codes into a pipe are garbage in the pipe; titles only make
    // sense on a terminal.
    if !stdout_is_tty() {
        return;
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]0;{title}\x07");
    let _ = out.flush();
}

/// The tab title's path: a short showcase, never the full absolute path.
/// Under $HOME the prefix collapses to `~`; elsewhere only the last two
/// components are shown, so a volume-qualified worktree reads cleanly
/// instead of bleeding its whole path into the tab.
fn title_path() -> String {
    title_path_from(
        &std::env::current_dir().unwrap_or_default(),
        &std::env::var("HOME").unwrap_or_default(),
    )
}

/// The shortening rule, split out for tests.
fn title_path_from(cwd: &std::path::Path, home: &str) -> String {
    use std::path::Component;
    let under_home = !home.is_empty() && cwd.starts_with(home);
    let mut comps: Vec<&str> = cwd
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_str().unwrap_or_default()),
            _ => None,
        })
        .collect();
    // The `~` marker replaces the whole home prefix, not one level of it.
    if under_home {
        let prefix = std::path::Path::new(home)
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_str().unwrap_or_default()),
                _ => None,
            })
            .count();
        comps.drain(..prefix.min(comps.len()));
    }
    let tail = comps.split_off(comps.len().saturating_sub(2)).join("/");
    if under_home {
        if tail.is_empty() {
            "~".to_string()
        } else {
            format!("~/{tail}")
        }
    } else if tail.is_empty() {
        // The root itself stays a slash rather than a bare "".
        "/".to_string()
    } else {
        tail
    }
}

/// The path as `~/…` when it lives under $HOME, whole otherwise — the
/// statusline's identity tail and the picker workspace labels share the
/// shape.
fn collapse_home(path: &std::path::Path) -> String {
    let shown = path.to_string_lossy().into_owned();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && shown.starts_with(&home) => {
            format!("~{}", &shown[home.len()..])
        }
        _ => shown,
    }
}

fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let secs = now.saturating_sub(ms) / 1000;
    if secs < 60 {
        "now".to_string()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

fn system_prompt() -> String {
    crate::core::agent::context::system_prompt_here()
}

fn persist_model(m: &Model) -> std::io::Result<()> {
    crate::core::config::settings::set_string("model", &model::slug(m))
}

/// The text after a slash command, only on a word boundary: `/login x` →
/// `Some(" x")`, `/login` → `Some("")`, `/loginfoo` → `None` (so a typo falls
/// through to the unknown-command notice instead of inventing an argument).
fn command_arg<'a>(input: &'a str, command: &str) -> Option<&'a str> {
    input
        .strip_prefix(command)
        .filter(|rest| rest.is_empty() || rest.starts_with(' '))
}

/// Decide whether a command-line prompt may start now or must wait for the
/// first-visit trust panel. Kept separate so launch ordering stays testable
/// without constructing a terminal frame.
/// Command names dispatch resolves before templates and extension commands.
/// Keep in sync with `dispatch_command`'s match arms.
fn is_builtin_command(name: &str) -> bool {
    matches!(
        name,
        "login"
            | "models"
            | "model"
            | "scoped-models"
            | "reload"
            | "resume"
            | "new"
            | "clear"
            | "copy"
            | "compact"
            | "trust"
            | "settings"
            | "help"
            | "version"
            | "quit"
            | "exit"
    )
}

fn stage_initial_prompt(
    initial: String,
    awaiting_trust: bool,
    pending: &mut Option<String>,
) -> Option<String> {
    if initial.trim().is_empty() {
        return None;
    }
    if awaiting_trust {
        *pending = Some(initial);
        None
    } else {
        Some(initial)
    }
}

/// Replace this process with the current e binary, optionally in a new cwd.
/// Extensions may choose arguments and environment, but never an arbitrary
/// executable.
pub fn relaunch_self(
    cwd: &str,
    args: &[String],
    env: &std::collections::BTreeMap<String, Option<String>>,
) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let mut command = std::process::Command::new(exe);
    command.current_dir(cwd).args(args);
    for (name, value) in env {
        if name.is_empty()
            || name.contains('=')
            || name.contains('\0')
            || value.as_deref().is_some_and(|value| value.contains('\0'))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid relaunch environment entry",
            ));
        }
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    Err(command.exec())
}

/// /tree's rewind points: every user-turn node's id, one-line preview, and
/// whether its parent already has more than one child — a branch point,
/// meaning /tree was used at that spot before.
fn tree_items(nodes: &[crate::core::session::Node]) -> Vec<(String, String, bool)> {
    let mut children_of = std::collections::HashMap::<&str, usize>::new();
    for n in nodes {
        if let Some(p) = n.parent.as_deref() {
            *children_of.entry(p).or_insert(0) += 1;
        }
    }
    nodes
        .iter()
        .filter(|n| n.message.role == "user")
        .map(|n| {
            let preview: String = n
                .message
                .content
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect();
            let branched = n
                .parent
                .as_deref()
                .map(|p| children_of.get(p).copied().unwrap_or(0) > 1)
                .unwrap_or(false);
            (n.id.clone(), preview, branched)
        })
        .collect()
}

/// The rewind target for a chosen node: its parent, the message history before
/// it, and its prompt text for the composer. None means the id no longer
/// resolves. A broken ancestor link truncates the replayed path there.
fn rewind_target(
    nodes: &[crate::core::session::Node],
    node_id: &str,
) -> Option<(
    Option<String>,
    Vec<crate::core::providers::ChatMessage>,
    String,
)> {
    let by_id: std::collections::HashMap<&str, &crate::core::session::Node> =
        nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let target = *by_id.get(node_id)?;
    let head = target.parent.clone();
    let mut path_ids = Vec::new();
    let mut cursor = head.clone();
    while let Some(id) = cursor {
        let Some(node) = by_id.get(id.as_str()).copied() else {
            break;
        };
        path_ids.push(id.clone());
        cursor = node.parent.clone();
    }
    path_ids.reverse();
    let messages = path_ids
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).map(|n| n.message.clone()))
        .collect();
    Some((head, messages, target.message.content.clone()))
}

/// The composer's editing keymap: a user's `~/.e/keybindings.json` chord
/// override is consulted first (`Some(action)` overrides, `Some(None)`
/// swallows the chord, `None` means "not mentioned"); anything left
/// unmentioned falls through to e's built-in bindings below, so an empty or
/// missing file reproduces this function's behavior exactly.
/// Question panels handle local input but leave global interrupt and exit keys alone.
fn question_owns_key(event: &KeyEvent) -> bool {
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    !(ctrl && matches!(event.code, KeyCode::Char('c') | KeyCode::Char('d')))
}

fn key_of(event: &KeyEvent, keymap: &crate::core::config::keybindings::Keymap) -> Option<Key> {
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let alt = event.modifiers.contains(KeyModifiers::ALT);
    let shift = event.modifiers.contains(KeyModifiers::SHIFT);
    if let Some(base) = crate::core::config::keybindings::base_name(event.code) {
        let chord = crate::core::config::keybindings::chord_string(ctrl, alt, shift, &base);
        if let Some(bound) = keymap.lookup(&chord) {
            return bound;
        }
    }
    Some(match (event.code, ctrl, alt) {
        (KeyCode::Enter, ..) if shift || alt => Key::Newline,
        (KeyCode::Enter, ..) => Key::Enter,
        (KeyCode::Backspace, _, true) => Key::KillWord,
        (KeyCode::Backspace, ..) => Key::Backspace,
        (KeyCode::Delete, ..) => Key::Delete,
        // Shift extends a selection through the same motions — the
        // reference's shift-arrow grammar; typing then replaces the range.
        (KeyCode::Left, _, true) if shift => Key::SelectWordLeft,
        (KeyCode::Right, _, true) if shift => Key::SelectWordRight,
        (KeyCode::Left, ..) if shift => Key::SelectLeft,
        (KeyCode::Right, ..) if shift => Key::SelectRight,
        (KeyCode::Up, ..) if shift => Key::SelectUp,
        (KeyCode::Down, ..) if shift => Key::SelectDown,
        (KeyCode::Home, ..) if shift => Key::SelectHome,
        (KeyCode::End, ..) if shift => Key::SelectEnd,
        (KeyCode::Left, _, true) => Key::WordLeft,
        (KeyCode::Right, _, true) => Key::WordRight,
        (KeyCode::Left, ..) => Key::Left,
        (KeyCode::Right, ..) => Key::Right,
        (KeyCode::Up, ..) => Key::Up,
        (KeyCode::Down, ..) => Key::Down,
        (KeyCode::Home, ..) => Key::Home,
        (KeyCode::End, ..) => Key::End,
        (KeyCode::Char('a'), true, _) => Key::Home,
        (KeyCode::Char('e'), true, _) => Key::End,
        (KeyCode::Char('k'), true, _) => Key::KillToEnd,
        (KeyCode::Char('u'), true, _) => Key::KillToStart,
        (KeyCode::Char('w'), true, _) => Key::KillWord,
        (KeyCode::Char('b'), true, _) => Key::Left,
        (KeyCode::Char('f'), true, _) => Key::Right,
        (KeyCode::Char('j'), true, _) => Key::Newline,
        (KeyCode::Char(c), false, false) => Key::Char(c),
        _ => return None,
    })
}

/// Open the interactive session. `args` are already past the extension
/// startup hook (flags rewritten, cwd possibly changed).
pub struct RunOptions {
    pub initial: String,
    pub continue_session: bool,
    pub resume_session: bool,
    pub model: Model,
    pub agent: AgentOptions,
    pub images: Vec<crate::core::providers::ImageInput>,
}

pub async fn run(
    options: RunOptions,
    host: std::sync::Arc<crate::core::api::ExtensionHost>,
    jobs_tx: tokio::sync::mpsc::Sender<String>,
    mut jobs_rx: tokio::sync::mpsc::Receiver<String>,
) -> std::io::Result<()> {
    let RunOptions {
        initial,
        continue_session,
        resume_session,
        model,
        agent: agent_options,
        images,
    } = options;
    // A panic mid-frame must not strand the shell in raw mode with a hidden
    // cursor or kitty keyboard flags — restore the terminal first, then
    // report as usual. (\x1b[<u pops the keyboard enhancement stack.)
    {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = terminal::disable_raw_mode();
            print!("\x1b[<u\x1b[?2004l\x1b[?25h\r\n");
            use std::io::Write as _;
            let _ = std::io::stdout().flush();
            default_hook(info);
        }));
    }

    // Raw mode first so the frame loop can take the terminal over. Theme
    // detection now queries the terminal (OSC 11 background color, then
    // COLORFGBG) so `auto` follows the real terminal theme instead of
    // defaulting to dark. The probe is timeout-bounded and runs here, where
    // the TUI owns the terminal reader, so it can't block startup or swallow
    // keystrokes (audit #93). The guard exists before any further mode
    // changes, so every exit path restores them.
    terminal::enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(
        std::io::stdout(),
        EnableBracketedPaste,
        // The kitty keyboard protocol: without it, terminals send plain Enter
        // for shift+enter and multi-line entry is unreachable.
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    // detect_light() probes the terminal background over OSC 11 (short
    // timeout) and falls back to COLORFGBG, then dark.
    let detected = crate::tui::background::detect_light().unwrap_or(false);
    let theme = crate::tui::theme::resolve(&crate::core::config::settings::theme(), detected);
    let keymap = crate::core::config::keybindings::load();

    let (mut cols, mut rows) = terminal::size()?;
    let mut painter = Painter::spawn(cols, rows);
    let (mut agent, mut session_events) = Agent::with_options(model, agent_options.clone());
    let (logins_tx, mut logins_rx) =
        tokio::sync::mpsc::channel::<crate::core::auth::login::Outcome>(4);
    let (results_tx, mut results_rx) = tokio::sync::mpsc::channel::<AppJob>(16);
    agent.set_host(host.clone());
    let mut app = App {
        theme,
        keymap,
        transcript: Transcript::default(),
        editor: Editor::new(),
        agent,
        active: None,
        overlay: None,
        armed_at: None,
        should_quit: false,
        context_tokens: 0,
        pending_key: None,
        menu: None,
        auth: None,
        settings: None,
        show_thinking: crate::core::config::settings::get_string("show_thinking").as_deref()
            == Some("on"),
        jobs: jobs_tx,
        logins: logins_tx,
        login_task: None,
        login_sequence: 0,
        host,
        results: results_tx,
        input_verdicts: PendingInputVerdicts::default(),
        compacting: false,
        compaction_epoch: 0,
        compact_requested: false,
        held_prompts: Vec::new(),
        trust: None,
        pending_initial: None,
        pending_initial_images: images,
        shell_block: None,
        reloading: false,
        reload_block: None,
        outputs: Vec::new(),
        output_seq: 0,
        viewer: None,
        viewer_cache: None,
        question: None,
        question_queue: std::collections::VecDeque::new(),
        queue_review: None,
        session_epoch: 0,
        update_installed: None,
        relaunch: false,
        light_background: detected,
        signed_in: false,
        status_effort: None,
    };
    app.refresh_status_cache();
    app.transcript
        .push(Block::new(Kind::Banner, crate::VERSION));
    for warning in model::config_warnings() {
        app.notice(format!("warning: {warning}"));
    }
    if !agent_options.save_session {
        app.notice("session saving disabled for this run".into());
    }
    match agent_options.tool_mode {
        crate::core::cli::ToolMode::ReadOnly => {
            app.notice("read-only mode — only read and grep are available".into())
        }
        crate::core::cli::ToolMode::None => {
            app.notice("no-tools mode — provider requests contain no tool schemas".into())
        }
        crate::core::cli::ToolMode::All => {}
    }
    if crate::core::config::trust::status(&app.agent.cwd()).is_none() {
        app.trust = Some(TrustStage::new(&app.agent.cwd()));
    }
    // The harness pattern: check for a newer release in the background at
    // launch, install it silently, and say so — the running session is
    // untouched until a restart. Dev builds and the opt-out are exempt.
    if !crate::core::update::is_dev_build() && crate::core::config::settings::auto_update() {
        let results = app.results.clone();
        tokio::spawn(async move {
            if let Ok(Some(version)) = crate::core::update::self_update().await {
                let _ = results.send(AppJob::Updated(version)).await;
            }
        });
    }
    // Providers' model lists refresh in the background (the reference
    // behavior, sourced from each gateway's own /models): a model a provider
    // ships today shows in /models today, no e release involved.
    tokio::spawn(crate::core::providers::catalog::refresh_remote());
    if crate::core::auth::load().is_empty() {
        app.notice(
            "no provider signed in — use /login to sign in with an account or API key".into(),
        );
        // Route straight to sign-in instead of leaving a phantom model
        // implied on the status bar. Yields to the trust panel above it,
        // if that's showing too — this still renders once trust is settled.
        app.open_login_menu();
    } else if let Some(wanted) = crate::core::config::settings::get_string("model") {
        let current = app.agent.model_slug();
        if wanted != current {
            app.notice(format!(
                "{wanted} is unavailable (provider not signed in) — using {current}"
            ));
        }
    }
    if resume_session {
        // The reference behavior: launch straight into the session picker.
        app.open_resume_menu();
    } else if continue_session {
        app.resume_recent();
    }

    // Terminal tab title: the custom glyph, a dot, the path — a named
    // session takes over the title when one lands (the reference prefers the
    // session name over the workspace).
    set_tab_title(&tab_title(
        &title_path(),
        app.agent.session_name().as_deref(),
    ));
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    // SIGTERM/SIGHUP (a kill, a closed tab) exit through the same cleanup as
    // /quit — the terminal is restored, the extension host shut down.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;

    // A -r launch prompt must wait for the session pick, or it would start a
    // turn whose reply splices into whichever session gets selected.
    let hold_initial = app.trust.is_some() || (resume_session && app.menu.is_some());
    if let Some(initial) = stage_initial_prompt(initial, hold_initial, &mut app.pending_initial) {
        app.submit_initial(initial);
    }
    painter.frame(app.frame(cols as usize, rows as usize));

    // Frame pacing: every select arm may change what's on screen, but frames
    // are built at most once per interval — a token burst becomes one paint,
    // and a deferred paint fires when the interval lapses.
    const FRAME_INTERVAL: Duration = Duration::from_millis(33);
    let mut next_paint = tokio::time::Instant::now();
    let mut paint_deferred = false;
    let mut event_buf: Vec<SessionEvent> = Vec::with_capacity(128);

    loop {
        tokio::select! {
            maybe = events.next() => {
                let Some(Ok(event)) = maybe else { break };
                match event {
                    TermEvent::Paste(text) => {
                        // A paste is one unit; long or multiline pastes become
                        // a placeholder token (the reference behavior) that
                        // expands back on submit.
                        app.editor.insert_paste(&text.replace('\r', "\n"));
                        app.sync_menu();
                    }
                    TermEvent::Resize(c, r) => {
                        cols = c;
                        rows = r;
                        painter.resize(c, r);
                    }
                    TermEvent::Key(k) if k.kind != crossterm::event::KeyEventKind::Release => {
                        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                        if app.viewer.is_some() {
                            let close = k.code == KeyCode::Esc
                                || (ctrl && k.code == KeyCode::Char('o'));
                            if close {
                                app.viewer = None;
                            } else {
                                let full =
                                    app.viewer.as_ref().map(|v| v.full).unwrap_or(false);
                                let total = app.viewer_rows(cols as usize, full).len();
                                if let Some(viewer) = &mut app.viewer {
                                    match k.code {
                                        KeyCode::Up => {
                                            viewer.scroll = viewer.scroll.saturating_sub(1)
                                        }
                                        KeyCode::Down => {
                                            viewer.scroll =
                                                (viewer.scroll + 1).min(total.saturating_sub(1))
                                        }
                                        KeyCode::PageUp => {
                                            viewer.scroll = viewer.scroll.saturating_sub(20)
                                        }
                                        KeyCode::PageDown => {
                                            viewer.scroll =
                                                (viewer.scroll + 20).min(total.saturating_sub(1))
                                        }
                                        // ←/→ switch between the reference's
                                        // two depths: Review folds details,
                                        // Full expands everything.
                                        KeyCode::Left => viewer.full = false,
                                        KeyCode::Right => viewer.full = true,
                                        _ => {}
                                    }
                                }
                            }
                        } else if ctrl && k.code == KeyCode::Char('o') {
                            app.viewer = Some(Viewer {
                                full: false,
                                scroll: 0,
                            });
                        } else if let Some(stage) = &mut app.trust {
                            match k.code {
                                KeyCode::Up => stage.step(-1),
                                KeyCode::Down => stage.step(1),
                                KeyCode::Enter => {
                                    // The middle row (when offered) trusts the
                                    // broader ancestor; trust propagates down,
                                    // so the workspace is covered too.
                                    let (parent, trusted) = stage.choice();
                                    let target = parent.unwrap_or_else(|| app.agent.cwd().to_path_buf());
                                    match crate::core::config::trust::set(&target, trusted) {
                                        Err(e) => app.notice(format!("trust: {e}")),
                                        Ok(()) => {
                                            app.trust = None;
                                            if !trusted {
                                                app.notice("working untrusted — project AGENTS.md and .e skills/prompts ignored (/trust to allow)".into());
                                            }
                                            // An open -r picker still owns
                                            // the launch prompt; submitting
                                            // it now would start a turn the
                                            // session pick then refuses.
                                            if app.menu.is_none() {
                                                if let Some(initial) = app.pending_initial.take() {
                                                    app.submit_initial(initial);
                                                }
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else if app.question.is_some() && question_owns_key(&k) {
                            // The ask tool's panel owns its local keys while open.
                            // Esc dismisses the question (the tool records a
                            // cancel) without aborting the turn; a digit
                            // chooses and answers in one stroke.
                            let mut submit: Option<Option<String>> = None;
                            if let Some(q) = &mut app.question {
                                match k.code {
                                    KeyCode::Esc => submit = Some(None),
                                    KeyCode::Up => q.step(-1),
                                    KeyCode::Down => q.step(1),
                                    KeyCode::Enter => {
                                        if let Some(text) = q.answer() {
                                            submit = Some(Some(text));
                                        }
                                    }
                                    KeyCode::Backspace if q.freeform_selected() => {
                                        q.freeform.pop();
                                    }
                                    KeyCode::Char(c) if !ctrl => {
                                        if q.freeform_selected() {
                                            q.freeform.push(c);
                                        } else if let Some(n) = c.to_digit(10) {
                                            if q.choose(n as usize) {
                                                if let Some(text) = q.answer() {
                                                    submit = Some(Some(text));
                                                }
                                            }
                                        } else if q.allow_freeform {
                                            q.selected = q.options.len();
                                            q.freeform.push(c);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            if let Some(reply) = submit {
                                if let Some(q) = app.question.take() {
                                    app.agent.answer(q.id, reply);
                                }
                                app.question = app.question_queue.pop_front();
                            }
                        } else if let Some(panel) = &mut app.settings {
                            let mut setting_error = None;
                            match k.code {
                                KeyCode::Up => panel.step(-1),
                                KeyCode::Down => panel.step(1),
                                KeyCode::Left => setting_error = panel.change(-1).err(),
                                KeyCode::Right => setting_error = panel.change(1).err(),
                                // Esc alone closes: Enter opens the panel
                                // from the command menu, so it must not be
                                // the same key that dismisses it.
                                KeyCode::Esc => app.settings = None,
                                _ => {}
                            }
                            if let Some(error) = setting_error {
                                app.notice(format!("could not save setting: {error}"));
                            }
                            // A theme change applies immediately; settings can
                            // also change what the statusline derives from disk.
                            // The thinking toggle and keymap are file-backed
                            // too — re-read them so a mid-session change
                            // lands this frame.
                            app.apply_theme();
                            app.apply_keymap();
                            app.show_thinking = crate::core::config::settings::get_string(
                                "show_thinking",
                            )
                            .as_deref()
                            == Some("on");
                            app.refresh_status_cache();
                        } else if let Some(stage) = &mut app.auth {
                            match (&mut *stage, k.code) {
                                (AuthStage::Choose { selected }, KeyCode::Up | KeyCode::Down) => {
                                    *selected = 1 - *selected;
                                }
                                (AuthStage::Choose { selected }, KeyCode::Enter) => {
                                    let choice = *selected;
                                    app.auth_choose(choice);
                                }
                                (AuthStage::Account { selected }, KeyCode::Up | KeyCode::Down) => {
                                    let n = crate::core::providers::registry::oauth_providers().len();
                                    *selected = (*selected + 1) % n.max(1);
                                }
                                (AuthStage::Key { selected }, KeyCode::Up) => {
                                    let n = crate::core::providers::registry::key_providers().len();
                                    *selected = (*selected + n - 1) % n.max(1);
                                }
                                (AuthStage::Key { selected }, KeyCode::Down) => {
                                    let n = crate::core::providers::registry::key_providers().len();
                                    *selected = (*selected + 1) % n.max(1);
                                }
                                (AuthStage::Account { selected }, KeyCode::Enter) => {
                                    let choice = *selected;
                                    app.auth_account(choice);
                                }
                                (AuthStage::Key { selected }, KeyCode::Enter) => {
                                    let choice = *selected;
                                    app.auth_key(choice);
                                }
                                (_, KeyCode::Esc) => {
                                    // Esc closes the whole panel from any
                                    // depth; an in-flight flow is cancelled
                                    // with it.
                                    let waiting = matches!(&*stage, AuthStage::Waiting { .. });
                                    let cancelled = app.cancel_login();
                                    app.auth = None;
                                    app.pending_key = None;
                                    app.editor.mask = false;
                                    app.editor.set_text("");
                                    if waiting && cancelled {
                                        app.notice("login cancelled".into());
                                    }
                                }
                                (AuthStage::Account { .. }, KeyCode::Backspace) => {
                                    *stage = AuthStage::Choose { selected: 0 };
                                }
                                (AuthStage::Key { .. }, KeyCode::Backspace) => {
                                    *stage = AuthStage::Choose { selected: 1 };
                                }
                                // The entry keeps Backspace for editing while
                                // there is text; an empty input navigates back.
                                (AuthStage::ApiKey { .. }, KeyCode::Backspace)
                                    if app.editor.is_empty() =>
                                {
                                    let provider = match &*stage {
                                        AuthStage::ApiKey { provider } => provider.clone(),
                                        _ => unreachable!(),
                                    };
                                    app.pending_key = None;
                                    app.editor.mask = false;
                                    let selected = crate::core::providers::registry::key_providers()
                                        .iter()
                                        .position(|p| p.name == provider)
                                        .unwrap_or(0);
                                    *stage = AuthStage::Key { selected };
                                }
                                (AuthStage::Waiting { back }, KeyCode::Backspace) => {
                                    let back = *back;
                                    let cancelled = app.cancel_login();
                                    match back {
                                        Some(selected) => {
                                            app.auth = Some(AuthStage::Account { selected });
                                            if cancelled {
                                                app.notice("login cancelled".into());
                                            }
                                        }
                                        // Launched by `/login <provider>`: no
                                        // list to return to, so close.
                                        None => {
                                            app.auth = None;
                                            if cancelled {
                                                app.notice("login cancelled".into());
                                            }
                                        }
                                    }
                                }
                                (AuthStage::Done { back, .. }, KeyCode::Enter | KeyCode::Backspace) => {
                                    *stage = back.stage();
                                }
                                (AuthStage::ApiKey { .. }, _) => {
                                    if let Some(key) = key_of(&k, &app.keymap) {
                                        if let EditorResult::Submit(text) = app.editor.key(key) {
                                            app.submit(text);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else if app
                            .menu
                            .as_ref()
                            .map(|m| m.kind == MenuKind::Scoped)
                            .unwrap_or(false)
                            && ((k.code == KeyCode::Char(' ') && !ctrl)
                                || (ctrl && k.code == KeyCode::Char('x')))
                        {
                            if ctrl {
                                match model::clear_scope() {
                                    Ok(()) => {
                                        app.notice("scope cleared — ctrl+p cycles every model again".into());
                                        app.open_scoped_menu();
                                    }
                                    Err(error) => app.notice(format!("could not save model scope: {error}")),
                                }
                            } else {
                                app.toggle_scoped();
                            }
                        } else if app.menu.is_some()
                            && (matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Esc)
                                || (k.code == KeyCode::Tab
                                    && !k.modifiers.contains(KeyModifiers::SHIFT)
                                    && app.menu.as_ref().unwrap().has_tabs()))
                            && !ctrl
                        {
                            match k.code {
                                KeyCode::Up => { app.menu.as_mut().unwrap().step(-1); }
                                KeyCode::Down => { app.menu.as_mut().unwrap().step(1); }
                                KeyCode::Tab => { app.menu.as_mut().unwrap().cycle_tab(); }
                                KeyCode::Enter => { app.select_menu(); }
                                KeyCode::Esc => {
                                    app.menu = None;
                                    // Declining the -r picker releases a
                                    // held launch prompt into the current
                                    // session.
                                    app.release_initial_prompt();
                                }
                                _ => {}
                            }
                        } else if k.code == KeyCode::Esc && app.pending_key.is_some() {
                            app.pending_key = None;
                            app.editor.mask = false;
                            app.editor.set_text("");
                            app.notice("login cancelled".into());
                        } else if k.code == KeyCode::Esc && app.agent.is_streaming() {
                            app.agent.interrupt();
                        } else if ctrl && k.code == KeyCode::Char('c') {
                            if app.agent.is_streaming() {
                                app.agent.interrupt();
                                arm(&mut app);
                            } else if !app.editor.is_empty() {
                                app.editor.set_text("");
                                arm(&mut app);
                            } else if app.armed_at.map(|t| t.elapsed() < Duration::from_millis(1500)).unwrap_or(false) {
                                break;
                            } else {
                                arm(&mut app);
                            }
                        } else if ctrl && matches!(k.code, KeyCode::Char('p') | KeyCode::Char('P')) {
                            let backward = k.code == KeyCode::Char('P')
                                || k.modifiers.contains(KeyModifiers::SHIFT);
                            app.cycle_model(!backward);
                        } else if ctrl && k.code == KeyCode::Char('d') && app.editor.is_empty() {
                            break;
                        } else if k.code == KeyCode::BackTab
                            || (k.code == KeyCode::Tab
                                && !ctrl
                                && k.modifiers.contains(KeyModifiers::SHIFT))
                        {
                            // Shift+tab cycles the reasoning effort through
                            // whatever levels this model declares. The
                            // statusline already shows the new level; only a
                            // model without a reasoning knob gets a notice.
                            match app.agent.cycle_effort() {
                                Ok(Some(_)) => app.refresh_status_cache(),
                                Ok(None) => app.notice("this model has no reasoning effort control".into()),
                                Err(error) => app.notice(format!("could not save reasoning effort: {error}")),
                            }
                        } else if !ctrl && app.queue_review_key(k.code) {
                            // Consumed by the queued-prompt review.
                        } else if let Some(key) = key_of(&k, &app.keymap) {
                            if let EditorResult::Submit(text) = app.editor.key(key) {
                                app.submit(text);
                            }
                            app.sync_menu();
                        }
                    }
                    _ => {}
                }
            }
            count = session_events.recv_many(&mut event_buf, 128) => {
                if count == 0 {
                    break;
                }
                // Apply the whole burst before building one frame — a fast
                // stream must not cost one paint per delta.
                for e in event_buf.drain(..) {
                    app.on_session_event(e);
                }
            }
            job = results_rx.recv() => {
                match job {
                    Some(AppJob::Notice(notice)) => app.notice(notice),
                    Some(AppJob::Prompt { text, epoch }) => {
                        // A prompt from a command that started in an earlier
                        // session must not start a turn in its replacement.
                        if epoch == app.session_epoch {
                            app.prompt(text);
                        } else {
                            app.notice(
                                "an extension command finished after the session changed — its prompt was discarded"
                                    .into(),
                            );
                        }
                    }
                    Some(AppJob::InputVerdict { sequence, text, images, verdict }) => {
                        // A later hook may finish first; hold it until every
                        // earlier submission has a verdict, then apply the
                        // contiguous ordered prefix.
                        for (text, images, verdict) in
                            app.input_verdicts.complete(sequence, text, images, verdict)
                        {
                            app.apply_input_verdict(text, images, verdict);
                        }
                    }
                    Some(AppJob::Rename { name, epoch }) => {
                        // A rename from a command that started in an earlier
                        // session must not rename its replacement.
                        if epoch == app.session_epoch {
                            app.agent.set_session_name(name.clone());
                            app.notice(format!("session: {name}"));
                            set_tab_title(&tab_title(&title_path(), Some(&name)));
                        }
                    }
                    Some(AppJob::Compacted { summary, kept, epoch }) => {
                        // Ignore a result that outlived its session (/new won).
                        if app.compaction_is_current(epoch) {
                            app.compacting = false;
                            if app.agent.load_compacted(&summary, kept).await {
                                app.context_tokens =
                                    crate::core::agent::compact::estimate_request_tokens(
                                        &system_prompt(),
                                        &app.agent.history_snapshot(),
                                    );
                                app.transcript.clear();
                                app.transcript.push(Block::new(Kind::Notice, "compacted — recent messages kept, the full session is under /resume"));
                                app.transcript.push(Block::new(Kind::Summary, summary));
                            }
                            for text in std::mem::take(&mut app.held_prompts) {
                                app.prompt(text);
                            }
                        }
                    }
                    Some(AppJob::CompactFailed { message, epoch })
                        if app.compaction_is_current(epoch) => {
                        app.compacting = false;
                        app.notice(format!("compact failed: {message}"));
                        for text in std::mem::take(&mut app.held_prompts) {
                            app.prompt(text);
                        }
                    }
                    Some(AppJob::CompactFailed { .. }) => {}
                    Some(AppJob::CatalogRefreshed) => {
                        if let Some(menu) = &app.menu {
                            if menu.kind == MenuKind::Models {
                                let selected =
                                    menu.current().map(|item| item.value.clone());
                                app.build_model_menu();
                                if let (Some(menu), Some(value)) =
                                    (&mut app.menu, selected)
                                {
                                    menu.select_value(&value);
                                }
                            }
                        }
                    }
                    Some(AppJob::Updated(version)) => {
                        app.notice(format!(
                            "e {version} installed — /reload to switch to it now"
                        ));
                        app.update_installed = Some(version);
                    }
                    Some(AppJob::Reloaded(host)) => {
                        app.reloading = false;
                        app.host = host.clone();
                        app.agent.set_host(host);
                        app.apply_theme();
                        app.apply_keymap();
                        app.refresh_status_cache();
                        finish_reload_notice(&mut app.transcript, app.reload_block.take());
                        for text in std::mem::take(&mut app.held_prompts) {
                            app.prompt(text);
                        }
                    }
                    Some(AppJob::Shell { cmd, output, epoch }) => {
                        // A result from a command started in an earlier
                        // session must not be recorded into this one.
                        if epoch != app.session_epoch {
                            app.notice(format!(
                                "`{cmd}` finished after the session changed — output discarded"
                            ));
                        } else {
                            // Display a trimmed tail in the live block;
                            // history gets the full (tool-truncated) output.
                            let display_output =
                                crate::core::tools::sanitize_display(&output.content);
                            let shown: String = {
                                let lines: Vec<&str> = display_output.lines().collect();
                                let tail = &lines[lines.len().saturating_sub(20)..];
                                let mut text = tail.join("\n");
                                if lines.len() > 20 {
                                    text = format!(
                                        "… ({} more lines above)\n{text}",
                                        lines.len() - 20
                                    );
                                }
                                text
                            };
                            if let Some(idx) = app.shell_block.take() {
                                if let Some(block) = app.transcript.blocks.get_mut(idx) {
                                    block.done = true;
                                    block.is_error = output.is_error();
                                    block.detail = Some(shown);
                                    block.touch();
                                }
                            }
                            if !output.content.trim().is_empty() {
                                app.remember_output(format!("$ {cmd}"), display_output);
                            }
                            app.agent.record_user(format!(
                                "I ran `{cmd}` in my shell. Output:\n```\n{}\n```",
                                output.content
                            ));
                            // Prompts held for the shell result submit now,
                            // ordered after it.
                            for text in std::mem::take(&mut app.held_prompts) {
                                app.prompt(text);
                            }
                        }
                    }
                    None => {}
                }
            }
            message = jobs_rx.recv() => {
                if let Some(message) = message {
                    app.notice(message);
                }
            }
            outcome = logins_rx.recv() => {
                // Control flow hangs off the typed outcome; the human-readable
                // notice arrives separately on `jobs`.
                match outcome {
                    Some(crate::core::auth::login::Outcome::SignedIn { flow_id, provider })
                        if app.login_outcome_is_current(flow_id) =>
                    {
                            app.login_task.take();
                            // Stay in the panel: show the outcome beat, then
                            // land back on the account list.
                            if let Some(AuthStage::Waiting { back }) = &app.auth {
                                let back = back.unwrap_or(0);
                                let display =
                                    crate::core::providers::catalog::display_name(&provider);
                                app.auth = Some(AuthStage::Done {
                                    ok: true,
                                    message: format!("{display} connected"),
                                    back: authpanel::BackTarget::Account(back),
                                });
                            }
                            tokio::spawn(crate::core::providers::catalog::refresh_remote());
                            // A fresh credential may make new models available:
                            // if the current model's provider is still signed out,
                            // fall back to the first available model.
                            if !crate::core::auth::signed_in(&crate::core::auth::load(), &app.agent.model.provider) {
                                if let Some(m) = crate::core::providers::catalog::available().into_iter().next() {
                                    app.notice(format!("model set to {}", crate::core::providers::catalog::slug(&m)));
                                    app.agent.model = m;
                                }
                            }
                            app.refresh_status_cache();
                        }
                    Some(crate::core::auth::login::Outcome::Failed { flow_id })
                        if app.login_outcome_is_current(Some(flow_id)) => {
                            app.login_task.take();
                            if let Some(AuthStage::Waiting { back }) = &app.auth {
                                let back = back.unwrap_or(0);
                                app.auth = Some(AuthStage::Done {
                                    ok: false,
                                    message:
                                        "sign-in did not complete — details in the notice below"
                                            .into(),
                                    back: authpanel::BackTarget::Account(back),
                                });
                            } else {
                                app.auth = None;
                            }
                        }
                    // A canceled flow can finish just before its task aborts.
                    // Its queued outcome must not affect the replacement flow.
                    Some(_) => {}
                    None => {}
                }
            }
            _ = sigterm.recv() => break,
            _ = sighup.recv() => break,
            // A paint was skipped inside the frame interval; fire it when
            // the interval lapses.
            _ = tokio::time::sleep_until(next_paint), if paint_deferred => {}
            _ = tick.tick() => {
                if let Some(at) = app.armed_at {
                    if at.elapsed() > Duration::from_millis(1600) {
                        app.armed_at = None;
                        app.overlay = None;
                    }
                }
                if let Some(s) = &mut app.active {
                    let expired = s.turn.recovered
                        .is_some_and(|r| r.since.elapsed() > Duration::from_millis(RECOVERED_VISIBLE_MS));
                    if expired {
                        s.turn.recovered = None;
                    }
                }
            }
        }
        let now = tokio::time::Instant::now();
        if now >= next_paint || app.should_quit {
            let frame = if app.viewer.is_some() {
                app.viewer_frame(cols as usize, rows as usize)
            } else {
                app.frame(cols as usize, rows as usize)
            };
            painter.frame(frame);
            next_paint = now + FRAME_INTERVAL;
            paint_deferred = false;
        } else {
            paint_deferred = true;
        }
        if app.should_quit {
            break;
        }
    }

    // Let the final frame land before the terminal is restored.
    painter.shutdown();
    app.host.shutdown().await;
    drop(_guard);
    // The tab title we set at launch (or from a session name) is ours to
    // clear — the reference leaves the terminal pristine on exit.
    set_tab_title("");
    if app.relaunch {
        // The terminal is restored and the host is down: replace this
        // process with the updated binary, continuing the same session.
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string();
        let args = vec!["-c".to_string()];
        if let Err(error) = relaunch_self(&cwd, &args, &std::collections::BTreeMap::new()) {
            eprintln!("relaunch failed: {error} — start e again by hand");
        }
    }
    Ok(())
}

fn arm(app: &mut App) {
    app.armed_at = Some(Instant::now());
    app.overlay = Some("press ctrl+c again to exit".into());
}

/// Restores every terminal mode the TUI enables — keyboard enhancement
/// flags, bracketed paste, raw mode, cursor visibility — on every exit
/// path, `?` returns and unwinds included. Popping a mode that never got
/// enabled is harmless; leaving one enabled corrupts the user's shell.
struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(
            std::io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableBracketedPaste
        );
        let _ = terminal::disable_raw_mode();
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = write!(out, "\r\n\x1b[?25h");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_title_shortens_to_two_components() {
        assert_eq!(
            title_path_from(
                std::path::Path::new("/Volumes/v0/workspaces/worktrees/e/bold-fox"),
                ""
            ),
            "e/bold-fox"
        );
        assert_eq!(title_path_from(std::path::Path::new("/etc"), ""), "etc");
        assert_eq!(title_path_from(std::path::Path::new("/"), ""), "/");
    }

    #[test]
    fn tab_title_is_home_relative_under_home() {
        assert_eq!(
            title_path_from(std::path::Path::new("/Users/fschr/code/x"), "/Users/fschr"),
            "~/code/x"
        );
        assert_eq!(
            title_path_from(std::path::Path::new("/Users/fschr"), "/Users/fschr"),
            "~"
        );
        assert_eq!(
            title_path_from(
                std::path::Path::new("/Users/fschr/code/a/b/c"),
                "/Users/fschr"
            ),
            "~/b/c"
        );
    }

    fn node(
        id: &str,
        parent: Option<&str>,
        message: crate::core::providers::ChatMessage,
    ) -> crate::core::session::Node {
        crate::core::session::Node {
            id: id.into(),
            parent: parent.map(String::from),
            message,
        }
    }

    #[test]
    fn tree_items_lists_user_turns_and_flags_branch_points() {
        use crate::core::providers::ChatMessage;
        let nodes = vec![
            node("1", None, ChatMessage::user("root question")),
            node(
                "2",
                Some("1"),
                ChatMessage::assistant("reply A", Vec::new()),
            ),
            // A second child of "1": root was rewound and branched from once.
            node("3", Some("1"), ChatMessage::user("second try")),
        ];
        let items = tree_items(&nodes);
        // Only user-role nodes are offered as rewind points.
        assert_eq!(items.len(), 2);
        let (id, preview, branched) = &items[0];
        assert_eq!(id, "1");
        assert_eq!(preview, "root question");
        assert!(!branched, "the root itself has no parent to branch under");
        let (id, preview, branched) = &items[1];
        assert_eq!(id, "3");
        assert_eq!(preview, "second try");
        assert!(*branched, "\"1\" now has two children — a branch point");
    }

    #[test]
    fn rewind_target_replays_ancestors_and_restores_the_chosen_prompt() {
        use crate::core::providers::ChatMessage;
        let nodes = vec![
            node("1", None, ChatMessage::user("first")),
            node("2", Some("1"), ChatMessage::assistant("reply", Vec::new())),
            node("3", Some("2"), ChatMessage::user("second\nwith details")),
        ];
        let (head, messages, prompt) = rewind_target(&nodes, "3").expect("node 3 exists");
        assert_eq!(head.as_deref(), Some("2"), "rewinds to just before node 3");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "first");
        assert_eq!(messages[1].content, "reply");
        assert_eq!(prompt, "second\nwith details");
    }

    #[test]
    fn rewind_target_to_the_root_yields_an_empty_history_and_no_head() {
        use crate::core::providers::ChatMessage;
        let nodes = vec![node("1", None, ChatMessage::user("only message"))];
        let (head, messages, prompt) = rewind_target(&nodes, "1").expect("node 1 exists");
        assert!(head.is_none());
        assert!(messages.is_empty());
        assert_eq!(prompt, "only message");
    }

    #[test]
    fn rewind_target_is_none_for_an_unknown_id() {
        use crate::core::providers::ChatMessage;
        let nodes = vec![node("1", None, ChatMessage::user("only message"))];
        assert!(rewind_target(&nodes, "missing").is_none());
    }

    // E_HOME is process-global; serialize the tests below that set it.
    static KEY_OF_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn question_panel_leaves_global_interrupt_and_exit_keys_unhandled() {
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        let plain_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(!question_owns_key(&ctrl_c));
        assert!(!question_owns_key(&ctrl_d));
        assert!(question_owns_key(&plain_c));
    }

    #[test]
    fn key_of_matches_the_built_in_bindings_when_the_keymap_is_empty() {
        let keymap = crate::core::config::keybindings::Keymap::empty();
        let ctrl_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(matches!(key_of(&ctrl_w, &keymap), Some(Key::KillWord)));
        let plain_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(matches!(key_of(&plain_x, &keymap), Some(Key::Char('x'))));
    }

    #[test]
    fn key_of_consults_an_override_before_the_built_in_binding() {
        let _guard = KEY_OF_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "e-key-of-override-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("E_HOME", &dir);
        std::fs::write(dir.join("keybindings.json"), r#"{"ctrl+w": "home"}"#).unwrap();

        let keymap = crate::core::config::keybindings::load();
        let ctrl_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(
            matches!(key_of(&ctrl_w, &keymap), Some(Key::Home)),
            "an override replaces the built-in action for that chord"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_of_none_override_swallows_a_built_in_chord() {
        let _guard = KEY_OF_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "e-key-of-none-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("E_HOME", &dir);
        // ctrl+j is a built-in binding for Newline; "none" must swallow it
        // rather than falling through to the default.
        std::fs::write(dir.join("keybindings.json"), r#"{"ctrl+j": "none"}"#).unwrap();

        let keymap = crate::core::config::keybindings::load();
        let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert!(key_of(&ctrl_j, &keymap).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_result_replaces_the_in_progress_notice() {
        let mut transcript = Transcript::default();
        let reload_block = transcript.push(Block::new(Kind::Notice, "reloading…"));
        transcript.push(Block::new(Kind::Notice, "an unrelated notice"));

        finish_reload_notice(&mut transcript, Some(reload_block));

        assert_eq!(transcript.blocks.len(), 2);
        assert_eq!(
            transcript.blocks[reload_block].text,
            "reloaded extensions, themes, and config — skills, prompts, and AGENTS.md are always read fresh"
        );
        assert_eq!(transcript.blocks[1].text, "an unrelated notice");
    }

    #[test]
    fn initial_prompt_waits_for_first_visit_trust() {
        let mut pending = None;
        let now = stage_initial_prompt("inspect this repo".into(), true, &mut pending);
        assert!(now.is_none());
        assert_eq!(pending.as_deref(), Some("inspect this repo"));

        let mut pending = None;
        let now = stage_initial_prompt("inspect this repo".into(), false, &mut pending);
        assert_eq!(now.as_deref(), Some("inspect this repo"));
        assert!(pending.is_none());
    }

    #[test]
    fn api_keys_bypass_input_hooks() {
        assert_eq!(input_route(true, true), InputRoute::ApiKey);
        assert_eq!(input_route(true, false), InputRoute::ApiKey);
        assert_eq!(input_route(false, true), InputRoute::Hook);
        assert_eq!(input_route(false, false), InputRoute::Direct);
    }

    #[test]
    fn tab_title_prefers_the_session_name_over_the_path() {
        assert_eq!(tab_title("~/work", None), "𝑒 · ~/work");
        assert_eq!(
            tab_title("~/work", Some("fix the renderer")),
            "𝑒 · fix the renderer"
        );
        // A blank name falls back to the path, never an empty title.
        assert_eq!(tab_title("~/work", Some("   ")), "𝑒 · ~/work");
    }

    #[test]
    fn input_hook_verdicts_apply_in_submission_order() {
        let mut pending = PendingInputVerdicts::default();
        let first = pending.reserve();
        let second = pending.reserve();

        let later = pending.complete(
            second,
            "second".into(),
            None,
            crate::core::api::InputVerdict::default(),
        );
        assert!(later.is_empty(), "a later verdict must wait");

        let ordered = pending.complete(
            first,
            "first".into(),
            None,
            crate::core::api::InputVerdict::default(),
        );
        assert_eq!(
            ordered
                .into_iter()
                .map(|(text, _, _)| text)
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    /// The initial launch prompt (`-i image.png "..."`) carries images
    /// through the same hook-ordering machinery as plain text so an input
    /// hook still sees it (the fix for the reported hook bypass) — this
    /// pins that the images stay attached to the right sequence entry, not
    /// dropped or swapped, once a hook actually sits in front of it.
    #[test]
    fn input_hook_verdicts_carry_images_with_the_right_sequence_entry() {
        let mut pending = PendingInputVerdicts::default();
        let text_only = pending.reserve();
        let with_images = pending.reserve();

        let image = crate::core::providers::ImageInput {
            media_type: "image/png".into(),
            data: std::sync::Arc::from(""),
        };

        // Completed out of order: the images-bearing one first.
        let none_ready = pending.complete(
            with_images,
            "with images".into(),
            Some(vec![image]),
            crate::core::api::InputVerdict::default(),
        );
        assert!(none_ready.is_empty(), "text_only hasn't completed yet");

        let ordered = pending.complete(
            text_only,
            "text only".into(),
            None,
            crate::core::api::InputVerdict::default(),
        );
        assert_eq!(ordered.len(), 2);
        let (first_text, first_images, _) = &ordered[0];
        assert_eq!(first_text, "text only");
        assert!(first_images.is_none());
        let (second_text, second_images, _) = &ordered[1];
        assert_eq!(second_text, "with images");
        assert_eq!(
            second_images.as_ref().map(Vec::len),
            Some(1),
            "the image must still be attached to its own text, not lost or moved"
        );
    }

    #[tokio::test]
    async fn active_login_guard_cancels_on_drop() {
        let cancellation = crate::core::auth::login::LoginCancellation::default();
        let observed = cancellation.clone();
        let task = tokio::spawn(std::future::pending());
        let login = ActiveLogin {
            flow_id: 1,
            cancellation,
            task,
            wait_for_callback: false,
        };

        drop(login);
        assert!(observed.is_cancelled());
        tokio::task::yield_now().await;
    }

    fn session_app() -> App {
        let (agent, _rx) = Agent::new(Model {
            provider: "mock".into(),
            id: "m".into(),
            base_url: "http://localhost".into(),
            api: crate::core::providers::catalog::Api::Completions,
            catalog: crate::core::providers::registry::CatalogStrategy::Openai,
            responses_mount: crate::core::providers::registry::ResponsesMount::Platform,
            provider_supports_tools: true,
            provider_image_input: false,
            efforts: Vec::new(),
            thinking: crate::core::providers::catalog::Thinking::Manual,
            context_window: 200_000,
            max_output: None,
            supports_tools: true,
            image_input: false,
            pricing: None,
        });
        let (jobs, _) = tokio::sync::mpsc::channel(1);
        let (logins, _) = tokio::sync::mpsc::channel(1);
        let (results, _) = tokio::sync::mpsc::channel(1);
        App {
            theme: crate::tui::theme::load_bundled(false).unwrap(),
            keymap: crate::core::config::keybindings::Keymap::empty(),
            transcript: Transcript::default(),
            editor: Editor::new(),
            agent,
            active: None,
            overlay: None,
            armed_at: None,
            should_quit: false,
            context_tokens: 0,
            pending_key: None,
            menu: None,
            auth: None,
            settings: None,
            show_thinking: true,
            jobs,
            logins,
            login_task: None,
            login_sequence: 0,
            host: crate::core::api::ExtensionHost::empty(),
            results,
            input_verdicts: PendingInputVerdicts::default(),
            compacting: false,
            compaction_epoch: 0,
            compact_requested: false,
            held_prompts: Vec::new(),
            trust: None,
            pending_initial: None,
            pending_initial_images: Vec::new(),
            shell_block: None,
            reloading: false,
            reload_block: None,
            outputs: Vec::new(),
            output_seq: 0,
            viewer: None,
            viewer_cache: None,
            question: None,
            question_queue: std::collections::VecDeque::new(),
            queue_review: None,
            session_epoch: 0,
            update_installed: None,
            relaunch: false,
            light_background: false,
            signed_in: false,
            status_effort: None,
        }
    }

    #[test]
    fn queue_review_ignores_an_empty_synchronized_snapshot() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);

        assert!(!app.queue_review_key(KeyCode::Up));
        assert!(app.queue_review.is_none());
        assert!(app.editor.is_empty());
    }

    #[test]
    fn queue_review_commit_leaves_untouched_entries_verbatim() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.queue_review = Some(QueueReview {
            // The untouched entry carries a trailing newline a blanket trim
            // would silently strip; only the edited entry may rewrite.
            entries: vec!["keep  me\n".into(), "old draft".into()],
            dirty: vec![false, true],
            selected: 1,
            visible: true,
        });
        app.editor.set_text("  new draft  ");

        assert!(app.queue_review_key(KeyCode::Enter));

        let entries = app.agent.pause_queue();
        assert_eq!(
            entries,
            vec!["keep  me\n".to_string(), "new draft".to_string()]
        );
        app.agent.resume_queue();
    }

    #[test]
    fn queue_review_commit_drops_an_entry_edited_to_empty() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.queue_review = Some(QueueReview {
            entries: vec!["first".into(), "second".into()],
            dirty: vec![false, true],
            selected: 1,
            visible: true,
        });
        app.editor.set_text("   ");

        assert!(app.queue_review_key(KeyCode::Enter));

        let entries = app.agent.pause_queue();
        assert_eq!(entries, vec!["first".to_string()]);
        app.agent.resume_queue();
    }

    #[test]
    fn review_screen_rebuilds_after_transcript_changes() {
        let mut app = session_app();
        app.viewer = Some(Viewer {
            full: false,
            scroll: 0,
        });
        app.transcript
            .push(Block::new(Kind::User, "before the change"));
        let before = app.viewer_rows(80, false).to_vec();

        // Same width and depth, new block: the cache must notice.
        app.transcript
            .push(Block::new(Kind::User, "after the change"));
        let after = app.viewer_rows(80, false).to_vec();

        assert_eq!(after.len(), before.len() + 2, "block plus its gap row");
        assert!(after.last().unwrap().contains("after the change"));
    }

    #[test]
    fn review_screen_rebuilds_when_a_tool_reports() {
        let mut app = session_app();
        app.viewer = Some(Viewer {
            full: false,
            scroll: 0,
        });
        let child = crate::tui::transcript::ToolChild::pending(
            7,
            "command".into(),
            "Running true".into(),
            "Ran true".into(),
            "true".into(),
        );
        let block = app.transcript.push(Block::tool_group(vec![child]));
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::ToolStart { id: 7 });
        let running = app.viewer_rows(80, false).to_vec();

        app.active.as_mut().unwrap().tool_blocks.insert(7, block);
        app.on_session_event(SessionEvent::ToolEnd {
            id: 7,
            outcome: crate::core::tools::ToolOutcome::Completed,
            summary: "done".into(),
            content: "full saved output".into(),
        });
        let reported = app.viewer_rows(80, false).to_vec();

        assert!(
            reported.iter().any(|row| row.contains("full saved output")),
            "the new detail must appear without a width or depth change"
        );
        assert_ne!(running, reported);
    }

    #[test]
    fn review_screen_clamps_scroll_when_the_body_shrinks() {
        let mut app = session_app();
        app.transcript.push(Block::new(Kind::User, "some content"));
        app.viewer = Some(Viewer {
            full: false,
            scroll: 500,
        });

        let frame = app.viewer_frame(80, 24);

        let body = &frame[..frame.len() - 1];
        assert!(
            body.iter().any(|r| r.contains("some content")),
            "a deep scroll must clamp to the shrunken body: {frame:?}"
        );
        assert_eq!(
            app.viewer.as_ref().unwrap().scroll,
            0,
            "the clamp persists so ↑/↓ arithmetic starts in range"
        );
    }

    #[test]
    fn queued_banner_offers_edit_only_for_editable_prompts() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        // A held prompt (compaction, a `!` command) cannot be pulled into
        // the composer — only queued steering prompts can.
        app.held_prompts = vec!["held".into()];

        let lines = app.frame(80, 24);

        let banner = lines
            .iter()
            .find(|l| l.contains("queued message"))
            .expect("the banner names the held prompt");
        assert!(!banner.contains("↑ to edit"), "{banner:?}");
    }

    #[test]
    fn cancelled_turn_discards_the_reviewed_prompt_from_composer() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.queue_review = Some(QueueReview {
            entries: vec!["original".into()],
            dirty: vec![false],
            selected: 0,
            visible: true,
        });
        app.editor.set_text("edited draft");

        app.on_session_event(SessionEvent::TurnEnd { aborted: true });

        assert!(app.queue_review.is_none());
        assert!(app.editor.is_empty());
    }

    fn thinking_flags(app: &App) -> Vec<(String, bool)> {
        app.transcript
            .blocks
            .iter()
            .filter(|block| block.kind == Kind::Thinking)
            .map(|block| (block.text.clone(), block.done))
            .collect()
    }

    #[test]
    fn help_picker_filters_without_a_slash_trigger() {
        let mut app = session_app();
        app.submit_direct("/help".into());

        app.editor.set_text("res");
        app.sync_menu();
        let menu = app
            .menu
            .as_ref()
            .expect("help picker stays open while typing");
        assert_eq!(
            menu.current().map(|item| item.value.as_str()),
            Some("/resume")
        );

        app.editor.set_text("vers");
        app.sync_menu();
        let menu = app
            .menu
            .as_ref()
            .expect("help picker stays open after paste");
        assert_eq!(
            menu.current().map(|item| item.value.as_str()),
            Some("/version")
        );
    }

    #[test]
    fn transcript_rebuild_discards_previous_output_details() {
        let mut app = session_app();
        app.remember_output("old session".into(), "old detail".into());
        app.rebuild_transcript(&[]);
        assert!(app.outputs.is_empty());
    }

    #[test]
    fn stale_compaction_generation_cannot_replace_current_history() {
        let mut app = session_app();
        app.compacting = true;
        app.compaction_epoch = 4;
        assert!(app.compaction_is_current(4));
        app.compaction_epoch += 1; // /new, /resume, or /tree invalidated it.
        assert!(!app.compaction_is_current(4));
    }

    fn tool_batch() -> SessionEvent {
        SessionEvent::ToolBatchStart {
            calls: vec![crate::core::agent::ToolCallPresentation {
                id: 1,
                category: "read".into(),
                running: "reading".into(),
                completed: "read".into(),
                target: "f.rs".into(),
            }],
        }
    }

    /// A typical think-then-tools turn opens a second thinking burst below
    /// the tools; each burst collapses to one row where it ends, and TurnEnd
    /// collapses the final one.
    #[test]
    fn thinking_bursts_collapse_at_each_handoff() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::ReasoningDelta("before tools".into()));
        assert_eq!(thinking_flags(&app), vec![("before tools".into(), false)]);

        app.on_session_event(tool_batch());
        assert_eq!(
            thinking_flags(&app),
            vec![("Thought for 0s".into(), true)],
            "the pre-tool burst collapses when tools take over"
        );

        app.on_session_event(SessionEvent::ReasoningDelta("after tools".into()));
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("Thought for 0s".into(), true),
                ("after tools".into(), false)
            ]
        );

        app.on_session_event(SessionEvent::TurnEnd { aborted: false });
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("Thought for 0s".into(), true),
                ("Thought for 0s".into(), true)
            ]
        );
    }

    /// Retries and steered messages also end the live burst; each collapsed
    /// row keeps its place in the transcript.
    #[test]
    fn thinking_collapses_at_retry_and_steer() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::ReasoningDelta("attempt one".into()));
        app.on_session_event(SessionEvent::Retry {
            attempt: 1,
            limit: 3,
            delay_secs: 1,
            cause: crate::core::providers::FailureCause::Network,
            reason: "timeout".into(),
        });
        assert_eq!(
            thinking_flags(&app),
            vec![("Thought for 0s".into(), true)],
            "the abandoned attempt's burst collapses at the retry"
        );

        app.on_session_event(SessionEvent::ReasoningDelta("attempt two".into()));
        app.on_session_event(SessionEvent::Steered("also check this".into()));
        app.on_session_event(SessionEvent::ReasoningDelta("after steer".into()));
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("Thought for 0s".into(), true),
                ("Thought for 0s".into(), true),
                ("after steer".into(), false)
            ]
        );

        app.on_session_event(SessionEvent::TurnEnd { aborted: false });
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("Thought for 0s".into(), true),
                ("Thought for 0s".into(), true),
                ("Thought for 0s".into(), true)
            ]
        );
    }
}
