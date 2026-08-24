//! Activity row and status line — pure projections of app state.

use crate::core::output::{compact_model_label, format_tokens};
use crate::core::provider::FailureCause;
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

/// The detail `TurnPhase::Retrying` needs to render — set once per failure,
/// not live-ticked: an honest snapshot of what's happening beats a countdown
/// that can drift from when the retry actually fires.
#[derive(Clone, Debug)]
pub struct RetryStatus {
    pub attempt: u32,
    pub limit: u32,
    pub delay_secs: u64,
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

/// Per-turn token flow and focused activity phase.
pub struct Turn {
    pub input: u64,
    pub output: u64,
    pub estimated_output: u64,
    streamed_chars: u64,
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
            output: 0,
            estimated_output: 0,
            streamed_chars: 0,
            phase: TurnPhase::Thinking,
            retry: None,
            recovered: None,
        }
    }

    pub fn note_text(&mut self, text: &str) {
        self.streamed_chars = self
            .streamed_chars
            .saturating_add(text.chars().count() as u64);
        self.estimated_output = self.streamed_chars.div_ceil(4);
    }

    pub fn tokens(&self) -> String {
        let output = self.output.max(self.estimated_output);
        if self.input == 0 && output == 0 {
            String::new()
        } else {
            format!(
                "(↑{} ↓{})",
                format_tokens(self.input),
                format_tokens(output)
            )
        }
    }

    /// The tool phase renders inside its transcript group, not in a duplicate
    /// footer row. Assistant text needs only markerless token progress. A
    /// recovered flash overrides everything else until it expires.
    pub fn label(&self, elapsed_secs: u64) -> Option<String> {
        if let Some(r) = &self.recovered {
            return Some(format!("Recovered · attempt {}/{}", r.attempt, r.limit));
        }
        match self.phase {
            TurnPhase::Thinking => {
                let tokens = self.tokens();
                let suffix = if tokens.is_empty() {
                    String::new()
                } else {
                    format!(" {tokens}")
                };
                Some(format!("Thinking ({elapsed_secs}s){suffix}"))
            }
            TurnPhase::Retrying => {
                let r = self.retry.as_ref()?;
                let reason = clip(&r.reason, 56);
                Some(if r.delay_secs > 0 {
                    format!(
                        "{} · {reason} · retrying in {}s · attempt {}/{}",
                        r.cause.label(),
                        r.delay_secs,
                        r.attempt,
                        r.limit
                    )
                } else {
                    format!(
                        "{} · {reason} · retrying now · attempt {}/{}",
                        r.cause.label(),
                        r.attempt,
                        r.limit
                    )
                })
            }
            TurnPhase::Tool => None,
            TurnPhase::AssistantText => {
                let tokens = self.tokens();
                (!tokens.is_empty()).then_some(tokens)
            }
        }
    }
}

pub struct StatusData {
    /// `None` when no provider is signed in — nothing to show as "the"
    /// model, since none was actually chosen by the user.
    pub model: Option<String>,
    pub effort: Option<String>,
    pub session_name: Option<String>,
    /// Context used, as a percent. Hidden until it rounds to at least 1.
    pub context_percent: Option<u8>,
    pub queued: usize,
}

/// The bottom row: blank spacer, then dot-joined segments; the leading one
/// brighter. A transient overlay (armed-exit, menu hints) replaces the right
/// or the whole row.
pub fn statusline(
    theme: &Theme,
    data: &StatusData,
    overlay: Option<&str>,
    hint: Option<&str>,
    width: usize,
) -> Vec<String> {
    if let Some(hint) = hint {
        return vec![String::new(), theme.fg("muted", hint)];
    }
    let mut segments = Vec::new();
    if data.queued > 0 {
        segments.push(format!("queued {}", data.queued));
    }
    if let Some(model) = &data.model {
        segments.push(compact_model_label(model));
        if let Some(e) = &data.effort {
            if e != "off" {
                segments.push(e.clone());
            }
        }
    }
    if let Some(n) = &data.session_name {
        segments.push(n.clone());
    }
    if let Some(p) = data.context_percent {
        if p >= 1 {
            segments.push(format!("{p}%"));
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
    vec![String::new(), line]
}

#[cfg(test)]
mod tests {
    use super::{RecoveredStatus, RetryStatus, Turn, TurnPhase};
    use crate::core::provider::FailureCause;

    #[test]
    fn activity_has_one_owner_per_phase() {
        let mut turn = Turn::new();
        assert_eq!(turn.label(0).as_deref(), Some("Thinking (0s)"));

        turn.phase = TurnPhase::Tool;
        assert_eq!(turn.label(1), None, "the focused group owns tool activity");

        turn.input = 1_000;
        turn.output = 20;
        turn.phase = TurnPhase::AssistantText;
        assert_eq!(turn.label(2).as_deref(), Some("(↑1k ↓20)"));

        turn.phase = TurnPhase::Thinking;
        assert_eq!(turn.label(3).as_deref(), Some("Thinking (3s) (↑1k ↓20)"));
    }

    #[test]
    fn retrying_shows_cause_reason_delay_and_attempt() {
        let mut turn = Turn::new();
        turn.phase = TurnPhase::Retrying;
        turn.retry = Some(RetryStatus {
            attempt: 3,
            limit: 10,
            delay_secs: 4,
            cause: FailureCause::ProviderUnavailable,
            reason: "503 Service Unavailable".into(),
        });
        assert_eq!(
            turn.label(0).as_deref(),
            Some("Provider unavailable · 503 Service Unavailable · retrying in 4s · attempt 3/10")
        );
    }

    #[test]
    fn retrying_with_no_delay_says_retrying_now() {
        let mut turn = Turn::new();
        turn.phase = TurnPhase::Retrying;
        turn.retry = Some(RetryStatus {
            attempt: 1,
            limit: 10,
            delay_secs: 0,
            cause: FailureCause::RateLimited,
            reason: "429 Too Many Requests".into(),
        });
        assert_eq!(
            turn.label(0).as_deref(),
            Some("Rate limited · 429 Too Many Requests · retrying now · attempt 1/10")
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
