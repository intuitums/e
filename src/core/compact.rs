//! Compaction: summarize the older part of the session so work continues in a
//! fresh context, keeping the recent messages verbatim.
//!
//! The shape follows the reference harness: compaction only ever runs between
//! turns — the frontend checks at TurnEnd (auto, when context usage crosses
//! the reserve threshold) or defers a mid-turn /compact until the turn ends.
//! The cut keeps roughly the most recent `KEEP_RECENT_TOKENS` of messages and
//! never lands on a tool result (a result must follow its call); everything
//! before the cut is flattened to plain text and summarized with a structured
//! checkpoint prompt. The summary seeds a fresh session file, followed by the
//! kept messages — the old session stays fully resumable.

use crate::core::model::Model;
use crate::core::provider::{self, ChatMessage, Event, Request};

/// Auto-compact when context tokens exceed `context_window - RESERVE_TOKENS`.
pub const RESERVE_TOKENS: u64 = 16_384;

/// Approximate token budget of recent messages kept verbatim through a compact.
pub const KEEP_RECENT_TOKENS: u64 = 20_000;

/// Tool results are trimmed in the flattened transcript; the summary needs
/// what they meant, not their full bytes.
const TOOL_RESULT_KEEP: usize = 1500;

const SYSTEM: &str = "You summarize a coding-agent session so it can continue in a fresh context window. Preserve exact file paths, symbol names, and error messages. Write only the summary — no commentary about the summarization itself.";

const INSTRUCTION: &str = "The conversation above is a coding session to checkpoint. Write a structured summary another agent will rely on to continue the work, using exactly these sections:

## Goal
What the user is trying to accomplish.

## Constraints & Preferences
Standing instructions and preferences the user has stated, or \"(none)\".

## Progress
What is done, what is in progress, what is blocked.

## Key Decisions
Each decision with its brief rationale.

## Next Steps
An ordered list of what should happen next.

## Critical Context
File paths, commands, data, or references needed to continue, or \"(none)\".

Keep each section concise.";

/// True when the session has grown into the reserve headroom.
pub fn should_compact(context_tokens: u64, context_window: u64) -> bool {
    context_tokens > context_window.saturating_sub(RESERVE_TOKENS)
}

/// chars/4, the reference heuristic — conservative, and only used to place
/// the cut; the threshold itself works on real provider usage.
fn estimate_tokens(message: &ChatMessage) -> u64 {
    let mut chars = message.content.len();
    for call in &message.tool_calls {
        chars += call.name.len() + call.arguments.len();
    }
    (chars as u64).div_ceil(4)
}

/// Split history into (to_summarize, kept): walk backwards accumulating
/// estimated tokens until the keep budget is reached, then cut at the nearest
/// non-tool message at or after that point — a tool result always stays with
/// its call. Returns an empty `to_summarize` when the history is small enough
/// that compaction would gain nothing.
pub fn split(history: &[ChatMessage]) -> (Vec<ChatMessage>, Vec<ChatMessage>) {
    let mut accumulated = 0u64;
    let mut cut = 0usize;
    for (i, message) in history.iter().enumerate().rev() {
        accumulated += estimate_tokens(message);
        if accumulated >= KEEP_RECENT_TOKENS {
            // The nearest valid cut at or after the overrun point.
            cut = (i..history.len())
                .find(|&c| history[c].role != "tool")
                .unwrap_or(history.len());
            break;
        }
    }
    (history[..cut].to_vec(), history[cut..].to_vec())
}

/// One request, one summary. Errors are the provider's message, verbatim.
pub async fn summarize(model: Model, history: &[ChatMessage]) -> Result<String, String> {
    let request = Request {
        model,
        system: SYSTEM.into(),
        messages: vec![ChatMessage::user(format!(
            "{}\n\n{INSTRUCTION}",
            transcript(history)
        ))],
        effort: None,
        tools: Vec::new(),
    };
    let (mut rx, _handle) = provider::stream(request);
    let mut summary = String::new();
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta(d) => summary.push_str(&d),
            Event::Error { message, .. } => return Err(message),
            Event::Done => break,
            _ => {}
        }
    }
    if summary.trim().is_empty() {
        return Err("the model returned an empty summary".into());
    }
    Ok(summary.trim().to_string())
}

/// The first message of the fresh session, carrying the summary forward.
pub fn seed(summary: &str) -> String {
    format!(
        "This session continues from an earlier one that was compacted. Summary of the work so far:\n\n{summary}\n\nThe most recent messages follow verbatim."
    )
}

/// Role-labeled plain text of the given history, tool results trimmed.
fn transcript(history: &[ChatMessage]) -> String {
    let mut out = String::new();
    for message in history {
        let content = message.content.trim();
        if message.role == "tool" {
            let kept: String = content.chars().take(TOOL_RESULT_KEEP).collect();
            let marker = if content.chars().count() > TOOL_RESULT_KEEP {
                "\n[trimmed]"
            } else {
                ""
            };
            out.push_str(&format!("tool result:\n{kept}{marker}\n\n"));
            continue;
        }
        if content.is_empty() && message.tool_calls.is_empty() {
            continue;
        }
        out.push_str(&format!("{}:\n{content}\n", message.role));
        for call in &message.tool_calls {
            out.push_str(&format!("[called {} {}]\n", call.name, call.arguments));
        }
        out.push('\n');
    }
    out
}
