//! Activity row and status line — pure projections of app state.

use crate::core::output::{compact_model_label, format_tokens};
use crate::core::providers::FailureCause;
use crate::tui::markdown::visible_width;
use crate::tui::theme::Theme;

/// How long a "recovered" flash stays up before reverting to normal turn
/// activity — matches the reference client's brief, self-clearing confirm.
pub const RECOVERED_VISIBLE_MS: u64 = 1500;

/// Which transient progress surface owns the current turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnPhase {
    Thinking,
    /// Backing off after a retryable failure before another attempt.
    Retrying,
    Tool,
    AssistantText,
}

/// The detail `TurnPhase::Retrying` needs to render. `since` anchors the
/// live countdown — the label ticks the remaining seconds down from
/// `delay_secs` as the backoff burns, pi-style.
#[derive(Clone, Debug)]
pub struct RetryStatus {
    pub attempt: u32,
    pub limit: u32,
    pub delay_secs: u64,
    pub since: std::time::Instant,
    pub cause: FailureCause,
    pub reason: String,
}

/// A brief confirmation shown after a retry campaign's first successful
/// event, cleared once its display window elapses.
#[derive(Clone, Copy, Debug)]
pub struct RecoveredStatus {
    pub attempt: u32,
    pub limit: u32,
    pub since: std::time::Instant,
}

/// Truncate to `max_chars`, marking the cut with an ellipsis — keeps a raw
/// connect-error string or provider message from blowing out the row.
fn clip(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// Elapsed time in the activity row grammar — the shared core formatter,
/// re-exported under its long-standing name.
pub use crate::core::output::format_elapsed;

/// Per-turn token flow and focused activity phase.
pub struct Turn {
    /// Latest request's full context, from real usage (a chars/4 seed until
    /// the first report lands).
    pub input: u64,
    /// True until a provider Usage frame replaces the chars/4 seed.
    pub input_estimated: bool,
    /// Output tokens of completed steps, from real usage.
    pub output: u64,
    /// chars/4 estimate of the current step's streamed text + reasoning.
    pub estimated_output: u64,
    streamed_chars: u64,
    /// Cumulative tool-call argument bytes streamed this step (real output
    /// the token estimate must count).
    assembly_bytes: u64,
    pub phase: TurnPhase,
    /// Set while `phase == Retrying`.
    pub retry: Option<RetryStatus>,
    /// Set briefly after a retry succeeds; the frame tick clears it.
    pub recovered: Option<RecoveredStatus>,
}

impl Default for Turn {
    fn default() -> Self {
        Self::new()
    }
}

impl Turn {
    pub fn new() -> Self {
        Turn {
            input: 0,
            input_estimated: false,
            output: 0,
            estimated_output: 0,
            streamed_chars: 0,
            assembly_bytes: 0,
            phase: TurnPhase::Thinking,
            retry: None,
            recovered: None,
        }
    }

    pub fn note_text(&mut self, text: &str) {
        self.streamed_chars = self
            .streamed_chars
            .saturating_add(text.chars().count() as u64);
        self.refresh_estimate();
    }

    /// Record the cumulative argument bytes the agent reported for this step.
    pub fn note_assembly(&mut self, cumulative_bytes: u64) {
        self.assembly_bytes = cumulative_bytes;
        self.refresh_estimate();
    }

    /// A real usage report landed: fold it in and reset the estimate — from
    /// here the estimate covers only what the *next* step streams, so the
    /// live display is real tokens plus the current step's delta, never an
    /// estimate of tokens already counted for real.
    pub fn note_usage(&mut self, input: u64, output: u64) {
        self.input = input;
        self.input_estimated = false;
        self.output = self.output.saturating_add(output);
        self.streamed_chars = 0;
        self.assembly_bytes = 0;
        self.estimated_output = 0;
    }

    pub fn seed_input(&mut self, input: u64) {
        self.input = input;
        self.input_estimated = true;
    }

    fn refresh_estimate(&mut self) {
        self.estimated_output = (self.streamed_chars + self.assembly_bytes).div_ceil(4);
    }

    pub fn tokens(&self) -> String {
        let output = self.output.saturating_add(self.estimated_output);
        if self.input == 0 && output == 0 {
            String::new()
        } else {
            // The reference compacts past a thousand — `(↑31 ↓9.6k)`. The
            // `~` marks a chars/4 seed the reference doesn't have; it
            // clears once real usage lands.
            let estimate = if self.input_estimated { "~" } else { "" };
            format!(
                "(↑{estimate}{} ↓{})",
                format_tokens(self.input),
                format_tokens(output)
            )
        }
    }

    /// The `Thinking (Ns) (↑… ↓…)` activity label. The clock keeps ticking
    /// through tool and assistant-text phases alike, so the row never
    /// vanishes mid-turn while a tree grows or a reply streams.
    fn thinking_label(&self, elapsed_secs: u64) -> Option<String> {
        if self.recovered.is_some()
            || !matches!(
                self.phase,
                TurnPhase::Thinking | TurnPhase::Tool | TurnPhase::AssistantText
            )
        {
            return None;
        }
        let tokens = self.tokens();
        let suffix = if tokens.is_empty() {
            String::new()
        } else {
            format!(" {tokens}")
        };
        Some(format!(
            "Thinking ({}){suffix}",
            format_elapsed(elapsed_secs)
        ))
    }

    /// A recovered flash overrides everything else until it expires.
    pub fn label(&self, elapsed_secs: u64) -> Option<String> {
        if let Some(r) = &self.recovered {
            return Some(format!("Recovered · attempt {}/{}", r.attempt, r.limit));
        }
        match self.phase {
            TurnPhase::Thinking | TurnPhase::Tool | TurnPhase::AssistantText => {
                self.thinking_label(elapsed_secs)
            }
            TurnPhase::Retrying => {
                let r = self.retry.as_ref()?;
                let waited = r.since.elapsed().as_secs();
                let remaining = r.delay_secs.saturating_sub(waited);
                let reason = clip(&r.reason, 48);
                let hint = if reason.is_empty() {
                    String::new()
                } else {
                    format!(" · {reason}")
                };
                Some(if remaining > 0 {
                    format!(
                        "Retrying ({}/{}) in {}s… esc to cancel{hint}",
                        r.attempt, r.limit, remaining
                    )
                } else {
                    format!(
                        "Retrying ({}/{}) now… esc to cancel{hint}",
                        r.attempt, r.limit
                    )
                })
            }
        }
    }
}

pub struct StatusData {
    /// `None` when no provider is signed in — nothing to show as "the"
    /// model, since none was actually chosen by the user.
    pub model: Option<String>,
    /// Estimated tokens of the current context and the model's window,
    /// for the trailing `N%` segment.
    pub context_used: u64,
    pub context_total: Option<u64>,
}

/// The bottom row: just the model (accent-bright) and the context percent
/// (muted) — everything else lives in the transcript or the activity row.
/// With no panel open a blank spacer rides above; an open panel's own
/// divider sits directly above the row instead. A transient overlay
/// (armed-exit) rides right-aligned; a menu hint replaces the row in dim.
pub fn statusline(
    theme: &Theme,
    data: &StatusData,
    overlay: Option<&str>,
    hint: Option<&str>,
    panel_open: bool,
    width: usize,
) -> Vec<String> {
    let lead: &[String] = if panel_open { &[] } else { &[String::new()] };
    if let Some(hint) = hint {
        let mut rows = lead.to_vec();
        rows.push(theme.fg("dim", hint));
        return rows;
    }
    let mut segments = Vec::new();
    if let Some(model) = &data.model {
        segments.push(compact_model_label(model));
    }
    if let Some(total) = data.context_total.filter(|t| *t > 0) {
        let percent = (data.context_used * 100) / total;
        if percent >= 1 {
            segments.push(format!("{percent}%"));
        }
    }

    let mut line = String::new();
    if let Some((head, rest)) = segments.split_first() {
        line = theme.fg("accent", head);
        if !rest.is_empty() {
            line.push_str(&theme.fg("muted", &format!(" · {}", rest.join(" · "))));
        }
    }
    if let Some(overlay) = overlay {
        let used = visible_width(&line);
        let pad = width.saturating_sub(used + overlay.chars().count());
        if pad > 1 {
            line.push_str(&" ".repeat(pad));
            line.push_str(&theme.fg("muted", overlay));
        } else {
            line = theme.fg("muted", overlay);
        }
    }
    let mut rows = lead.to_vec();
    rows.push(line);
    rows
}

#[cfg(test)]
mod tests {
    use super::{
        format_elapsed, statusline, RecoveredStatus, RetryStatus, StatusData, Turn, TurnPhase,
    };

    #[test]
    fn statusline_is_just_the_model_and_percent() {
        let theme = crate::tui::theme::resolve("dark", false);
        let data = StatusData {
            model: Some("zai/glm-5.3-flash".into()),
            context_used: 6_000,
            context_total: Some(200_000),
        };
        let rows = statusline(&theme, &data, None, None, false, 120);
        let line = rows.last().unwrap();
        // The model leads in the accent; the percent trails muted; nothing
        // else rides the row.
        assert!(line.contains("glm-5.3-flash"), "{line:?}");
        assert!(line.contains("3%"), "{line:?}");
        assert!(
            line.starts_with(&theme.fg_prefix("accent").to_string()),
            "{line:?}"
        );
        for gone in ["Context:", "enter queue", "queued", "/", "("] {
            assert!(
                !line.contains(gone),
                "segment {gone:?} should be gone: {line:?}"
            );
        }
    }
    use crate::core::providers::FailureCause;

    #[test]
    fn activity_has_one_owner_per_phase() {
        let mut turn = Turn::new();
        assert_eq!(turn.label(0).as_deref(), Some("Thinking (0s)"));

        turn.phase = TurnPhase::Tool;
        // The reference keeps the Thinking clock ticking below the
        // transient tool row.
        assert_eq!(turn.label(1).as_deref(), Some("Thinking (1s)"));

        turn.input = 1_000;
        turn.output = 20;
        // The row persists through reply streaming too — it never vanishes
        // mid-turn.
        turn.phase = TurnPhase::AssistantText;
        assert_eq!(turn.label(2).as_deref(), Some("Thinking (2s) (↑1k ↓20)"));

        turn.phase = TurnPhase::Thinking;
        assert_eq!(turn.label(3).as_deref(), Some("Thinking (3s) (↑1k ↓20)"));
    }

    #[test]
    fn elapsed_switches_to_minutes_above_a_minute() {
        // The reference grammar: no separators, no zero padding.
        assert_eq!(format_elapsed(59), "59s");
        assert_eq!(format_elapsed(60), "1m0s");
        assert_eq!(format_elapsed(636), "10m36s");
        assert_eq!(format_elapsed(1_080), "18m0s");
        assert_eq!(format_elapsed(3_663), "1h1m3s");
    }

    #[test]
    fn thinking_label_shows_minutes_past_a_minute() {
        let mut turn = Turn::new();
        turn.input = 39_000;
        turn.output = 20_000;
        // The reference compacts counts past a thousand — `↓9.6k` on the
        // live client — while small counts stay raw.
        assert_eq!(
            turn.label(636).as_deref(),
            Some("Thinking (10m36s) (↑39k ↓20k)")
        );
    }

    #[test]
    fn assembly_bytes_count_toward_the_estimate_while_thinking() {
        let mut turn = Turn::new();
        turn.note_usage(50_000, 200);
        turn.note_assembly(8_000); // ~2k tokens of argument JSON so far
        assert_eq!(
            turn.label(7).as_deref(),
            Some("Thinking (7s) (↑50k ↓2.2k)"),
            "argument streaming stays in the Thinking phase — the tool row, not the footer, owns the activity"
        );
        // The next cumulative report ticks the same counter.
        turn.note_assembly(12_000);
        assert_eq!(turn.tokens(), "(↑50k ↓3.2k)");
    }

    #[test]
    fn usage_resets_the_streaming_estimate() {
        let mut turn = Turn::new();
        turn.note_text(&"x".repeat(4_000)); // ~1k estimated
        assert_eq!(turn.tokens(), "(↑0 ↓1k)");
        // Real usage lands: the estimate must not double what is now
        // counted for real.
        turn.note_usage(9_000, 800);
        assert_eq!(turn.tokens(), "(↑9k ↓800)");
        // The next step's streaming adds on top of the real total.
        turn.note_text("abcd");
        assert_eq!(turn.tokens(), "(↑9k ↓801)");
    }

    #[test]
    fn seeded_input_is_visibly_an_estimate() {
        let mut turn = Turn::new();
        turn.seed_input(5_208_000);
        assert_eq!(turn.tokens(), "(↑~5208k ↓0)");
        turn.note_usage(10_000, 2);
        assert_eq!(turn.tokens(), "(↑10k ↓2)");
    }

    #[test]
    fn retrying_counts_down_with_an_esc_hint() {
        let mut turn = Turn::new();
        turn.phase = TurnPhase::Retrying;
        turn.retry = Some(RetryStatus {
            attempt: 3,
            limit: 10,
            delay_secs: 4,
            since: std::time::Instant::now(),
            cause: FailureCause::ProviderUnavailable,
            reason: "503 Service Unavailable".into(),
        });
        assert_eq!(
            turn.label(0).as_deref(),
            Some("Retrying (3/10) in 4s… esc to cancel · 503 Service Unavailable")
        );
    }

    #[test]
    fn a_burned_backoff_says_retrying_now() {
        let mut turn = Turn::new();
        turn.phase = TurnPhase::Retrying;
        turn.retry = Some(RetryStatus {
            attempt: 1,
            limit: 10,
            delay_secs: 4,
            since: std::time::Instant::now() - std::time::Duration::from_secs(9),
            cause: FailureCause::RateLimited,
            reason: "429 Too Many Requests".into(),
        });
        assert_eq!(
            turn.label(0).as_deref(),
            Some("Retrying (1/10) now… esc to cancel · 429 Too Many Requests")
        );
    }

    #[test]
    fn retrying_clips_a_long_reason() {
        let mut turn = Turn::new();
        turn.phase = TurnPhase::Retrying;
        turn.retry = Some(RetryStatus {
            attempt: 1,
            limit: 10,
            delay_secs: 1,
            since: std::time::Instant::now(),
            cause: FailureCause::Network,
            reason: "x".repeat(200),
        });
        let label = turn.label(0).unwrap();
        assert!(label.len() < 150, "reason should be clipped: {label}");
        assert!(label.contains('…'));
    }

    #[test]
    fn recovered_overrides_the_underlying_phase() {
        let mut turn = Turn::new();
        turn.phase = TurnPhase::Thinking;
        turn.recovered = Some(RecoveredStatus {
            attempt: 4,
            limit: 10,
            since: std::time::Instant::now(),
        });
        assert_eq!(turn.label(9).as_deref(), Some("Recovered · attempt 4/10"));
    }
}
