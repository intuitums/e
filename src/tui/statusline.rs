//! Activity row and status line — pure projections of app state.

use crate::core::output::{compact_model_label, format_tokens};
use crate::tui::markdown::visible_width;
use crate::tui::theme::Theme;
/// Which transient progress surface owns the current turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnPhase {
    Thinking,
    Tool,
    AssistantText,
}

/// Per-turn token flow and focused activity phase.
pub struct Turn {
    pub input: u64,
    pub output: u64,
    pub estimated_output: u64,
    streamed_chars: u64,
    /// True while the counters hold a request-size seed rather than real
    /// usage; the first Usage event replaces them.
    pub seeded: bool,
    pub phase: TurnPhase,
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
            seeded: false,
            phase: TurnPhase::Thinking,
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
    /// footer row. Assistant text needs only markerless token progress.
    pub fn label(&self, elapsed_secs: u64) -> Option<String> {
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
            TurnPhase::Tool => None,
            TurnPhase::AssistantText => {
                let tokens = self.tokens();
                (!tokens.is_empty()).then_some(tokens)
            }
        }
    }
}

pub struct StatusData {
    pub model: String,
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
    segments.push(compact_model_label(&data.model));
    if let Some(e) = &data.effort {
        if e != "off" {
            segments.push(e.clone());
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

    let (head, rest) = segments.split_first().unwrap();
    let mut line = theme.fg("accent", head);
    if !rest.is_empty() {
        line.push_str(&theme.fg("muted", &format!(" · {}", rest.join(" · "))));
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
    use super::{Turn, TurnPhase};

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
}
