//! Activity row and status line — pure projections of app state.

use crate::core::output::{compact_model_label, format_tokens};
use crate::tui::markdown::visible_width;
use crate::tui::theme::Theme;
use std::collections::BTreeMap;

/// Per-turn activity: verb, tool tally, token flow, clock.
pub struct Turn {
    pub counts: BTreeMap<&'static str, u64>,
    pub last_verb: Option<&'static str>,
    pub input: u64,
    pub output: u64,
    pub started_at: std::time::Instant,
}

const TOOLS: &[(&str, &str, &str, &str, &str)] = &[
    // name, verb, singular, plural, past
    ("read", "reading", "file", "files", "read"),
    ("ls", "listing", "directory", "directories", "listed"),
    ("write", "writing", "file", "files", "wrote"),
    ("edit", "editing", "file", "files", "edited"),
    ("bash", "running", "command", "commands", "started"),
];

impl Turn {
    pub fn new() -> Self {
        Turn { counts: BTreeMap::new(), last_verb: None, input: 0, output: 0, started_at: std::time::Instant::now() }
    }

    /// Count a tool by the display verb the agent already resolved.
    pub fn note_tool_verb(&mut self, verb: &str) {
        let name = match verb {
            "Read" | "Searched" | "Listed" => "read",
            "Wrote" => "write",
            "Edited" => "edit",
            "Ran" => "bash",
            _ => return,
        };
        if let Some((k, v, ..)) = TOOLS.iter().find(|(n, ..)| *n == name) {
            *self.counts.entry(k).or_insert(0) += 1;
            self.last_verb = Some(v);
        }
    }

    pub fn note_tool(&mut self, name: &str) {
        let key = match name {
            "find" | "grep" | "glob" => "read",
            other => other,
        };
        if let Some((k, verb, ..)) = TOOLS.iter().find(|(n, ..)| *n == key) {
            *self.counts.entry(k).or_insert(0) += 1;
            self.last_verb = Some(verb);
        }
    }

    pub fn label(&self, elapsed_secs: u64) -> String {
        let tokens = if self.input == 0 && self.output == 0 {
            String::new()
        } else {
            format!(" (↑{} ↓{})", format_tokens(self.input), format_tokens(self.output))
        };
        match self.last_verb {
            None => {
                let clock = if elapsed_secs >= 3 { format!(" ({elapsed_secs}s)") } else { String::new() };
                format!("Thinking{clock}{tokens}")
            }
            Some(verb) => {
                let mut parts = Vec::new();
                for (name, _, singular, plural, past) in TOOLS {
                    if let Some(&count) = self.counts.get(name) {
                        if count > 0 {
                            let noun = if count == 1 { singular } else { plural };
                            parts.push(format!("{count} {noun} {past}"));
                        }
                    }
                }
                if parts.is_empty() {
                    format!("{verb}{tokens}")
                } else {
                    format!("{verb} | {}{tokens}", parts.join(", "))
                }
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
pub fn statusline(theme: &Theme, data: &StatusData, overlay: Option<&str>, hint: Option<&str>, width: usize) -> Vec<String> {
    if let Some(hint) = hint {
        return vec![String::new(), theme.fg("muted", hint)];
    }
    let mut segments = Vec::new();
    if data.queued > 0 { segments.push(format!("queued {}", data.queued)); }
    segments.push(compact_model_label(&data.model));
    if let Some(e) = &data.effort { if e != "off" { segments.push(e.clone()); } }
    if let Some(n) = &data.session_name { segments.push(n.clone()); }
    if let Some(p) = data.context_percent { if p >= 1 { segments.push(format!("{p}%")); } }

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
