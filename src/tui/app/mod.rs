//! The interactive frame: App state, key handling, and the paint loop.
//!
//! The binary (`main.rs`) owns CLI dispatch (`auth`, `ask`, `docs`, …) and
//! hands off here once a session should open.

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};
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
    statusline, RecoveredStatus, RetryStatus, StatusData, Turn, TurnPhase, RECOVERED_VISIBLE_MS,
};
use crate::tui::theme::Theme;
use crate::tui::transcript::{Block, Kind, Transcript};
use crate::tui::trustpanel::{self, TrustStage};

mod clipboard;

mod events;
mod extui;
mod login;
mod menus;
mod viewer;
use viewer::Viewer;

/// Per-turn frontend bookkeeping; the engine state lives in the Agent.
struct ActiveTurn {
    /// The current assistant text block, if one is streaming.
    block: Option<usize>,
    /// The live thinking block for the current burst, if reasoning has
    /// streamed. Ending a burst detaches this so the next reasoning opens a
    /// fresh block; the finished thought stays expanded in place — this index
    /// is only the open segment.
    thinking_block: Option<usize>,
    turn: Turn,
    started: Instant,
    error: Option<String>,
    error_summary: Option<String>,
    /// tool id → stable group block, so lifecycle events update in place.
    tool_blocks: std::collections::HashMap<u64, usize>,
    /// Batch members not yet terminal, including pending calls.
    pending_tools: usize,
    /// Set when the turn was stopped because the device slept past the
    /// resume window: the stop line is already in the transcript, so the
    /// cancelled row is suppressed at TurnEnd.
    sleep_stopped: bool,
    /// Accumulated provider-billed estimate for this turn, when the model
    /// declares rates. Unlike the context gauge, every request step counts.
    cost_usd: Option<f64>,
}

/// The queued-prompt review's working state: a keyed snapshot of the
/// queue (oldest first), which entry the composer holds, and whether a
/// draft is showing at all (↓ past the newest hides it). The turn keeps
/// steering meanwhile — an entry it already drained commits as a fresh
/// prompt instead of resurrecting the sent text.
struct QueueReview {
    entries: Vec<(u64, String)>,
    dirty: Vec<bool>,
    selected: usize,
    visible: bool,
}

/// Asynchronous work landing back in the frame loop.
enum AppJob {
    /// An input hook's verdict on a submitted line: consume/replace/notice.
    /// Images from `-i` or the composer clipboard ride through the text hook;
    /// the hook never sees their bytes, but they still attach to whatever text
    /// its verdict submits.
    InputVerdict {
        sequence: u64,
        text: String,
        images: Option<Vec<crate::core::providers::ImageInput>>,
        verdict: crate::core::extensions::InputVerdict,
    },
    /// A finished `!` shell command: what ran and what it printed. Tagged
    /// with the session epoch it started in.
    Shell {
        cmd: String,
        output: crate::core::tools::ToolOutput,
        epoch: u64,
    },
    /// An extension command or shortcut finished. Tagged with the session
    /// epoch it started in.
    Command {
        result: crate::core::extensions::CommandResult,
        epoch: u64,
    },
    /// Argument completions for `/command prefix` arrived; shown only if the
    /// composer still says exactly that.
    Completions {
        command: String,
        prefix: String,
        items: Vec<crate::core::extensions::Completion>,
    },
    /// A /reload finished: the restarted extension host.
    Reloaded(std::sync::Arc<crate::core::extensions::ExtensionHost>),
    /// The background updater installed a new version.
    Updated(String),
    /// Clipboard content read asynchronously, tied to the draft that requested it.
    ClipboardPaste {
        generation: u64,
        paste: Result<clipboard::Paste, String>,
        /// The pasted text to restore when a path attachment cannot load —
        /// a clipboard read has nothing to restore, a paste does.
        fallback: Option<String>,
    },
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
    crate::core::extensions::InputVerdict,
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
        verdict: crate::core::extensions::InputVerdict,
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
    /// Images attached to the current composer draft by ctrl+v.
    composer_images: Vec<crate::core::providers::ImageInput>,
    /// Invalidates a clipboard read when its draft was submitted or cleared.
    composer_generation: u64,
    /// One clipboard read at a time; cleared when its result lands.
    clipboard_reading: bool,
    /// Enter pressed during a clipboard read submits after its payload lands.
    clipboard_submit_pending: bool,
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
    /// The scoped-models picker's staged scope: what Space has toggled but
    /// Ctrl+S has not yet committed. None when not staging (the picker shows
    /// the saved scope); Some when the picker is open with edits pending.
    staged_scope: Option<Vec<String>>,
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
    host: std::sync::Arc<crate::core::extensions::ExtensionHost>,
    results: tokio::sync::mpsc::Sender<AppJob>,
    /// Completed input-hook calls waiting for earlier submissions to finish.
    input_verdicts: PendingInputVerdicts,
    /// A /compact summary is being generated; cleared when it lands or fails.
    compacting: bool,
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
    /// The queued-prompt review: ↑ on an empty composer while prompts wait
    /// loads the newest into the composer for editing; the turn keeps
    /// steering while the review edits the queue.
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
    /// The latest frame has waited on the paint thread long enough to make
    /// terminal output, rather than provider work, the current bottleneck.
    rendering_delayed: bool,
    /// Dedupe one paint-failure episode while retries keep posting frames.
    last_paint_failure: Option<String>,
    /// OSC-11 background detection, probed once at startup before the
    /// event stream owns stdin. Re-probing mid-session would block the
    /// loop and swallow keystrokes, so a changed terminal background
    /// applies on restart.
    light_background: bool,
    /// Cached statusline inputs. Deriving them reads `~/.e/auth.json` and
    /// `~/.e/settings.json`; doing that per frame stalls streaming, so
    /// they refresh only via `refresh_status_cache`.
    bottom_pinned: bool,
    live_preview_rows: usize,
    tool_label_rows: usize,
    signed_in: bool,
    status_effort: Option<String>,
    /// Where extensions' `ui.*` / `session.*` requests arrive; handed to
    /// every host this session starts (launch, /reload).
    requests: tokio::sync::mpsc::Sender<crate::core::extensions::HostRequest>,
    /// Modal requests waiting for the footer to be free, first-come.
    ui_queue: extui::UiQueue,
    /// The open modal request, if any.
    ui_prompt: Option<extui::UiPrompt>,
    /// Each extension's `ui.status` text, by extension name.
    ext_status: std::collections::BTreeMap<String, String>,
    /// The extension panel below the composer, one slot.
    ext_panel: Option<extui::ExtPanel>,
    /// ctrl+g was pressed: the frame loop hands the terminal to the
    /// external editor before its next select.
    external_edit: bool,
}

impl App {
    /// Color a detail-viewer row shaped like a diff row: the number-and-sign
    /// column takes the diff-marker hue (`+` green, `-` red), context and
    /// `⋯` elision rows dim, anything else passes through untouched — the
    /// reference keeps the changed text itself neutral.
    fn diff_row_color(theme: &Theme, line: &str) -> Option<String> {
        crate::tui::transcript::diff_row_style(theme, line)
    }

    /// Ordinary chat joins the transcript and composer into one frame.
    fn frame(&mut self, width: usize, height: usize) -> Vec<String> {
        self.pump_ui_queue();
        let mut lines = self.transcript_frame(width);
        let dock_start = lines.len();
        lines.extend(self.composer_frame(width, height));
        if self.bottom_pinned && lines.len() < height {
            lines.splice(
                dock_start..dock_start,
                vec![String::new(); height - lines.len()],
            );
        }
        lines
    }

    /// Transcript and live activity only, without editor or footer chrome.
    fn transcript_frame(&mut self, width: usize) -> Vec<String> {
        let blink_on = self
            .active
            .as_ref()
            .map(|turn| (turn.started.elapsed().as_millis() / 500) % 2 == 0)
            .unwrap_or(true);
        let mut lines = self
            .transcript
            .render_animated(&self.theme, width, blink_on);
        let dock_start = lines.len();
        if let Some(s) = &self.active {
            if self.rendering_delayed {
                lines.push(String::new());
                let dot = if blink_on { "•" } else { " " };
                lines.push(
                    self.theme
                        .fg("warning", &format!("{dot} Rendering delayed")),
                );
            } else if let Some(label) = s.turn.label(s.started.elapsed().as_secs()) {
                lines.push(String::new());
                if s.turn.recovered.is_some() {
                    // A brief, non-blinking confirmation — not an ongoing
                    // wait, so no dot animation.
                    lines.push(self.theme.fg("success", &format!("✓ {label}")));
                } else if s.turn.phase == TurnPhase::Retrying {
                    // Keep the activity row's blinking dot, toned as a
                    // warning so a struggling provider
                    // reads distinctly from ordinary thinking.
                    let dot = if blink_on { "•" } else { " " };
                    lines.push(self.theme.fg("warning", &format!("{dot} {label}")));
                } else if matches!(
                    s.turn.phase,
                    TurnPhase::Waiting
                        | TurnPhase::Thinking
                        | TurnPhase::ToolCall
                        | TurnPhase::Tool
                        | TurnPhase::AssistantText
                ) {
                    // The activity dot runs on the same column as the user
                    // rail. Once reply text is visible, the answer itself
                    // carries the turn and the label sits where the dot was.
                    if s.turn.phase == TurnPhase::AssistantText {
                        lines.push(self.theme.fg("dim", &label));
                    } else {
                        let dot = if blink_on {
                            self.theme.fg("accent", "•")
                        } else {
                            " ".to_string()
                        };
                        lines.push(format!("{dot} {}", self.theme.fg("dim", &label)));
                    }
                } else {
                    lines.push(label);
                }
            }
        }
        if self.active.is_some() {
            lines.resize(lines.len().max(dock_start + 2), String::new());
        }
        lines
    }

    /// One full-width editor and status band, shared beneath both review panes.
    fn composer_frame(&mut self, width: usize, height: usize) -> Vec<String> {
        let mut lines = Vec::new();

        let entering_key = matches!(self.auth, Some(AuthStage::ApiKey { .. }));
        if !entering_key {
            // The reference caps the composer at half the frame plus one
            // row; a longer draft scrolls behind the ┃↑ marker.
            let cap = (height / 2 + 1).max(3);
            let mut composer = self.editor.render(&self.theme, width, cap);
            if !self.composer_images.is_empty() {
                // Attachment labels are chrome, not editable prompt text.
                // The existing dim token is the palette's light gray.
                lines.push(
                    self.theme
                        .fg("dim", &image_labels(self.composer_images.len())),
                );
            }

            // The queued banner band: the collapsed summary (ink-bright),
            // the review's hint line while it edits the queue, a gap row —
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
                        "reviewing the queue · enter to apply"
                    } else if self.editor.is_empty() {
                        "delete again to remove the queued prompt · enter to send unchanged"
                    } else {
                        "enter to apply the edit"
                    };
                    lines.push(self.theme.fg("dim", hint));
                }
                lines.push(String::new());
                composer[0] = self.theme.fg("border", &"─".repeat(width));
            }
            lines.extend(composer);
        }
        if let Some(stage) = &mut self.trust {
            let dir = self.agent.cwd().to_string_lossy().into_owned();
            lines.extend(trustpanel::render_view(
                stage,
                &self.theme,
                width,
                height.saturating_sub(1),
                &dir,
            ));
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
        } else if self.ui_input_open() {
            lines.extend(self.render_ui_input(width));
        } else if let Some(panel) = &self.ext_panel {
            lines.extend(panel.render(&self.theme, width));
        }
        let data = self.status_data();
        let ext_panel_hint = self.ext_panel.as_ref().map(|p| {
            if p.interactive {
                extui::HINT_PANEL_INTERACTIVE
            } else {
                extui::HINT_PANEL
            }
        });
        let hint = self
            .settings
            .as_ref()
            .map(|_| crate::tui::settingspanel::HINT)
            .or_else(|| self.menu.as_ref().map(|m| m.hint))
            .or_else(|| self.ui_input_open().then_some(extui::HINT_INPUT))
            .or(ext_panel_hint)
            .map(|h| crate::tui::menu::degrade_hint(h, width));
        // A framed surface's bottom divider sits directly above the hint
        // row — the blank spacer belongs only to the bare-composer layout.
        let panel_open = self.trust.is_some()
            || self.auth.is_some()
            || self.settings.is_some()
            || self.menu.is_some()
            || self.ui_input_open()
            || self.ext_panel.is_some();
        // Extensions' status slots share the right-hand overlay spot; a
        // transient app overlay (copied, clipboard) takes precedence while shown.
        let ext_status = (!self.ext_status.is_empty()).then(|| {
            self.ext_status
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .join(" · ")
        });
        let overlay = self
            .overlay
            .as_deref()
            .or(self.clipboard_reading.then_some("reading clipboard…"))
            .or(ext_status.as_deref());
        let footer = statusline(&self.theme, &data, overlay, hint, panel_open, width);
        lines.extend(footer);

        lines
    }

    /// Shared model and context inputs for the composer and transcript footers.
    fn status_data(&self) -> StatusData {
        let window = self.agent.model.context_window.max(1);
        // Nothing is signed in for the current model — it's a bootstrap
        // placeholder, not something the user chose, so don't show it.
        StatusData {
            model: self.signed_in.then(|| self.agent.model_slug()),
            effort: if self.signed_in {
                self.status_effort.clone()
            } else {
                None
            },
            context_used: self.context_tokens,
            context_total: Some(window),
        }
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
                if scoped && pool.is_empty() {
                    "no scoped models are currently available; your saved scope is unchanged"
                } else if scoped {
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
        self.emit(
            "model_change",
            serde_json::json!({"model": self.agent.model_slug()}),
        );
    }

    /// Queued-prompt review keys, the reference's grammar: ↑ on an empty
    /// composer while prompts wait opens the newest for editing (queue
    /// draining pauses); ↑/↓ step older/newer, ↓ past the newest hides the
    /// draft; Enter commits edits back to the queue and resumes — an empty
    /// draft leaves its entry unchanged; Backspace on an emptied draft
    /// deletes the entry. Returns true when the key was consumed.
    fn queue_review_key(&mut self, code: KeyCode) -> bool {
        if self.trust.is_some()
            || self.auth.is_some()
            || self.settings.is_some()
            || self.menu.is_some()
        {
            return false;
        }
        let Some(mut review) = self.queue_review.take() else {
            if code == KeyCode::Up && self.editor.is_empty() && self.active.is_some() {
                let entries = self.agent.queue_snapshot();
                let Some(selected) = entries.len().checked_sub(1) else {
                    return false;
                };
                let dirty = vec![false; entries.len()];
                self.editor.set_text(&entries[selected].1);
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
            if review.entries[review.selected].1 != text {
                review.entries[review.selected].1 = text;
                review.dirty[review.selected] = true;
            }
        };
        let consumed = match code {
            KeyCode::Up => {
                if review.visible {
                    stash(&mut review, self.editor.expanded_text());
                    if review.selected > 0 {
                        review.selected -= 1;
                        self.editor.set_text(&review.entries[review.selected].1);
                    }
                    true
                } else if self.editor.is_empty() {
                    review.visible = true;
                    self.editor.set_text(&review.entries[review.selected].1);
                    true
                } else {
                    false
                }
            }
            KeyCode::Down if review.visible => {
                stash(&mut review, self.editor.expanded_text());
                if review.selected + 1 < review.entries.len() {
                    review.selected += 1;
                    self.editor.set_text(&review.entries[review.selected].1);
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
                    stash(&mut review, self.editor.expanded_text());
                }
                if review.dirty.iter().any(|d| *d) {
                    // Only edited entries rewrite: a trim drops an entry that
                    // emptied, and leaves untouched entries verbatim — a
                    // multi-line prompt's trailing newline is not the user's
                    // doing. An edit to an entry the turn already drained
                    // lands as a fresh prompt, not a resurrection.
                    let mut edits = Vec::new();
                    let mut removed = Vec::new();
                    for ((key, entry), dirty) in review.entries.iter().zip(&review.dirty) {
                        if !dirty {
                            continue;
                        }
                        match entry.trim() {
                            "" => removed.push(*key),
                            trimmed => edits.push((*key, trimmed.to_string())),
                        }
                    }
                    self.agent.update_queued(edits, removed);
                }
                self.editor.set_text("");
                return true;
            }
            KeyCode::Backspace if review.visible && self.editor.is_empty() => {
                let (key, _) = review.entries.remove(review.selected);
                review.dirty.remove(review.selected);
                self.agent.update_queued(Vec::new(), vec![key]);
                if review.entries.is_empty() {
                    return true;
                }
                review.selected = review.selected.min(review.entries.len() - 1);
                self.editor.set_text(&review.entries[review.selected].1);
                true
            }
            _ => false,
        };
        self.queue_review = Some(review);
        consumed
    }

    /// Close the review without committing the visible draft. The draft is
    /// discarded with the review; the queue was never paused, so there is
    /// nothing to resume.
    fn close_queue_review(&mut self) {
        if self.queue_review.take().is_some() {
            self.editor.set_text("");
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
            Menu::new(MenuKind::Sessions, "Sessions", HINT_SESSIONS, items)
                .without_trigger()
                .with_tabs(
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
            match m.role() {
                "user" => {
                    open_group = None;
                    let count = m.images().len();
                    let content = if count == 0 {
                        m.content.clone()
                    } else {
                        display_image_prompt(&m.content, count)
                    };
                    self.transcript
                        .push(Block::new(Kind::User, content).with_images(count));
                }
                "assistant" => {
                    if !m.content.trim().is_empty() {
                        open_group = None;
                        self.transcript
                            .push(Block::new(Kind::Assistant, m.content.clone()));
                    }
                    if !m.tool_calls().is_empty() {
                        let mut children = Vec::with_capacity(m.tool_calls().len());
                        let mut ids = Vec::with_capacity(m.tool_calls().len());
                        for call in m.tool_calls() {
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
                    let Some(call_id) = m.tool_call_id() else {
                        continue;
                    };
                    let Some(&(block, id)) = restored_calls.get(call_id) else {
                        continue;
                    };
                    let (outcome, summary) = m
                        .tool_meta()
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
                block.tool_label_rows = self.tool_label_rows;
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
        // Ownership first: a session another e is appending to must not be
        // replayed into a second, diverging history.
        let session = match crate::core::session::SessionLog::reopen(&path) {
            Ok(s) => s,
            Err(e) => {
                self.notice(format!("could not resume session: {e}"));
                self.release_initial_prompt();
                return;
            }
        };
        let messages = match crate::core::session::SessionLog::load(&path) {
            Ok(m) => m,
            Err(e) => {
                self.notice(format!("could not open session: {e}"));
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
        self.discard_composer_images();
        self.rebuild_transcript(&messages);
        self.agent.load_history(messages);
        self.agent.set_session(Some(session));
        // Identity travels together: the resumed log's persisted name
        // replaces whatever the previous session was called.
        self.agent
            .adopt_session_name(crate::core::session::name_of(&path));
        self.session_epoch += 1;
        extui::shutdown_then_start(self, "resume");
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
        let nodes = match crate::core::session::SessionLog::nodes(&path) {
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
        self.menu = Some(Menu::new(MenuKind::Tree, "Rewind to", HINT_USE, items).without_trigger());
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
        let nodes = match crate::core::session::SessionLog::nodes(&path) {
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
        self.discard_composer_images();
        self.rebuild_transcript(&messages);
        self.agent.rewind_to(head, messages);
        self.editor.set_text(&prompt);
        self.session_epoch += 1;
        self.notice("edit or resend the restored prompt to branch".into());
    }

    fn open_settings(&mut self) {
        self.menu = None;
        self.settings = Some(crate::tui::settingspanel::SettingsPanel::new(
            self.agent.effort_levels(),
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

    /// Ctrl+C is global, including during trust and login. The first press
    /// cancels work and clears transient input; a second press exits without
    /// recording a trust decision or waiting for cancellation to finish.
    fn interrupt_or_exit(&mut self) {
        if self
            .armed_at
            .is_some_and(|at| at.elapsed() < Duration::from_millis(1500))
        {
            self.should_quit = true;
            return;
        }
        if self.agent.is_streaming() {
            self.agent.interrupt();
        }
        self.cancel_login();
        // An extension's modal is answered "cancelled", never left waiting
        // behind a surface that is gone.
        self.cancel_ui_prompt();
        self.close_ext_panel(true);
        self.auth = None;
        self.trust = None;
        self.queue_review = None;
        self.pending_initial = None;
        self.pending_initial_images.clear();
        self.pending_key = None;
        self.editor.mask = false;
        self.editor.set_text("");
        self.discard_composer_images();
        self.viewer = None;
        self.settings = None;
        self.menu = None;
        self.staged_scope = None;
        arm(self);
    }

    /// Insert text normally, or turn a pasted list of image paths into
    /// attachments — but only into a free composer: over an open surface a
    /// paste is plain text, so it cannot silently stack onto a draft the
    /// user is not looking at. Line endings normalise to `\n`: CRLF first,
    /// so a Windows clipboard does not double every line, then the bare CR
    /// some terminals send for a pasted newline.
    fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.composer_free() {
            let paths: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(String::from)
                .collect();
            let all_files = !paths.is_empty()
                && paths
                    .iter()
                    .all(|path| std::path::Path::new(path).is_file());
            if all_files && self.agent.model.image_input {
                // The reads run off the event loop — a slow or networked
                // file must not stall input and repaint. Stale results are
                // dropped by the draft generation, like a clipboard read;
                // a read that cannot attach restores the pasted text.
                let generation = self.composer_generation;
                let results = self.results.clone();
                let fallback = Some(text.clone());
                crate::core::config::home::spawn(async move {
                    let images = tokio::task::spawn_blocking(move || {
                        crate::core::providers::ImageInput::from_paths(&paths)
                    })
                    .await
                    .unwrap_or_else(|_| Err("image attachment reader panicked".into()));
                    let _ = results
                        .send(AppJob::ClipboardPaste {
                            generation,
                            paste: images.map(clipboard::Paste::Images),
                            fallback,
                        })
                        .await;
                });
                return;
            }
        }
        self.editor.insert_paste(&text);
        self.sync_menu();
    }

    /// True when nothing overlays the composer and a paste may attach to it.
    fn composer_free(&self) -> bool {
        self.viewer.is_none()
            && self.menu.is_none()
            && self.settings.is_none()
            && self.auth.is_none()
            && self.trust.is_none()
            && self.queue_review.is_none()
    }

    /// Forget attachments with a discarded or replaced composer draft.
    fn discard_composer_images(&mut self) {
        self.composer_images.clear();
        self.clipboard_submit_pending = false;
        self.composer_generation = self.composer_generation.wrapping_add(1);
    }

    /// Put a paste back into the composer when its images could not load —
    /// the text is the user's, whether or not it turned into attachments.
    fn restore_fallback(&mut self, fallback: Option<String>) {
        if let Some(text) = fallback {
            self.editor.insert_paste(&text);
            self.sync_menu();
        }
    }

    /// Start a bounded clipboard read without blocking terminal input. One
    /// read at a time — a second paste while one is in flight is declined
    /// rather than stacked, so a slow helper cannot accumulate waiters.
    fn paste_clipboard(&mut self) {
        if self.clipboard_reading {
            return;
        }
        self.clipboard_reading = true;
        let generation = self.composer_generation;
        let results = self.results.clone();
        crate::core::config::home::spawn(async move {
            let paste = tokio::task::spawn_blocking(clipboard::read)
                .await
                .unwrap_or_else(|_| Err("clipboard reader panicked".into()));
            let _ = results
                .send(AppJob::ClipboardPaste {
                    generation,
                    paste,
                    fallback: None,
                })
                .await;
        });
    }

    /// Apply a clipboard or pasted-path result to the current draft. Attachment
    /// state renders outside the editor so history can never contain fake labels.
    fn apply_clipboard_paste(
        &mut self,
        generation: u64,
        paste: Result<clipboard::Paste, String>,
        fallback: Option<String>,
    ) {
        // Only a direct clipboard job owns these flags; a path-paste read
        // never set them.
        let submit_after = if fallback.is_none() {
            self.clipboard_reading = false;
            std::mem::take(&mut self.clipboard_submit_pending)
        } else {
            false
        };
        if generation != self.composer_generation {
            // The old payload cannot enter a newer draft, but Enter may have
            // been pressed on that draft while this stale read still owned the
            // in-flight flag. Release the requested submission now.
            if submit_after {
                let text = self.editor.text();
                self.editor.set_text("");
                self.submit_composer(text);
            }
            return;
        }
        match paste {
            Ok(clipboard::Paste::Images(images)) if !images.is_empty() => {
                let mut batch = self.composer_images.clone();
                batch.extend(images.iter().cloned());
                if let Err(error) = crate::core::providers::ImageInput::validate_batch(&batch) {
                    self.notice(error);
                    self.restore_fallback(fallback);
                } else if self.agent.model.image_input {
                    self.composer_images.extend(images);
                    self.sync_menu();
                } else {
                    self.notice(format!(
                        "{} does not accept image input",
                        model::slug(&self.agent.model)
                    ));
                }
            }
            Ok(clipboard::Paste::Images(_)) => {}
            Ok(clipboard::Paste::Text(text)) => {
                self.editor.insert_paste(&text);
                self.sync_menu();
            }
            Err(error) => {
                self.notice(error);
                self.restore_fallback(fallback);
            }
        }
        if submit_after {
            let text = self.editor.text();
            self.editor.set_text("");
            self.submit_composer(text);
        }
    }

    /// Submit the visible draft with any clipboard images attached to it.
    fn submit_composer(&mut self, text: String) {
        if self.clipboard_reading {
            self.editor.set_text(&text);
            self.clipboard_submit_pending = true;
            return;
        }
        if self.composer_images.is_empty() {
            if !text.trim().is_empty() {
                self.composer_generation = self.composer_generation.wrapping_add(1);
            }
            self.submit(text);
            return;
        }
        // A command or shell line never carries images: route the text
        // through the normal dispatch and drop the attachments — they were
        // attached to a draft, and the dispatch owns what happens to it.
        let trimmed = text.trim();
        if trimmed.starts_with('/') || trimmed.starts_with('!') {
            self.discard_composer_images();
            self.notice("commands do not carry image attachments".into());
            self.submit(text);
            return;
        }
        if !self.agent.model.image_input {
            self.editor.set_text(&text);
            self.notice(format!(
                "{} does not accept image input",
                model::slug(&self.agent.model)
            ));
            return;
        }
        if self.agent.is_streaming()
            || self.compacting
            || self.reloading
            || self.shell_block.is_some()
        {
            self.editor.set_text(&text);
            self.notice("send image prompts between turns".into());
            return;
        }
        self.composer_generation = self.composer_generation.wrapping_add(1);
        let images = std::mem::take(&mut self.composer_images);
        self.submit_images(text, images);
    }

    /// Submit attached images through the same ordered input-hook path as text.
    fn submit_images(&mut self, text: String, images: Vec<crate::core::providers::ImageInput>) {
        let trimmed = text.trim();
        let prompt = if trimmed.is_empty() {
            "Describe this image.".to_string()
        } else {
            trimmed.to_string()
        };
        // History is recorded where the prompt is actually accepted — the
        // hook may consume or replace this text.
        if self.host.has_input_hook() {
            let host = self.host.clone();
            let results = self.results.clone();
            let sequence = self.input_verdicts.reserve();
            crate::core::config::home::spawn(async move {
                let verdict = host.hook_input(&prompt).await;
                let _ = results
                    .send(AppJob::InputVerdict {
                        sequence,
                        text: prompt,
                        images: Some(images),
                        verdict,
                    })
                    .await;
            });
        } else {
            self.submit_with_images(prompt, images);
        }
    }

    fn submit(&mut self, text: String) {
        let trimmed = text.trim().to_string();
        // An extension's question takes the line whole — even an empty one
        // is an answer — and it never reaches input hooks or the model.
        if self.ui_input_open() {
            self.answer_ui_input(&text);
            return;
        }
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
            crate::core::config::home::spawn(async move {
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
        verdict: crate::core::extensions::InputVerdict,
    ) {
        if let Some(notice) = verdict.notice.filter(|n| !n.trim().is_empty()) {
            self.notice(notice);
        }
        if verdict.consume {
            // Swallowed entirely. Any attached images are dropped with the
            // text because there is no accepted prompt left to carry them.
        } else if let Some(replace) = verdict.replace {
            // The extension rewrote the line; it already saw the original, so
            // no second hook pass.
            match images {
                Some(images) if !images.is_empty() => self.submit_with_images(replace, images),
                _ => self.submit_direct(replace),
            }
        } else {
            // Allowed through — the hook already saw the text, so submit
            // directly. Re-running submit() here would loop through the hook.
            match images {
                Some(images) if !images.is_empty() => self.submit_with_images(text, images),
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

        if let Some((path, prompt)) = leading_image_prompt(&trimmed) {
            // The successful branch records history in submit_with_images,
            // with the text that actually went to the model; these falls
            // record the original line.
            if !self.agent.model.image_input {
                // The image cannot ride along, but the question after the
                // path is still the user's prompt — discarding it and
                // stopping the turn would swallow the typed message along
                // with the attachment.
                if prompt.is_empty() {
                    self.remember_prompt(text);
                    self.notice(format!(
                        "{} does not accept image input",
                        model::slug(&self.agent.model)
                    ));
                } else {
                    self.remember_prompt(text);
                    self.notice(format!(
                        "{} does not accept image input — sending the text without the screenshot",
                        model::slug(&self.agent.model)
                    ));
                    self.prompt(prompt.to_string());
                }
                return;
            }
            match crate::core::providers::ImageInput::from_path(std::path::Path::new(path)) {
                Ok(image) => {
                    let prompt = if prompt.is_empty() {
                        "Describe this image.".to_string()
                    } else {
                        prompt.to_string()
                    };
                    self.submit_with_images(prompt, vec![image]);
                }
                Err(error) => {
                    self.remember_prompt(text);
                    self.notice(format!("could not attach image: {error}"));
                }
            }
            return;
        }

        self.remember_prompt(text);

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
        if let Some(rest) = command_arg(&trimmed, "/effort") {
            let requested = rest.trim();
            let levels = self.agent.effort_levels();
            if levels.is_empty() {
                self.notice("this model has no reasoning effort control".into());
            } else if requested.is_empty() {
                let current = self.agent.effort().unwrap_or_else(|| levels[0].clone());
                self.notice(format!(
                    "reasoning effort is {current} · available: {} · use /effort <level>",
                    levels.join(", ")
                ));
            } else if !levels.iter().any(|level| level == requested) {
                self.notice(format!(
                    "unsupported reasoning effort {requested:?} · available: {}",
                    levels.join(", ")
                ));
            } else {
                match self.agent.set_effort(requested) {
                    Ok(true) => {
                        self.refresh_status_cache();
                        self.notice(format!("reasoning effort set to {requested}"));
                    }
                    Ok(false) => self.notice("this model has no reasoning effort control".into()),
                    Err(error) => self.notice(format!("could not save reasoning effort: {error}")),
                }
            }
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
                     shift+tab cycles reasoning effort · ctrl+v attaches clipboard images · \
                     ctrl+o opens full tool detail"
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
                self.held_prompts.clear();
                self.shell_block = None;
                self.reload_block = None;
                self.context_tokens = 0;
                self.discard_composer_images();
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
                extui::shutdown_then_start(self, "new");
            }
            "/resume" => self.open_resume_menu(),
            "/tree" => self.open_tree_menu(),
            "/settings" => self.open_settings(),
            "/copy" => self.copy_last(),
            "/undo" => self.undo_change(),
            "/usage" => self.show_usage(""),
            _ if trimmed.starts_with("/usage ") => {
                self.show_usage(trimmed["/usage ".len()..].trim())
            }
            "/fork" => self.fork_session(None),
            _ if trimmed.starts_with("/fork ") => {
                self.fork_session(Some(trimmed["/fork ".len()..].trim().to_string()))
            }
            "/export" => self.export_session(None),
            _ if trimmed.starts_with("/export ") => {
                self.export_session(Some(trimmed["/export ".len()..].trim().to_string()))
            }
            "/compact" => self.compact_now(None),
            _ if trimmed.starts_with("/compact ") => {
                self.compact_now(Some(trimmed["/compact ".len()..].trim().to_string()))
            }
            "/reload" => self.reload(),
            "/trust" => match crate::core::config::trust::set(&self.agent.cwd(), true) {
                Ok(()) => {
                    self.notice(
                        "directory trusted — its AGENTS.md and .e skills/prompts now load".into(),
                    );
                    self.install_project_packages();
                }
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
                    crate::core::config::home::spawn(async move {
                        let result = host.run_command(&name, &args).await;
                        let _ = results.send(AppJob::Command { result, epoch }).await;
                    });
                } else if is_literal_slash_prompt(&trimmed) {
                    // Absolute paths and a literal leading slash are prompt
                    // text, not misspelled commands. This is how screenshot
                    // clipboard tools hand e `/var/.../capture.png question`.
                    self.prompt(trimmed);
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
            crate::core::config::home::spawn(async move {
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
        self.submit_with_images(text, images);
    }

    fn submit_with_images(
        &mut self,
        text: String,
        images: Vec<crate::core::providers::ImageInput>,
    ) {
        self.remember_prompt(text.clone());
        let count = images.len();
        let held = self.agent.submit_message(
            crate::core::providers::ChatMessage::user_with_images(text.clone(), images),
            system_prompt(),
        );
        if !held {
            self.transcript.push(
                Block::new(Kind::User, display_image_prompt(&text, count)).with_images(count),
            );
        }
    }

    fn prompt(&mut self, text: String) {
        // A fresh prompt closes the queued-prompt review and resumes the
        // queue — the reference's resume-after-new-prompt.
        self.close_queue_review();
        // While reloading or a `!` shell command is running,
        // hold the message; it submits (and displays) when the block lifts —
        // a turn must not start without the shell output it was promised.
        if self.reloading || self.shell_block.is_some() {
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

    /// Ask the core to checkpoint at its next safe provider boundary.
    /// `/usage [24h|7d|30d|all]`: tokens and estimated cost by model over a
    /// period, from the sessions on disk.
    fn show_usage(&mut self, period: &str) {
        let Some(report) = crate::core::usage::report_for(period) else {
            self.notice("usage periods: 24h, 7d (default), 30d, all".into());
            return;
        };
        let label = match period.trim() {
            "24h" | "day" | "today" => "last 24 hours",
            "30d" | "month" => "last 30 days",
            "all" => "whole history",
            _ => "last 7 days",
        };
        self.transcript
            .push(Block::show(crate::core::extensions::Show {
                title: format!("Usage · {label}"),
                body: crate::core::usage::markdown(&report, label),
                format: crate::core::extensions::Format::Markdown,
            }));
    }

    /// `/undo`: put back what the last write or edit replaced.
    fn undo_change(&mut self) {
        if self.active.is_some() || self.agent.is_streaming() {
            self.notice("a turn is running — press Esc to stop it, then /undo".into());
            return;
        }
        match self.agent.undo_last_change() {
            Ok(Some(label)) => {
                let left = self.agent.undo_depth();
                self.notice(format!(
                    "undid {label}{}",
                    if left > 0 {
                        format!(" — {left} more to undo")
                    } else {
                        String::new()
                    }
                ));
            }
            Ok(None) => self.notice("nothing to undo".into()),
            Err(error) => self.notice(error),
        }
    }

    /// `/fork [name]`: continue in a new session file seeded with the
    /// current branch. The transcript and history stay as they are; only
    /// where the next messages land changes. The original file is untouched.
    fn fork_session(&mut self, name: Option<String>) {
        if self.active.is_some() || self.agent.is_streaming() {
            self.notice("a turn is running — press Esc to stop it, then /fork".into());
            return;
        }
        let messages = self.agent.history_snapshot();
        if messages.is_empty() {
            self.notice("nothing to fork yet — send a message first".into());
            return;
        }
        let model = self.agent.model_slug();
        let mut log = match crate::core::session::SessionLog::create_with(
            &self.agent.cwd(),
            &model,
            &messages,
        ) {
            Ok(log) => log,
            Err(error) => {
                self.notice(format!("could not fork: {error}"));
                return;
            }
        };
        let name = name
            .filter(|n| !n.is_empty())
            .or_else(|| self.agent.session_name().map(|n| format!("{n} (fork)")));
        if let Some(name) = &name {
            if let Err(error) = log.set_name(name) {
                self.notice(format!("could not name the fork: {error}"));
            }
        }
        let file = log
            .path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.agent.set_session(Some(log));
        self.agent.adopt_session_name(name.clone());
        self.session_epoch += 1;
        set_tab_title(&tab_title(&title_path(), name.as_deref()));
        extui::shutdown_then_start(self, "fork");
        self.notice(format!(
            "forked into {file} — the original session is unchanged"
        ));
    }

    /// `/export [path]`: write the current branch as a self-contained HTML
    /// page. Default: `e-session-<id>.html` in the working directory.
    fn export_session(&mut self, path: Option<String>) {
        let messages = self.agent.history_snapshot();
        if messages.is_empty() {
            self.notice("nothing to export yet".into());
            return;
        }
        let title = self
            .agent
            .session_name()
            .or_else(|| {
                self.agent
                    .session_path()
                    .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
            })
            .unwrap_or_else(|| "e session".into());
        let target = match path.filter(|p| !p.is_empty()) {
            Some(path) => {
                let path = std::path::PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    self.agent.cwd().join(path)
                }
            }
            None => {
                let id = self
                    .agent
                    .session_id()
                    .map(|id| id.chars().take(8).collect::<String>())
                    .unwrap_or_else(|| "unsaved".into());
                self.agent.cwd().join(format!("e-session-{id}.html"))
            }
        };
        let page = crate::core::export::html(&title, &self.agent.model_slug(), &messages);
        match std::fs::write(&target, page) {
            Ok(()) => self.notice(format!("exported to {}", target.display())),
            Err(error) => self.notice(format!("could not export: {error}")),
        }
    }

    fn compact_now(&mut self, focus: Option<String>) {
        if self.shell_block.is_some() {
            self.notice("a shell command is running — compact after it finishes".into());
            return;
        }
        self.agent.request_compaction_with(system_prompt(), focus);
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
        crate::core::config::home::spawn(async move {
            let shell_cmd = cmd.clone();
            let home = crate::core::config::home::home();
            let output = tokio::task::spawn_blocking(move || {
                crate::core::config::home::with_home(home, || {
                    crate::core::tools::run_shell(&shell_cmd, &cwd)
                })
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
    /// A just-trusted repository's `.e/packages` may list packages not on
    /// disk: install them now, in the background, and say so — the one
    /// moment trust and a network fetch belong together. The result lands
    /// as a notice; `/reload` picks the packages up.
    fn install_project_packages(&mut self) {
        let cwd = self.agent.cwd().to_path_buf();
        let missing = crate::core::resources::packages::project_missing(&cwd);
        if missing.is_empty() {
            return;
        }
        self.notice(format!(
            "installing {} from .e/packages…",
            match missing.len() {
                1 => "1 package".to_string(),
                n => format!("{n} packages"),
            }
        ));
        let results = self.results.clone();
        let epoch = self.session_epoch;
        crate::core::config::home::spawn(async move {
            let outcomes = crate::core::resources::packages::install_project(&cwd).await;
            let failed = outcomes.iter().filter(|r| r.is_err()).count();
            let lines: Vec<String> = outcomes
                .into_iter()
                .map(|r| r.unwrap_or_else(|e| e))
                .collect();
            let notice = if failed == 0 {
                format!("{} — /reload to use them", lines.join("; "))
            } else {
                format!("{} — fix and run `e install`", lines.join("; "))
            };
            let result = crate::core::extensions::CommandResult {
                notice: Some(notice),
                show: None,
                prompt: None,
                session_name: None,
            };
            let _ = results.send(AppJob::Command { result, epoch }).await;
        });
    }

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
        // The old host's surfaces die with it; a modal it was waiting on is
        // answered "cancelled" by the drop.
        self.ui_queue.clear();
        self.cancel_ui_prompt();
        if self
            .menu
            .as_ref()
            .is_some_and(|m| m.kind == MenuKind::Extension)
        {
            self.menu = None;
        }
        self.ext_panel = None;
        self.ext_status.clear();
        let old = self.host.clone();
        let jobs = self.jobs.clone();
        let results = self.results.clone();
        let requests = self.requests.clone();
        let path = self.agent.session_path().map(|p| p.display().to_string());
        crate::core::config::home::spawn(async move {
            old.event("session_shutdown", serde_json::json!({"reason": "reload"}))
                .await;
            old.shutdown().await;
            let host = crate::core::extensions::ExtensionHost::start(jobs, Some(requests)).await;
            host.event(
                "session_start",
                serde_json::json!({"reason": "reload", "path": path}),
            )
            .await;
            let _ = results.send(AppJob::Reloaded(host)).await;
        });
    }

    fn copy_last(&mut self) {
        let last = self
            .agent
            .history_snapshot()
            .iter()
            .rev()
            .find(|m| m.role() == "assistant" && !m.content.trim().is_empty())
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

    /// Refresh cached sign-in, effort, and layout preferences from disk.
    /// Call after sign-in, model switches, effort cycles, settings changes,
    /// and /reload.
    fn refresh_status_cache(&mut self) {
        self.signed_in =
            crate::core::auth::signed_in(&crate::core::auth::load(), &self.agent.model.provider);
        self.status_effort = self.agent.effort();
        self.bottom_pinned = crate::core::config::settings::tui_mode() == "fullscreen";
        self.live_preview_rows = crate::core::config::settings::get_u64("tool_preview_rows")
            .filter(|n| *n <= 20)
            .unwrap_or(5) as usize;
        self.tool_label_rows = crate::core::config::settings::get_u64("tool_label_rows")
            .filter(|n| (1..=20).contains(n))
            .unwrap_or(2) as usize;
        for block in &mut self.transcript.blocks {
            if block.live_preview_rows != self.live_preview_rows
                || block.tool_label_rows != self.tool_label_rows
            {
                block.live_preview_rows = self.live_preview_rows;
                block.tool_label_rows = self.tool_label_rows;
                block.touch();
            }
        }
    }

    fn notice(&mut self, text: String) {
        self.transcript.push(Block::new(Kind::Notice, text));
    }

    /// A submitted prompt joins up-arrow recall for this session and the
    /// history file for the next. API keys never come through here.
    fn remember_prompt(&mut self, text: String) {
        // A memory-only run (`--no-save`) leaves no trace on disk, prompts
        // included; recall still works within the session.
        if self.agent.saves_session() {
            crate::tui::history::append(&text);
        }
        self.editor.push_history(text);
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

/// Write a title without letting a path or session name terminate its OSC.
fn set_tab_title(title: &str) {
    // Escape codes into a pipe are garbage in the pipe; titles only make
    // sense on a terminal.
    if !stdout_is_tty() {
        return;
    }
    let title = crate::core::tools::sanitize_display(title).replace('\n', " ");
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

/// Stable labels used in composer chrome and the transcript for attachments.
fn image_labels(count: usize) -> String {
    (1..=count)
        .map(|index| format!("[Image {index}]"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Add attachment labels when the prompt text does not already carry them.
fn display_image_prompt(text: &str, count: usize) -> String {
    let labels = image_labels(count);
    let already_labeled = text.starts_with(&labels);
    if already_labeled {
        text.to_string()
    } else if text.trim().is_empty() {
        labels
    } else {
        format!("{labels} {text}")
    }
}

/// A screenshot tool commonly pastes an absolute temporary path followed by
/// the user's prompt. Return the existing image prefix and the text after it.
/// Checking extension boundaries from left to right also handles spaces in the
/// filename without requiring shell quoting.
fn leading_image_prompt(input: &str) -> Option<(&str, &str)> {
    let lower = input.to_ascii_lowercase();
    for extension in [".png", ".jpg", ".jpeg", ".gif", ".webp"] {
        let mut from = 0;
        while let Some(relative) = lower[from..].find(extension) {
            let end = from + relative + extension.len();
            let boundary = input[end..].chars().next().is_none_or(char::is_whitespace);
            if boundary && std::path::Path::new(&input[..end]).is_file() {
                return Some((&input[..end], input[end..].trim_start()));
            }
            from = end;
        }
    }
    None
}

fn is_literal_slash_prompt(input: &str) -> bool {
    input == "/"
        || input.starts_with("/ ")
        || input
            .split_whitespace()
            .next()
            .is_some_and(|token| token[1..].contains('/'))
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
            | "effort"
            | "scoped-models"
            | "reload"
            | "resume"
            | "new"
            | "clear"
            | "copy"
            | "compact"
            | "fork"
            | "export"
            | "undo"
            | "usage"
            | "trust"
            | "settings"
            | "help"
            | "version"
            | "quit"
            | "exit"
    )
}

/// The `/` picker's functional group for a built-in command, shown as its
/// right-aligned category. `value` is the slashed command (`/login`); an
/// unknown name falls to General.
fn builtin_category(value: &str) -> &'static str {
    match value {
        "/login" => "Account",
        "/models" | "/effort" | "/scoped-models" => "Model",
        "/resume" | "/new" | "/tree" | "/compact" | "/fork" | "/export" => "Session",
        "/trust" | "/undo" => "Workspace",
        _ => "General",
    }
}

/// Display order of the `/` picker's category tags: Account, Model,
/// Session, Workspace, then the General catch-all — so same-tag rows sit
/// together instead of scattering through the list.
fn category_rank(meta: &str) -> u8 {
    match meta {
        "Account" => 0,
        "Model" => 1,
        "Session" => 2,
        "Workspace" => 3,
        _ => 4,
    }
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
        .filter(|n| n.message.role() == "user")
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
/// it (repaired the same way a resume's is, so a crash-cut ancestor never
/// replays as a dangling call), and its prompt text for the composer. None
/// means the id no longer resolves or the ancestor path is corrupt.
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
    let mut seen = std::collections::HashSet::new();
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) {
            return None;
        }
        let node = by_id.get(id.as_str()).copied()?;
        path_ids.push(id.clone());
        cursor = node.parent.clone();
    }
    path_ids.reverse();
    let mut messages = path_ids
        .iter()
        .filter_map(|id| by_id.get(id.as_str()).map(|n| n.message.clone()))
        .collect();
    crate::core::session::repair_history(&mut messages);
    Some((head, messages, target.message.content.clone()))
}

/// The composer's editing keymap: a user's `~/.e/keybindings.json` chord
/// override is consulted first (`Some(action)` overrides, `Some(None)`
/// swallows the chord, `None` means "not mentioned"); anything left
/// unmentioned falls through to e's built-in bindings below, so an empty or
/// missing file reproduces this function's behavior exactly.
fn key_of(event: &KeyEvent, keymap: &crate::core::config::keybindings::Keymap) -> Option<Key> {
    // Crossterm can report Command/Super through the enhanced keyboard
    // protocol. Never degrade an unhandled modified key to printable text.
    if event
        .modifiers
        .intersects(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META)
    {
        return None;
    }
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
        // ctrl+d with text deletes forward, completing the emacs chord
        // family (a/e/k/u/w/b/f). On an empty composer the app-level
        // handler above has already taken it as quit, like a shell EOF.
        (KeyCode::Char('d'), true, _) => Key::Delete,
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

/// The channel extensions' own requests travel on: created by the caller
/// before the host starts (so `initialize` can promise a UI), consumed here.
pub type Requests = (
    tokio::sync::mpsc::Sender<crate::core::extensions::HostRequest>,
    tokio::sync::mpsc::Receiver<crate::core::extensions::HostRequest>,
);

pub async fn run(
    options: RunOptions,
    host: std::sync::Arc<crate::core::extensions::ExtensionHost>,
    jobs_tx: tokio::sync::mpsc::Sender<String>,
    jobs_rx: tokio::sync::mpsc::Receiver<String>,
    requests: Requests,
) -> std::io::Result<()> {
    let home = options
        .agent
        .home
        .clone()
        .unwrap_or_else(crate::core::config::home::home);
    crate::core::config::home::scope(home, run_scoped(options, host, jobs_tx, jobs_rx, requests))
        .await
}

/// Run the terminal and its configuration reads within the selected home.
async fn run_scoped(
    options: RunOptions,
    host: std::sync::Arc<crate::core::extensions::ExtensionHost>,
    jobs_tx: tokio::sync::mpsc::Sender<String>,
    mut jobs_rx: tokio::sync::mpsc::Receiver<String>,
    requests: Requests,
) -> std::io::Result<()> {
    let (requests_tx, mut requests_rx) = requests;
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
    // report as usual. (\x1b[<u pops the keyboard enhancement stack.) Only
    // a panic on this thread — the frame loop, driven by the runtime's
    // block_on — is fatal to the session; the paint thread, tool tasks and
    // the turn worker all run elsewhere and catch their own panics to keep
    // the session alive, so the hook must leave the terminal alone for them
    // (the hook fires before any catch_unwind gets its say).
    {
        let default_hook = std::panic::take_hook();
        let frame_thread = std::thread::current().id();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == frame_thread {
                let _ = terminal::disable_raw_mode();
                print!("\x1b[<u\x1b[?2004l\x1b[?25h\r\n");
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
            }
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
    // The launch anchor: the frame paints below where the user launched e,
    // never over what came before (the main-screen model mirrors pi's
    // regular mode). A terminal that doesn't answer DSR 6n — a raw pty —
    // falls back to the screen's bottom row, the common launch spot.
    let anchor = crate::tui::paint::background::query_cursor_row(rows).unwrap_or(rows - 1) as usize;
    let mut painter = Painter::spawn(cols, rows, anchor);
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
        composer_images: Vec::new(),
        composer_generation: 0,
        clipboard_reading: false,
        clipboard_submit_pending: false,
        agent,
        active: None,
        overlay: None,
        armed_at: None,
        should_quit: false,
        context_tokens: 0,
        pending_key: None,
        menu: None,
        staged_scope: None,
        auth: None,
        settings: None,
        show_thinking: crate::core::config::settings::show_thinking(),
        jobs: jobs_tx,
        logins: logins_tx,
        login_task: None,
        login_sequence: 0,
        host,
        results: results_tx,
        input_verdicts: PendingInputVerdicts::default(),
        compacting: false,
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
        queue_review: None,
        session_epoch: 0,
        update_installed: None,
        relaunch: false,
        rendering_delayed: false,
        last_paint_failure: None,
        light_background: detected,
        bottom_pinned: false,
        live_preview_rows: 5,
        tool_label_rows: 2,
        signed_in: false,
        status_effort: None,
        requests: requests_tx,
        ui_queue: extui::UiQueue::new(),
        ui_prompt: None,
        ext_status: std::collections::BTreeMap::new(),
        ext_panel: None,
        external_edit: false,
    };
    app.editor
        .seed_history(crate::tui::history::load(crate::tui::history::RECALL));
    app.refresh_status_cache();
    app.emit(
        "session_start",
        serde_json::json!({
            "reason": "startup",
            "path": app.agent.session_path().map(|p| p.display().to_string()),
        }),
    );
    app.transcript
        .push(Block::new(Kind::Banner, crate::VERSION));
    for warning in model::config_warnings() {
        app.notice(format!("warning: {warning}"));
    }
    if !agent_options.save_session {
        app.notice("session saving disabled for this run".into());
    }
    match agent_options.tool_mode {
        crate::core::cli::ToolMode::None => {
            app.notice("no-tools mode — provider requests contain no tool schemas".into())
        }
        crate::core::cli::ToolMode::All => {}
    }
    if crate::core::config::trust::status(&app.agent.cwd()).is_none() {
        app.trust = Some(TrustStage::new(&app.agent.cwd()));
    }
    // The trust lookup may be the first read of trust.json; drain afterward so
    // its recovery joins warnings collected while constructing the app.
    for warning in crate::core::config::store::take_warnings() {
        app.notice(format!("warning: {warning}"));
    }
    // The harness pattern: check for a newer release in the background at
    // launch, install it silently, and say so — the running session is
    // untouched until a restart. Dev builds and the opt-out are exempt.
    if !crate::core::update::is_dev_build() && crate::core::config::settings::auto_update() {
        let results = app.results.clone();
        crate::core::config::home::spawn(async move {
            if let Ok(Some(version)) = crate::core::update::self_update().await {
                let _ = results.send(AppJob::Updated(version)).await;
            }
        });
    }
    // Providers' model lists refresh in the background (the reference
    // behavior, sourced from each gateway's own /models): a model a provider
    // ships today shows in /models today, no e release involved.
    crate::core::config::home::spawn(crate::core::providers::catalog::refresh_remote());
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
    // Terminal input is read on its own thread and can be paused: while an
    // external editor owns the terminal (ctrl+g), nothing here may read
    // it, or the editor's keystrokes land in e instead.
    let input_paused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut input_rx = spawn_input_reader(input_paused.clone());
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
    painter.frame_in_view(app.frame(cols as usize, rows as usize), false);

    // Frame pacing: every select arm may change what's on screen, but frames
    // are built at most once per interval — a token burst becomes one paint,
    // and a deferred paint fires when the interval lapses.
    const FRAME_INTERVAL: Duration = Duration::from_millis(33);
    let mut next_paint = tokio::time::Instant::now();
    let mut paint_deferred = false;
    let mut event_buf: Vec<SessionEvent> = Vec::with_capacity(128);
    let mut mouse_enabled = false;

    loop {
        if app.external_edit {
            app.external_edit = false;
            edit_externally(&mut app, &mut painter, &input_paused, cols, rows, anchor).await;
        }
        tokio::select! {
            maybe = input_rx.recv() => {
                let Some(Ok(event)) = maybe else { break };
                match event {
                    TermEvent::Paste(text) if app.viewer.is_none() => {
                        app.paste(&text);

                    }
                    TermEvent::Mouse(event) => {
                        if app.viewer.is_some() {
                            match event.kind {
                                crossterm::event::MouseEventKind::ScrollUp => app.scroll_viewer(false, 3, cols as usize, rows as usize),
                                crossterm::event::MouseEventKind::ScrollDown => app.scroll_viewer(true, 3, cols as usize, rows as usize),
                                _ => {}
                            }
                        }
                    },
                    TermEvent::Resize(c, r) => {
                        cols = c;
                        rows = r;
                        painter.resize(c, r);
                    }
                    TermEvent::Key(k) if k.kind != crossterm::event::KeyEventKind::Release => {
                        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                        let system_paste = k
                            .modifiers
                            .intersects(KeyModifiers::SUPER | KeyModifiers::META)
                            && matches!(k.code, KeyCode::Char('v') | KeyCode::Char('V'));
                        if ctrl && k.code == KeyCode::Char('c') {
                            app.interrupt_or_exit();
                        } else if ((ctrl && k.code == KeyCode::Char('v')) || system_paste)
                            && app.composer_free()
                            && !app.editor.mask
                        {
                            app.paste_clipboard();
                        } else if app.viewer_key(k, cols as usize, rows as usize) {

                        } else if ctrl && k.code == KeyCode::Char('o') {
                            app.viewer = Some(Viewer::new());
                        } else if app.ext_panel.as_ref().is_some_and(|p| p.interactive)
                            && app.menu.is_none()
                            && app.settings.is_none()
                            && app.auth.is_none()
                            && app.trust.is_none()
                            && !(ctrl && k.code == KeyCode::Char('c'))
                        {
                            // The panel owns the keyboard: Esc closes it
                            // here, every other chord goes to its owner.
                            // ctrl+c stays e's.
                            if k.code == KeyCode::Esc {
                                app.close_ext_panel(true);
                            } else {
                                app.forward_panel_key(&k);
                            }
                        } else if k.code == KeyCode::Esc
                            && app.ext_panel.is_some()
                            && app.menu.is_none()
                            && app.settings.is_none()
                            && app.auth.is_none()
                            && app.trust.is_none()
                            && !app.ui_input_open()
                        {
                            app.close_ext_panel(true);
                        } else if k.code == KeyCode::Esc && app.ui_input_open() {
                            app.cancel_ui_prompt();
                        } else if let Some(stage) = &mut app.trust {
                            match k.code {
                                KeyCode::Up => stage.step(-1),
                                KeyCode::Down => stage.step(1),
                                KeyCode::PageUp => stage.page(-1, cols as usize, (rows as usize).saturating_sub(1)),
                                KeyCode::PageDown => stage.page(1, cols as usize, (rows as usize).saturating_sub(1)),
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
                                            if trusted {
                                                app.install_project_packages();
                                            } else {
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
                        } else if let Some(panel) = &mut app.settings {
                            let mut setting_error = None;
                            let changing_effort = panel.selected_key() == Some("effort")
                                && matches!(k.code, KeyCode::Left | KeyCode::Right);
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
                            } else if changing_effort {
                                app.agent.use_saved_effort();
                            }
                            // A theme change applies immediately; settings can
                            // also change what the statusline derives from disk.
                            // The thinking toggle and keymap are file-backed
                            // too — re-read them so a mid-session change
                            // lands this frame.
                            app.apply_theme();
                            app.apply_keymap();
                            app.show_thinking =
                                crate::core::config::settings::show_thinking();
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
                                    app.discard_composer_images();
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
                                    // The outer guard matched ApiKey; if the
                                    // stage somehow moved on, fall through to
                                    // the default handling instead of panicking.
                                    if let AuthStage::ApiKey { provider } = &*stage {
                                        let provider = provider.clone();
                                        app.pending_key = None;
                                        app.editor.mask = false;
                                        let selected = crate::core::providers::registry::key_providers()
                                            .iter()
                                            .position(|p| p.name == provider)
                                            .unwrap_or(0);
                                        *stage = AuthStage::Key { selected };
                                    }
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
                                || (ctrl && matches!(k.code, KeyCode::Char('x') | KeyCode::Char('s'))))
                        {
                            match (k.code, ctrl) {
                                (KeyCode::Char('x'), true) => {
                                    // Reset: stage nothing — the picker
                                    // mirrors "no scope" and Ctrl+S saves it
                                    // (or Ctrl+X again is enough to walk
                                    // back). Nothing hits settings.json
                                    // until Ctrl+S.
                                    app.staged_scope = Some(Vec::new());
                                    app.open_scoped_menu();
                                }
                                (KeyCode::Char('s'), true) => app.save_scope(),
                                _ => app.toggle_scoped(),
                            }
                        } else if app.menu.is_some()
                            && (matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Esc)
                                || (k.code == KeyCode::Tab
                                    && !k.modifiers.contains(KeyModifiers::SHIFT)
                                    && app
                                        .menu
                                        .as_ref()
                                        .is_some_and(|menu| menu.has_tabs()))
                                // Shift+tab belongs to the picker while it
                                // is open: it steps the tabs backward. The
                                // effort shortcut stays a bare-composer key.
                                || (k.code == KeyCode::BackTab
                                    && app
                                        .menu
                                        .as_ref()
                                        .is_some_and(|menu| menu.has_tabs())))
                            && !ctrl
                        {
                            match k.code {
                                KeyCode::Up => {
                                    if let Some(menu) = app.menu.as_mut() {
                                        menu.step(-1);
                                    }
                                }
                                KeyCode::Down => {
                                    if let Some(menu) = app.menu.as_mut() {
                                        menu.step(1);
                                    }
                                }
                                KeyCode::Tab => {
                                    if let Some(menu) = app.menu.as_mut() {
                                        menu.cycle_tab();
                                    }
                                }
                                KeyCode::BackTab => {
                                    if let Some(menu) = app.menu.as_mut() {
                                        menu.cycle_tab_back();
                                    }
                                }
                                KeyCode::Enter => { app.select_menu(); }
                                KeyCode::Esc => {
                                    if app.menu.as_ref().is_some_and(|m| m.kind == MenuKind::Extension) {
                                        app.cancel_ui_prompt();
                                    }
                                    if app.menu.as_ref().is_some_and(|m| m.filter_without_trigger && m.kind != MenuKind::Commands) {
                                        app.editor.set_text("");
                                    }
                                    app.menu = None;
                                    // Closing the scoped picker without
                                    // Ctrl+S discards its staged edits.
                                    app.staged_scope = None;
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
                            app.discard_composer_images();
                            app.notice("login cancelled".into());
                        } else if k.code == KeyCode::Esc && app.agent.is_streaming() {
                            app.agent.interrupt();
                        } else if ctrl
                            && k.code == KeyCode::Char('g')
                            && app.menu.is_none()
                            && app.settings.is_none()
                            && app.auth.is_none()
                            && app.trust.is_none()
                            && !app.ui_input_open()
                            && app.pending_key.is_none()
                        {
                            // Deferred to the top of the loop: the terminal
                            // hand-off needs the painter and the reader,
                            // which the key handler does not own.
                            app.external_edit = true;
                        } else if ctrl && matches!(k.code, KeyCode::Char('p') | KeyCode::Char('P')) {
                            let backward = k.code == KeyCode::Char('P')
                                || k.modifiers.contains(KeyModifiers::SHIFT);
                            app.cycle_model(!backward);
                        } else if app.menu.is_none()
                            && app.settings.is_none()
                            && app.auth.is_none()
                            && app.trust.is_none()
                            && (k.code == KeyCode::BackTab
                                || (k.code == KeyCode::Tab
                                    && !ctrl
                                    && k.modifiers.contains(KeyModifiers::SHIFT)))
                        {
                            // Shift+tab cycles the model's declared levels —
                            // a bare-composer shortcut. With a picker or
                            // panel open the keys belong to that surface;
                            // the effort setting must not mutate unseen
                            // beneath it. The statusline confirms the
                            // change; nothing lands in the transcript.
                            match app.agent.cycle_effort() {
                                Ok(Some(level)) => {
                                    app.refresh_status_cache();
                                    app.emit("effort_change", serde_json::json!({"effort": level}));
                                }
                                Ok(None) => {}
                                Err(error) => app.notice(format!("could not save reasoning effort: {error}")),
                            }
                        } else if !ctrl && app.queue_review_key(k.code) {
                            // Consumed by the queued-prompt review.
                        } else if let Some(key) = key_of(&k, &app.keymap) {
                            if let EditorResult::Submit(text) = app.editor.key(key) {
                                app.submit_composer(text);
                            }
                            app.sync_menu();
                        } else if let Some(chord) = extui::chord_of(&k)
                            .filter(|chord| app.host.has_shortcut(chord))
                            .filter(|_| !app.ui_input_open() && app.pending_key.is_none())
                        {
                            // A declared extension shortcut, answered like
                            // a command. Only a chord neither e nor the
                            // composer (built in, or the user's
                            // keybindings.json) took reaches this branch —
                            // so unbinding a chord there frees it for an
                            // extension.
                            let host = app.host.clone();
                            let results = app.results.clone();
                            let epoch = app.session_epoch;
                            crate::core::config::home::spawn(async move {
                                let result = host.run_shortcut(&chord).await;
                                let _ = results.send(AppJob::Command { result, epoch }).await;
                            });
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
            request = requests_rx.recv() => {
                if let Some(request) = request {
                    app.on_host_request(request);
                }
            }
            job = results_rx.recv() => {
                match job {
                    Some(AppJob::Command { result, epoch }) => {
                        app.deliver_command_result(result, epoch);
                    }
                    Some(AppJob::Completions { command, prefix, items }) => {
                        app.show_completions(&command, &prefix, items);
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
                    Some(AppJob::ClipboardPaste {
                        generation,
                        paste,
                        fallback,
                    }) => {
                        app.apply_clipboard_paste(generation, paste, fallback);
                    }
                    Some(AppJob::Reloaded(host)) => {
                        app.reloading = false;
                        app.host = host.clone();
                        app.agent.set_host(host);
                        // A narrowing the old host's extension installed
                        // has no owner left to lift it.
                        app.agent.set_active_tools(None);
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
                            let output_id = (!output.content.trim().is_empty()).then(|| app.remember_output(format!("$ {cmd}"), display_output));
                            if let Some(idx) = app.shell_block.take() {
                                if let Some(block) = app.transcript.blocks.get_mut(idx) {
                                    block.done = true;
                                    block.is_error = output.is_error();
                                    block.detail = Some(shown);
                                    block.output_id = output_id;
                                    block.touch();
                                }
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
                            crate::core::config::home::spawn(crate::core::providers::catalog::refresh_remote());
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
        let capture_mouse = app.viewer.is_some();
        if capture_mouse != mouse_enabled {
            if capture_mouse {
                let _ = execute!(std::io::stdout(), EnableMouseCapture);
            } else {
                let _ = execute!(std::io::stdout(), DisableMouseCapture);
            }
            mouse_enabled = capture_mouse;
        }
        let paint_status = painter.status();
        app.rendering_delayed = paint_status.delayed(Duration::from_millis(500));
        match paint_status.failure.as_ref().map(|(_, error)| error) {
            Some(error) if app.last_paint_failure.as_ref() != Some(error) => {
                app.notice(format!("render failed: {error}"));
                app.last_paint_failure = Some(error.clone());
            }
            None => app.last_paint_failure = None,
            Some(_) => {}
        }

        let now = tokio::time::Instant::now();
        if now >= next_paint || app.should_quit {
            let frame = if app.viewer.is_some() {
                app.viewer_frame(cols as usize, rows as usize)
            } else {
                app.frame(cols as usize, rows as usize)
            };
            painter.frame_in_view(frame, app.viewer.is_some());
            next_paint = now + FRAME_INTERVAL;
            paint_deferred = false;
        } else {
            paint_deferred = true;
        }
        if app.should_quit {
            break;
        }
    }

    // Let the final frame land before the terminal is restored. Shells use
    // detached process groups, so stop them explicitly before this process
    // gives extensions their shutdown notification.
    painter.shutdown();
    crate::core::tools::kill_tracked_processes();
    app.host
        .event("session_shutdown", serde_json::json!({"reason": "quit"}))
        .await;
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

/// Read terminal events on a thread the frame loop can pause. Each poll
/// waits at most 100 ms, so a pause takes effect within that; the thread
/// ends when the receiver is dropped.
fn spawn_input_reader(
    paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> tokio::sync::mpsc::Receiver<std::io::Result<TermEvent>> {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    std::thread::spawn(move || loop {
        if paused.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        match crossterm::event::poll(Duration::from_millis(100)) {
            Ok(true) => {
                let event = crossterm::event::read();
                if tx.blocking_send(event).is_err() {
                    break;
                }
            }
            Ok(false) => {}
            Err(error) => {
                let _ = tx.blocking_send(Err(error));
                break;
            }
        }
    });
    rx
}

/// ctrl+g: hand the terminal to `$VISUAL` / `$EDITOR` (or the `editor`
/// setting) with the draft in a private temp file, then take it back and
/// load what was saved. The painter is stopped and respawned around the
/// hand-off so the next frame repaints from a known-blank screen; the input
/// reader is paused so the editor gets every keystroke.
async fn edit_externally(
    app: &mut App,
    painter: &mut Painter,
    input_paused: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    cols: u16,
    rows: u16,
    anchor: usize,
) {
    let command = crate::core::config::settings::external_editor();
    let Some(program) = command.first().cloned() else {
        app.notice("no editor: set `editor` in ~/.e/settings.json or $EDITOR".into());
        return;
    };
    let draft = app.editor.expanded_text();
    // An unguessable name and create_new: a pre-placed symlink in the shared
    // temp directory is refused rather than followed.
    let path = std::env::temp_dir().join(format!(
        "e-draft-{}-{}.md",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let written = options
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, draft.as_bytes()));
        if let Err(error) = written {
            app.notice(format!("could not stage the draft: {error}"));
            return;
        }
    }
    input_paused.store(true, std::sync::atomic::Ordering::SeqCst);
    // The reader's poll in flight ends within its 100 ms window.
    tokio::time::sleep(Duration::from_millis(150)).await;
    painter.shutdown();
    let _ = execute!(
        std::io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste
    );
    let _ = terminal::disable_raw_mode();
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b[?25h");
        let _ = out.flush();
    }
    let args: Vec<String> = command.iter().skip(1).cloned().collect();
    let edit_path = path.clone();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&program)
            .args(&args)
            .arg(&edit_path)
            .status()
    })
    .await;
    let _ = terminal::enable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    *painter = Painter::spawn(cols, rows, anchor);
    input_paused.store(false, std::sync::atomic::Ordering::SeqCst);
    match status {
        Ok(Ok(status)) if status.success() => match std::fs::read_to_string(&path) {
            Ok(text) => {
                let text = text.trim_end_matches('\n').to_string();
                app.editor.set_text(&text);
                app.sync_menu();
            }
            Err(error) => app.notice(format!("could not read the edited draft: {error}")),
        },
        Ok(Ok(status)) => app.notice(format!("editor exited with {status}; draft unchanged")),
        Ok(Err(error)) => app.notice(format!("could not start the editor: {error}")),
        Err(_) => app.notice("the editor task failed; draft unchanged".into()),
    }
    let _ = std::fs::remove_file(&path);
    painter.frame(app.frame(cols as usize, rows as usize));
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
            DisableBracketedPaste,
            DisableMouseCapture
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
    /// Interrupt dismisses transient navigation without trusting or submitting.
    #[test]
    fn interrupt_dismisses_trust_and_queue_review_and_drops_held_prompt() {
        let mut app = session_app();
        app.trust = Some(crate::tui::trustpanel::TrustStage::new(&app.agent.cwd()));
        app.queue_review = Some(QueueReview {
            entries: vec![(1, "queued".into())],
            dirty: vec![false],
            selected: 0,
            visible: true,
        });
        app.pending_initial = Some("must not run".into());
        app.editor.set_text("draft");
        app.interrupt_or_exit();
        assert!(app.trust.is_none());
        assert!(app.queue_review.is_none());
        assert!(app.pending_initial.is_none());
        assert!(app.editor.is_empty());
        assert!(!app.agent.is_streaming());
        assert!(!app.should_quit);
    }

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
    fn key_of_matches_the_built_in_bindings_when_the_keymap_is_empty() {
        let keymap = crate::core::config::keybindings::Keymap::empty();
        let ctrl_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(matches!(key_of(&ctrl_w, &keymap), Some(Key::KillWord)));
        let plain_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(matches!(key_of(&plain_x, &keymap), Some(Key::Char('x'))));
        let command_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::SUPER);
        assert!(
            key_of(&command_v, &keymap).is_none(),
            "an unhandled Command key must not insert its printable character"
        );
        // On a non-empty composer ctrl+d is forward delete (the empty
        // composer's quit is the app-level handler's job, before key_of).
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(matches!(key_of(&ctrl_d, &keymap), Some(Key::Delete)));
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
            crate::core::extensions::InputVerdict::default(),
        );
        assert!(later.is_empty(), "a later verdict must wait");

        let ordered = pending.complete(
            first,
            "first".into(),
            None,
            crate::core::extensions::InputVerdict::default(),
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
            crate::core::extensions::InputVerdict::default(),
        );
        assert!(none_ready.is_empty(), "text_only hasn't completed yet");

        let ordered = pending.complete(
            text_only,
            "text only".into(),
            None,
            crate::core::extensions::InputVerdict::default(),
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

    #[test]
    fn a_command_submitted_with_attachments_dispatches_without_them() {
        let mut app = session_app();
        app.composer_images = vec![crate::core::providers::ImageInput {
            media_type: "image/png".into(),
            data: std::sync::Arc::from("AA=="),
        }];

        app.submit_composer("/effort high".into());

        assert!(app.composer_images.is_empty(), "commands drop attachments");
        // The command dispatched: this model has no effort levels, so the
        // command's own notice replaces a model prompt.
        let notice = app
            .transcript
            .blocks
            .iter()
            .rev()
            .find(|block| block.kind == crate::tui::transcript::Kind::Notice)
            .expect("the command dispatched");
        assert_eq!(notice.text, "this model has no reasoning effort control");
    }

    #[test]
    fn saved_unavailable_models_remain_visible_in_the_scope_picker() {
        let mut app = session_app();
        let missing = "signed-out/model".to_string();
        app.staged_scope = Some(vec![missing.clone()]);
        app.menu = Some(Menu::new(
            MenuKind::Scoped,
            "Scoped models",
            HINT_SCOPED,
            Vec::new(),
        ));

        app.open_scoped_menu();
        let menu = app.menu.as_mut().expect("scope picker remains open");
        menu.select_value(&missing);
        let rendered = menu.render(&app.theme, 80).join("\n");
        assert!(rendered.contains(&missing));
        assert!(rendered.contains("unavailable"));
        assert_eq!(
            app.staged_scope.as_deref(),
            Some(std::slice::from_ref(&missing))
        );

        app.toggle_scoped();
        assert_eq!(app.staged_scope.as_deref(), Some([].as_slice()));
    }

    #[test]
    fn a_paste_over_an_open_surface_stays_text() {
        let mut app = session_app();
        app.viewer = Some(Viewer::new());
        let path = std::env::temp_dir().join("e-paste-gate-test.png");
        std::fs::write(&path, b"png").unwrap();

        app.paste(&path.display().to_string());

        assert!(app.composer_images.is_empty(), "no attach over a surface");
        assert_eq!(app.editor.text(), path.display().to_string());
    }

    #[test]
    fn a_crlf_paste_keeps_one_newline_per_line() {
        let mut app = session_app();
        app.paste("line1\r\nline2\r\n");
        assert_eq!(app.editor.text(), "line1\nline2\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enter_waits_for_the_clipboard_result_before_submitting() {
        let mut app = session_app();
        app.clipboard_reading = true;

        app.submit_composer("question".into());
        assert_eq!(app.editor.text(), "question");
        assert!(app.clipboard_submit_pending);

        app.apply_clipboard_paste(0, Ok(clipboard::Paste::Text(" answer".into())), None);
        assert!(!app.clipboard_reading);
        assert!(!app.clipboard_submit_pending);
        assert_eq!(app.editor.text(), "");
        app.editor.key(Key::Up);
        assert_eq!(app.editor.text(), "question answer");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_clipboard_result_releases_a_newer_pending_submit() {
        let mut app = session_app();
        app.clipboard_reading = true;
        app.discard_composer_images();
        app.submit_composer("new draft".into());

        app.apply_clipboard_paste(0, Ok(clipboard::Paste::Text("stale".into())), None);

        assert_eq!(app.editor.text(), "");
        app.editor.key(Key::Up);
        assert_eq!(app.editor.text(), "new draft");
    }

    #[test]
    fn clipboard_images_stay_out_of_editable_text_and_get_chat_labels() {
        let mut app = session_app();
        app.agent.model.image_input = true;
        let image = || crate::core::providers::ImageInput {
            media_type: "image/png".into(),
            data: std::sync::Arc::from("AA=="),
        };

        app.apply_clipboard_paste(
            0,
            Ok(clipboard::Paste::Images(vec![image(), image()])),
            None,
        );

        assert_eq!(app.editor.text(), "");
        assert_eq!(app.composer_images.len(), 2);
        let attachment_label = app.theme.fg("dim", "[Image 1] [Image 2]");
        assert!(app
            .frame(80, 20)
            .iter()
            .any(|line| line.contains(&attachment_label)));
        assert_eq!(
            display_image_prompt("explain these", 2),
            "[Image 1] [Image 2] explain these"
        );
        assert_eq!(
            display_image_prompt("[Image 1] [Image 2] explain these", 2),
            "[Image 1] [Image 2] explain these"
        );
    }

    #[test]
    fn screenshot_paths_at_the_start_of_a_prompt_are_split_from_the_question() {
        let dir = std::env::temp_dir().join(format!("e-shot-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Shotbase Capture.png");
        std::fs::write(&path, b"png").unwrap();
        let input = format!("{} what is wrong here?", path.display());
        let (found, prompt) = leading_image_prompt(&input).expect("image prefix is recognized");
        assert_eq!(std::path::Path::new(found), path);
        assert_eq!(prompt, "what is wrong here?");
        assert!(is_literal_slash_prompt(&input));
        assert!(is_literal_slash_prompt("/ explain this"));
        assert!(!is_literal_slash_prompt("/modles please"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A screenshot paste onto a model that cannot take images must not
    /// swallow the user's typed question: the text survives as a normal
    /// prompt, with a notice saying the image was dropped.
    #[tokio::test(flavor = "multi_thread")]
    async fn image_paste_text_survives_a_model_without_image_input() {
        let dir = std::env::temp_dir().join(format!("e-shot-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.png");
        std::fs::write(&path, b"png").unwrap();

        let mut app = session_app();
        // A dead port: the turn task must never reach a server.
        app.agent.model.base_url = "http://127.0.0.1:1".into();
        assert!(!app.agent.model.image_input);

        app.submit(format!("{} what is wrong here?", path.display()));

        let texts: Vec<&str> = app
            .transcript
            .blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("does not accept image input")),
            "the drop must be announced: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("what is wrong here?")),
            "the question must still reach the model: {texts:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A bare image path with no question has nothing left to submit after
    /// the image is dropped, so only the notice appears.
    #[tokio::test(flavor = "multi_thread")]
    async fn bare_image_path_on_a_text_only_model_only_notices() {
        let dir = std::env::temp_dir().join(format!("e-shot-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("shot.png");
        std::fs::write(&path, b"png").unwrap();

        let mut app = session_app();
        app.agent.model.base_url = "http://127.0.0.1:1".into();

        app.submit(path.display().to_string());

        let texts: Vec<&str> = app
            .transcript
            .blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect();
        assert_eq!(
            texts.len(),
            1,
            "only the notice, no phantom prompt: {texts:?}"
        );
        assert!(
            texts[0].contains("does not accept image input"),
            "{texts:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The error block supplies its own label, so the event text stays bare.
    #[test]
    fn failed_turn_does_not_duplicate_the_error_label() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::Error("provider response interrupted".into()));
        app.on_session_event(SessionEvent::TurnEnd { aborted: false });
        let error = app
            .transcript
            .blocks
            .iter()
            .find(|block| block.kind == Kind::Error)
            .unwrap();
        assert_eq!(error.text, "provider response interrupted");
    }

    /// CRLF is one pasted line break; standalone CR and LF still work.
    #[test]
    fn paste_normalizes_line_endings_once() {
        let mut app = session_app();
        app.paste("first\r\nsecond\rthird\nfourth");
        assert_eq!(app.editor.text(), "first\nsecond\nthird\nfourth");
    }

    /// Global cancellation stops an in-flight sign-in and retires secret input.
    #[tokio::test]
    async fn ctrl_c_cancels_login_before_arming_exit() {
        let mut app = session_app();
        let cancellation = crate::core::auth::login::LoginCancellation::default();
        let observed = cancellation.clone();
        app.login_task = Some(ActiveLogin {
            flow_id: 1,
            cancellation,
            task: tokio::spawn(std::future::pending()),
            wait_for_callback: false,
        });
        app.auth = Some(AuthStage::Waiting { back: None });
        app.pending_key = Some("mock".into());
        app.editor.mask = true;
        app.editor.set_text("synthetic-secret");
        app.interrupt_or_exit();
        assert!(observed.is_cancelled());
        assert!(app.auth.is_none());
        assert!(app.pending_key.is_none());
        assert!(!app.editor.mask);
        assert!(app.editor.is_empty());
        assert!(!app.should_quit);
        app.interrupt_or_exit();
        assert!(app.should_quit);
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

    #[test]
    fn compaction_uses_the_request_models_pricing_after_a_model_switch() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.agent.model.id = "switched".into();
        app.agent.model.pricing = Some(crate::core::providers::catalog::Pricing {
            input_per_million: 99.0,
            output_per_million: 99.0,
            cache_read_per_million: None,
            cache_write_5m_per_million: None,
            cache_write_1h_per_million: None,
        });
        let request_pricing = crate::core::providers::catalog::Pricing {
            input_per_million: 2.0,
            output_per_million: 10.0,
            cache_read_per_million: Some(0.2),
            cache_write_5m_per_million: Some(2.5),
            cache_write_1h_per_million: Some(4.0),
        };
        let usage = crate::core::providers::Usage {
            input: 1_000_000,
            ..Default::default()
        };
        app.on_session_event(SessionEvent::Compacted {
            summary: "summary".into(),
            context_tokens: 10,
            response: crate::core::providers::ResponseMeta {
                id: "response".into(),
                timestamp: 1,
                provider: "mock".into(),
                model: "m".into(),
                purpose: crate::core::providers::ResponsePurpose::Compaction,
                usage: Some(usage),
            },
            pricing: Some(request_pricing),
        });

        assert_eq!(app.active.unwrap().cost_usd, Some(2.0));
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
            effort: Vec::new(),
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
            composer_images: Vec::new(),
            composer_generation: 0,
            clipboard_reading: false,
            clipboard_submit_pending: false,
            agent,
            active: None,
            overlay: None,
            armed_at: None,
            should_quit: false,
            context_tokens: 0,
            pending_key: None,
            menu: None,
            staged_scope: None,
            auth: None,
            settings: None,
            show_thinking: true,
            jobs,
            logins,
            login_task: None,
            login_sequence: 0,
            host: crate::core::extensions::ExtensionHost::empty(),
            results,
            input_verdicts: PendingInputVerdicts::default(),
            compacting: false,
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
            queue_review: None,
            session_epoch: 0,
            update_installed: None,
            relaunch: false,
            rendering_delayed: false,
            last_paint_failure: None,
            light_background: false,
            bottom_pinned: false,
            live_preview_rows: 5,
            tool_label_rows: 2,
            signed_in: false,
            status_effort: None,
            requests: tokio::sync::mpsc::channel(1).0,
            ui_queue: extui::UiQueue::new(),
            ui_prompt: None,
            ext_status: std::collections::BTreeMap::new(),
            ext_panel: None,
            external_edit: false,
        }
    }

    /// A request as the host would deliver it, with a receiver for the
    /// reply an extension would read.
    fn fake_request(
        extension: &str,
        method: &str,
        params: serde_json::Value,
    ) -> (
        crate::core::extensions::HostRequest,
        tokio::sync::oneshot::Receiver<Result<serde_json::Value, String>>,
    ) {
        crate::core::extensions::HostRequest::fake(extension, method, params)
    }

    #[test]
    fn show_requests_become_transcript_blocks_and_status_is_bounded() {
        let mut app = session_app();
        let (request, reply) = fake_request(
            "diff",
            "ui.show",
            serde_json::json!({"title": "diff f.txt", "body": "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+B\n", "format": "diff"}),
        );
        app.on_host_request(request);
        assert_eq!(reply.blocking_recv().unwrap(), Ok(serde_json::json!({})));
        let block = app.transcript.blocks.last().unwrap();
        assert_eq!(block.kind, Kind::Show);
        assert_eq!(block.text, "diff f.txt");
        assert_eq!(
            block.detail.as_deref(),
            Some("f.txt\n    1   a\n    2 - b\n    2 + B")
        );

        let (request, reply) = fake_request(
            "diff",
            "ui.status",
            serde_json::json!({"text": "x".repeat(100) + "\x1b[31m"}),
        );
        app.on_host_request(request);
        assert!(reply.blocking_recv().unwrap().is_ok());
        let status = app.ext_status.get("diff").unwrap();
        assert_eq!(status.chars().count(), 40);
        assert!(status.ends_with('…') && !status.contains('\x1b'));

        let (request, reply) = fake_request("diff", "ui.bogus", serde_json::json!({}));
        app.on_host_request(request);
        assert!(reply
            .blocking_recv()
            .unwrap()
            .unwrap_err()
            .contains("unknown method"));
    }

    #[test]
    fn select_opens_the_picker_and_enter_answers_with_the_value() {
        let mut app = session_app();
        let (request, reply) = fake_request(
            "plan",
            "ui.select",
            serde_json::json!({"title": "Mode", "options": [
                {"label": "Plan", "description": "read only", "value": "plan"},
                "Build"
            ]}),
        );
        app.on_host_request(request);
        let menu = app.menu.as_ref().expect("picker opened");
        assert_eq!(menu.kind, MenuKind::Extension);
        assert_eq!(menu.title, "Mode");
        assert!(app.select_menu());
        assert_eq!(
            reply.blocking_recv().unwrap(),
            Ok(serde_json::json!({"value": "plan", "label": "Plan"}))
        );
        assert!(app.menu.is_none() && app.ui_prompt.is_none());

        // A long plain option answers with the string that was offered,
        // whatever the row showed.
        let long = "/Users/me/projects/very/long/path/to/some/deeply/nested/file_name.rs";
        let (request, reply) = fake_request(
            "plan",
            "ui.select",
            serde_json::json!({"title": "File", "options": [long]}),
        );
        app.on_host_request(request);
        assert!(app.select_menu());
        assert_eq!(reply.blocking_recv().unwrap().unwrap()["value"], long);

        // A second modal while one is open waits its turn; Esc cancels
        // the open one and the next takes the surface on the next frame.
        let (first, first_reply) =
            fake_request("a", "ui.confirm", serde_json::json!({"title": "Sure?"}));
        let (second, second_reply) =
            fake_request("b", "ui.input", serde_json::json!({"title": "Name"}));
        app.on_host_request(first);
        app.on_host_request(second);
        assert_eq!(app.ui_queue.len(), 1);
        app.cancel_ui_prompt();
        app.menu = None;
        assert_eq!(
            first_reply.blocking_recv().unwrap(),
            Ok(serde_json::json!({"confirmed": false}))
        );
        app.pump_ui_queue();
        assert!(app.ui_input_open());
        app.editor.set_text("world");
        app.submit("world".into());
        assert_eq!(
            second_reply.blocking_recv().unwrap(),
            Ok(serde_json::json!({"text": "world"}))
        );
        assert!(!app.ui_input_open() && app.editor.is_empty());
    }

    #[test]
    fn session_tools_narrows_the_agent_and_info_reports_it() {
        let mut app = session_app();
        let (request, reply) = fake_request(
            "plan",
            "session.tools",
            serde_json::json!({"names": ["read", "grep"]}),
        );
        app.on_host_request(request);
        assert!(reply.blocking_recv().unwrap().is_ok());
        assert_eq!(
            app.agent.active_tools(),
            Some(vec!["read".to_string(), "grep".to_string()])
        );
        let (request, reply) = fake_request("plan", "session.info", serde_json::json!({}));
        app.on_host_request(request);
        let info = reply.blocking_recv().unwrap().unwrap();
        assert_eq!(info["tools"], serde_json::json!(["read", "grep"]));
        assert_eq!(info["running"], false);
        let (request, _) =
            fake_request("plan", "session.tools", serde_json::json!({"names": null}));
        app.on_host_request(request);
        assert_eq!(app.agent.active_tools(), None);
    }

    #[test]
    fn a_command_opened_picker_filters_on_typed_text_and_clears_it_on_close() {
        use crate::tui::menu::{Menu, MenuItem, MenuKind, HINT_USE};
        let mut app = session_app();
        let items = vec![
            MenuItem::new("fix the parser", "", "/a.jsonl"),
            MenuItem::new("write docs", "", "/b.jsonl"),
        ];
        app.menu =
            Some(Menu::new(MenuKind::Sessions, "Sessions", HINT_USE, items).without_trigger());
        app.editor.set_text("pars");
        app.sync_menu();
        let menu = app.menu.as_ref().unwrap();
        assert_eq!(menu.len(), 1, "typed text is the filter");
        assert_eq!(menu.current().unwrap().value, "/a.jsonl");
        // Esc path: the filter text was never a draft.
        app.menu = None;
        // (the key loop clears the editor for a filtered picker; the
        // selection path does the same through select_menu)
        app.menu = Some(
            Menu::new(
                MenuKind::Tree,
                "Rewind to",
                HINT_USE,
                vec![MenuItem::new("x", "", "n1")],
            )
            .without_trigger(),
        );
        app.editor.set_text("x");
        app.sync_menu();
        assert!(app.select_menu());
        assert!(
            app.editor.is_empty(),
            "the filter does not linger as a draft"
        );
    }

    #[test]
    fn picking_an_argument_completion_replaces_the_typed_prefix() {
        use crate::tui::menu::{Menu, MenuItem, MenuKind, HINT_USE};
        let mut app = session_app();
        app.editor.set_text("/deploy eu st");
        app.menu = Some(Menu::new(
            MenuKind::Arguments,
            "/deploy",
            HINT_USE,
            vec![MenuItem::new("staging", "pre-prod", "staging")],
        ));
        assert!(app.select_menu());
        assert_eq!(app.editor.text(), "/deploy eu staging ");
        assert!(
            app.menu.is_none(),
            "an empty host offers no further completions"
        );
    }

    #[test]
    fn panels_keep_span_spacing_strip_control_bytes_and_paint_tokens() {
        let mut app = session_app();
        let (request, reply) = fake_request(
            "plan",
            "ui.panel",
            serde_json::json!({"title": "Plan", "interactive": true, "lines": [
                [{"text": "› ", "token": "accent"}, {"text": "[ ] ", "token": "dim"}, {"text": "step\x1b[31m one"}],
                "plain\nrow"
            ]}),
        );
        app.on_host_request(request);
        assert!(reply.blocking_recv().unwrap().is_ok());
        let panel = app.ext_panel.as_ref().unwrap();
        assert!(panel.interactive && panel.extension == "plan");
        let rows = panel.render(&app.theme, 80);
        // divider, header, blank, two rows, divider
        assert_eq!(rows.len(), 6);
        assert_eq!(
            rows[3],
            format!(
                "  {}{}step one",
                app.theme.fg("accent", "› "),
                app.theme.fg("dim", "[ ] ")
            )
        );
        assert_eq!(rows[4], "  plain row");
        // Another extension's panel replaces it; a null from the owner closes.
        let (request, _) = fake_request("other", "ui.panel", serde_json::json!({"lines": ["x"]}));
        app.on_host_request(request);
        assert_eq!(app.ext_panel.as_ref().unwrap().title, "other");
        let (request, _) = fake_request("plan", "ui.panel", serde_json::Value::Null);
        app.on_host_request(request);
        assert!(app.ext_panel.is_some(), "only the owner closes a panel");
        let (request, _) = fake_request("other", "ui.panel", serde_json::Value::Null);
        app.on_host_request(request);
        assert!(app.ext_panel.is_none());
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
        // The queue is paused-free: seed it directly, then open the review
        // on the keyed snapshot the commit will edit against.
        app.agent.update_queued(
            vec![(1, "keep  me\n".into()), (2, "old draft".into())],
            vec![],
        );
        let entries = app.agent.queue_snapshot();
        app.queue_review = Some(QueueReview {
            // The untouched entry carries a trailing newline a blanket trim
            // would silently strip; only the edited entry may rewrite.
            entries,
            dirty: vec![false, true],
            selected: 1,
            visible: true,
        });
        app.editor.set_text("  new draft  ");

        assert!(app.queue_review_key(KeyCode::Enter));

        let entries = app.agent.queue_snapshot();
        assert_eq!(
            entries,
            vec![(1, "keep  me\n".to_string()), (2, "new draft".to_string())]
        );
    }

    #[test]
    fn queue_review_commit_drops_an_entry_edited_to_empty() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.agent
            .update_queued(vec![(1, "first".into()), (2, "second".into())], vec![]);
        let entries = app.agent.queue_snapshot();
        app.queue_review = Some(QueueReview {
            entries,
            dirty: vec![false, true],
            selected: 1,
            visible: true,
        });
        app.editor.set_text("   ");

        assert!(app.queue_review_key(KeyCode::Enter));

        let entries = app.agent.queue_snapshot();
        assert_eq!(entries, vec![(1, "first".to_string())]);
    }

    #[test]
    fn full_reader_opens_at_tail_and_keeps_a_paused_reading_position() {
        let mut app = session_app();
        app.editor.set_text("unsent draft");
        for i in 0..30 {
            app.transcript
                .push(Block::new(Kind::User, format!("message {i}")));
        }
        app.viewer = Some(Viewer::new());
        let frame = app.viewer_frame(80, 24);
        assert_eq!(frame.len(), 24);
        assert!(frame[..21].iter().any(|row| row.contains("message 29")));
        assert!(frame[21].contains("Review"));
        assert_eq!(frame[22], "");
        app.scroll_viewer(false, 5, 80, 24);
        let paused = app.viewer_frame(80, 24);
        app.transcript.push(Block::new(Kind::User, "new arrival"));
        assert_eq!(app.viewer_frame(80, 24), paused);
        app.scroll_viewer(true, usize::MAX, 80, 24);
        assert!(app
            .viewer_frame(80, 24)
            .iter()
            .any(|row| row.contains("new arrival")));
        assert!(app.viewer_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 80, 24));
        assert!(app.viewer.is_none());
        assert_eq!(app.editor.text(), "unsent draft");
    }

    #[test]
    fn review_screen_rebuilds_after_transcript_changes() {
        let mut app = session_app();
        app.viewer = Some(Viewer {
            follow_tail: false,
            scroll: 0,
            ..Viewer::new()
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
            follow_tail: false,
            scroll: 0,
            ..Viewer::new()
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
            follow_tail: false,
            scroll: 500,
            ..Viewer::new()
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

    /// Review closes each group after its inserted output, at either depth.
    #[test]
    fn review_branches_connect_through_output_and_omission_rows() {
        let mut app = session_app();
        let detail = app.remember_output("output".into(), "one\ntwo\nthree\nfour".into());
        let mut group = Block::tool_group(
            (1..=2)
                .map(|id| {
                    crate::tui::transcript::ToolChild::pending(
                        id,
                        "command".into(),
                        "Running".into(),
                        "Ran".into(),
                        format!("command {id}\nwrapped argument"),
                    )
                })
                .collect(),
        );
        for id in 1..=2 {
            group.start_tool(id);
            group.finish_tool(
                id,
                crate::core::tools::ToolOutcome::Completed,
                "done".into(),
                "",
            );
        }
        for child in &mut group.tool_children {
            child.detail = Some(detail);
        }
        app.transcript.push(group);
        for full in [false, true] {
            let rows: Vec<_> = app
                .viewer_rows(80, full)
                .iter()
                .map(|row| crate::core::tools::strip_ansi(row))
                .collect();
            assert_eq!(rows.iter().filter(|row| row.starts_with('└')).count(), 1);
            assert_eq!(rows.iter().filter(|row| row.starts_with('├')).count(), 2);
            assert_eq!(rows[2], "│ wrapped argument");
            assert_eq!(
                rows.last().unwrap(),
                if full {
                    "└ four"
                } else {
                    "└ 1 more rows · → to expand"
                }
            );
            assert!(rows[1..rows.len() - 1]
                .iter()
                .all(|row| row.starts_with('├') || row.starts_with('│')));
        }
    }

    #[test]
    fn tui_mode_defaults_inline_and_settings_cycle_the_layout() {
        let home = std::env::temp_dir().join(format!("e-composer-{}", uuid::Uuid::new_v4()));
        crate::core::config::home::with_home(home.clone(), || {
            let mut app = session_app();
            app.refresh_status_cache();
            assert!(!app.bottom_pinned);
            assert!(app.frame(80, 30).len() < 30);

            let setting = crate::core::config::settings::all(Vec::new())
                .into_iter()
                .find(|setting| setting.key == "tui_mode")
                .unwrap();
            assert_eq!(setting.current(), "inline");
            crate::core::config::settings::set_string("composer_position", "bottom").unwrap();
            app.refresh_status_cache();
            assert!(app.bottom_pinned);
            assert_eq!(setting.current(), "fullscreen");
            setting.cycle(-1).unwrap();
            assert_eq!(setting.current(), "inline");
            setting.cycle(1).unwrap();
            app.refresh_status_cache();
            assert!(app.bottom_pinned);
            assert_eq!(app.frame(80, 30).len(), 30);

            setting.cycle(-1).unwrap();
            app.refresh_status_cache();
            assert!(!app.bottom_pinned);
            assert!(app.frame(80, 30).len() < 30);

            crate::core::config::settings::set_string("tui_mode", "invalid").unwrap();
            app.refresh_status_cache();
            assert_eq!(setting.current(), "inline");
            assert!(!app.bottom_pinned);
        });
        std::fs::remove_dir_all(home).unwrap();
    }

    /// Reloaded label budgets invalidate existing frames and apply to new groups.
    #[test]
    fn tool_label_preference_updates_existing_and_new_groups() {
        let home = std::env::temp_dir().join(format!("e-tool-labels-{}", uuid::Uuid::new_v4()));
        crate::core::config::home::with_home(home.clone(), || {
            let mut app = session_app();
            app.refresh_status_cache();
            app.on_session_event(SessionEvent::TurnStart);
            let batch = || SessionEvent::ToolBatchStart {
                calls: vec![crate::core::agent::ToolCallPresentation {
                    id: 1,
                    name: "bash".into(),
                    arguments: "{}".into(),
                    category: "command".into(),
                    running: "Running".into(),
                    completed: "Ran".into(),
                    target: "long-command".repeat(20),
                }],
            };
            app.on_session_event(batch());
            app.on_session_event(SessionEvent::ToolStart { id: 1 });
            let original = app.transcript.blocks[0].lines_for_test(&app.theme, 40);
            crate::core::config::store::update_versioned(
                &home.join("settings.json"),
                0o644,
                1,
                |settings| {
                    settings.insert("tool_label_rows".into(), serde_json::json!(1));
                },
            )
            .unwrap();
            app.refresh_status_cache();
            let shorter = app.transcript.blocks[0].lines_for_test(&app.theme, 40);
            assert_eq!(shorter.len() + 1, original.len());
            app.notice("separate group".into());
            app.on_session_event(batch());
            assert_eq!(app.transcript.blocks.last().unwrap().tool_label_rows, 1);
        });
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn command_output_and_completion_do_not_move_the_composer_dock() {
        let mut app = session_app();
        app.bottom_pinned = true;
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::ToolBatchStart {
            calls: vec![crate::core::agent::ToolCallPresentation {
                id: 1,
                name: "bash".into(),
                arguments: "{}".into(),
                category: "command".into(),
                running: "Running".into(),
                completed: "Ran".into(),
                target: "test command with a long argument".into(),
            }],
        });
        app.on_session_event(SessionEvent::ToolStart { id: 1 });
        for (width, height, count) in [(80, 24, 1), (24, 12, 30), (80, 24, 2)] {
            app.on_session_event(SessionEvent::ToolOutput {
                id: 1,
                stream: crate::core::tools::OutputStream::Stdout,
                chunk: "output line with a long argument\n".repeat(count),
            });
            let frame = app.frame(width, height);
            assert!(frame.len() >= height);
            assert!(crate::core::tools::strip_ansi(&frame[frame.len() - 3]).starts_with("┃ "));
            let review = app.viewer_rows(width, true).join("\n");
            assert!(
                review.contains("output line"),
                "running output must be reviewable"
            );
        }
        app.on_session_event(SessionEvent::ToolEnd {
            id: 1,
            outcome: crate::core::tools::ToolOutcome::Completed,
            summary: "done".into(),
            content: "authoritative final output".into(),
        });
        app.on_session_event(SessionEvent::TurnEnd { aborted: false });
        let frame = app.frame(80, 24);
        assert_eq!(frame.len(), 24);
        assert!(crate::core::tools::strip_ansi(&frame[21]).starts_with("┃ "));
        assert!(app
            .viewer_rows(80, true)
            .join("\n")
            .contains("authoritative final output"));
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
            entries: vec![(9, "original".into())],
            dirty: vec![false],
            selected: 0,
            visible: true,
        });
        app.editor.set_text("edited draft");

        app.on_session_event(SessionEvent::TurnEnd { aborted: true });

        assert!(app.queue_review.is_none());
        assert!(app.editor.is_empty());
    }

    #[test]
    fn rejected_image_suffixes_remain_literal_prompts() {
        let image =
            std::env::temp_dir().join(format!("e-image-command-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&image, b"image placeholder").unwrap();
        for suffix in ["/new", "/quit", "!touch should-not-run"] {
            let mut app = session_app();
            app.agent
                .load_history(vec![crate::core::providers::ChatMessage::user(
                    "keep history",
                )]);
            // Hold literal prompts locally without starting a provider request.
            app.reloading = true;
            app.submit_direct(format!("{} {suffix}", image.display()));
            assert_eq!(app.held_prompts, [suffix]);
            assert!(!app.should_quit);
            assert!(app.shell_block.is_none());
            assert_eq!(app.agent.history_snapshot()[0].content, "keep history");
        }
        std::fs::remove_file(image).unwrap();
    }

    #[test]
    fn stale_tool_lifecycle_does_not_change_the_current_turn() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::ToolStart { id: 99 });
        assert!(matches!(
            app.active.as_ref().unwrap().turn.phase,
            TurnPhase::Waiting
        ));
        app.on_session_event(SessionEvent::ToolBatchStart {
            calls: vec![crate::core::agent::ToolCallPresentation {
                id: 2,
                name: "bash".into(),
                arguments: "{}".into(),
                category: "command".into(),
                running: "Running".into(),
                completed: "Ran".into(),
                target: "current".into(),
            }],
        });
        app.on_session_event(SessionEvent::ToolEnd {
            id: 99,
            outcome: crate::core::tools::ToolOutcome::Completed,
            summary: "late".into(),
            content: "old output".into(),
        });
        let active = app.active.as_ref().unwrap();
        assert_eq!(active.pending_tools, 1);
        assert!(matches!(active.turn.phase, TurnPhase::Tool));
        assert!(app.outputs.is_empty());
    }

    #[test]
    fn streamed_deltas_append_to_the_active_transcript_block() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::TextDelta("first ".into()));
        app.on_session_event(SessionEvent::TextDelta("second".into()));

        let replies: Vec<_> = app
            .transcript
            .blocks
            .iter()
            .filter(|block| block.kind == Kind::Assistant)
            .collect();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].text, "first second");
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

    fn tool_batch() -> SessionEvent {
        SessionEvent::ToolBatchStart {
            calls: vec![crate::core::agent::ToolCallPresentation {
                id: 1,
                name: "read".into(),
                arguments: "{\"path\":\"f.rs\"}".into(),
                category: "read".into(),
                running: "reading".into(),
                completed: "read".into(),
                target: "f.rs".into(),
            }],
        }
    }

    /// A typical think-then-tools turn opens a second thinking burst below
    /// the tools. Ending a burst — tools taking over, or TurnEnd — detaches it
    /// but leaves the thought expanded where it sits: no collapse to a
    /// `Thought for Ns` row, and never marked done (which would shrink the
    /// frame and jump the screen).
    #[test]
    fn thinking_bursts_stay_expanded_at_each_handoff() {
        let mut app = session_app();
        app.on_session_event(SessionEvent::TurnStart);
        app.on_session_event(SessionEvent::ReasoningDelta("before tools".into()));
        assert_eq!(thinking_flags(&app), vec![("before tools".into(), false)]);

        app.on_session_event(tool_batch());
        assert_eq!(
            thinking_flags(&app),
            vec![("before tools".into(), false)],
            "the pre-tool burst stays expanded when tools take over"
        );

        app.on_session_event(SessionEvent::ReasoningDelta("after tools".into()));
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("before tools".into(), false),
                ("after tools".into(), false)
            ],
            "a fresh burst opens its own block below the tools"
        );

        // Continuing tools must not absorb expanded reasoning as though it
        // were an old collapsed summary, shrinking the transcript mid-turn.
        app.on_session_event(tool_batch());
        assert_eq!(thinking_flags(&app).len(), 2);

        app.on_session_event(SessionEvent::TurnEnd { aborted: false });
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("before tools".into(), false),
                ("after tools".into(), false)
            ],
            "TurnEnd leaves both thoughts expanded, unchanged"
        );
    }

    /// Retries and steered messages also end the live burst; each thought
    /// keeps its expanded place, and the next burst opens a new block.
    #[test]
    fn thinking_stays_expanded_at_retry_and_steer() {
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
            vec![("attempt one".into(), false)],
            "the abandoned attempt's thought stays put at the retry"
        );

        app.on_session_event(SessionEvent::ReasoningDelta("attempt two".into()));
        app.on_session_event(SessionEvent::Steered("also check this".into()));
        app.on_session_event(SessionEvent::ReasoningDelta("after steer".into()));
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("attempt one".into(), false),
                ("attempt two".into(), false),
                ("after steer".into(), false)
            ]
        );

        app.on_session_event(SessionEvent::TurnEnd { aborted: false });
        assert_eq!(
            thinking_flags(&app),
            vec![
                ("attempt one".into(), false),
                ("attempt two".into(), false),
                ("after steer".into(), false)
            ]
        );
    }
}
