//! Compaction: summarize the older part of the session so work continues in a
//! fresh context, keeping the recent messages verbatim.
//!
//! The core compacts between provider requests, after every tool result has
//! been committed. Both interactive and headless runs use the same path.
//! The cut keeps roughly the most recent `KEEP_RECENT_TOKENS` of messages and
//! never lands on a tool result (a result must follow its call) or between a
//! signed thinking block and the assistant turn it precedes; everything
//! before the cut is flattened to plain text and summarized with a structured
//! checkpoint prompt. The summary seeds a fresh session file, followed by the
//! kept messages — the old session stays fully resumable.

use crate::core::providers::catalog::Model;
use crate::core::providers::{self, ChatMessage, Event, Request};

/// Ceiling of the auto-compact reserve (large windows).
pub const RESERVE_TOKENS: u64 = 16_384;

/// Ceiling of the keep-recent budget (large windows).
pub const KEEP_RECENT_TOKENS: u64 = 20_000;

/// Headroom kept free before auto-compact fires: an eighth of the window,
/// bounded. A fixed 16k reserve was tuned for 200k windows — against a 32k
/// local model it triggered at half the usable context.
pub fn reserve_tokens(context_window: u64) -> u64 {
    (context_window / 8).clamp(2_048.min(context_window), RESERVE_TOKENS)
}

/// Budget of recent messages kept verbatim through a compact: a quarter of
/// the window, bounded. The fixed 20k budget was larger than some whole
/// windows, which made compaction a no-op exactly where it was needed most.
pub fn keep_recent_tokens(context_window: u64) -> u64 {
    KEEP_RECENT_TOKENS.min((context_window / 4).max(1_024))
}

/// Tool results are trimmed in the flattened transcript; the summary needs
/// what they meant, not their full bytes.
const TOOL_RESULT_KEEP: usize = 1500;
const TOOL_CALL_KEEP: usize = 1500;

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

/// True when the session has grown into the reserve headroom. A window of 0
/// (a broken user-declared model) never compacts: the threshold would sit at
/// zero and every turn would end in a pointless compaction loop.
pub fn should_compact(context_tokens: u64, context_window: u64) -> bool {
    context_window > 0
        && context_tokens > context_window.saturating_sub(reserve_tokens(context_window))
}

/// Approximate tokens for cut placement and providers that omit usage.
/// This heuristic is not a tokenizer; real provider usage takes precedence.
pub fn estimate_message_tokens(message: &ChatMessage) -> u64 {
    let mut chars = message.content.chars().count();
    for call in message.tool_calls() {
        chars += call.name.chars().count() + call.arguments.chars().count();
    }
    (chars as u64)
        .div_ceil(4)
        .saturating_add(message.images().len() as u64 * 1_000)
}

/// Provider-independent fallback for gateways that omit usage. It is visibly
/// treated as an estimate by the TUI; its purpose is safety headroom, not
/// billing accuracy.
pub fn estimate_request_tokens(system: &str, history: &[ChatMessage]) -> u64 {
    let system = (system.chars().count() as u64).div_ceil(4);
    history.iter().fold(system, |total, message| {
        total.saturating_add(estimate_message_tokens(message))
    })
}

/// Split history into (to_summarize, kept): walk backwards accumulating
/// estimated tokens until the keep budget (window-relative) is reached, then
/// cut at the nearest valid boundary after the over-budget message. A tool result
/// always stays with its call, and a signed thinking block ("reasoning")
/// always stays with the assistant turn it precedes; replaying either apart
/// from its partner fails the request. Returns an empty `to_summarize` when
/// the history is small enough that compaction would gain nothing.
pub fn split(history: &[ChatMessage], context_window: u64) -> (Vec<ChatMessage>, Vec<ChatMessage>) {
    let keep = keep_recent_tokens(context_window);
    let mut accumulated = 0u64;
    let mut cut = 0usize;
    for (i, message) in history.iter().enumerate().rev() {
        accumulated += estimate_message_tokens(message);
        if accumulated >= keep {
            // Exclude the message that exceeded the budget. Never cut on a
            // tool result, and not between a reasoning block and the
            // assistant message whose signature it carries.
            cut = (i + 1..history.len())
                .find(|&c| {
                    history[c].role() != "tool"
                        && !(history[c].role() == "reasoning"
                            && c > 0
                            && history[c - 1].role() == "reasoning")
                        && !(history[c].role() == "assistant"
                            && c > 0
                            && history[c - 1].role() == "reasoning")
                })
                .unwrap_or(history.len());
            break;
        }
    }
    (history[..cut].to_vec(), history[cut..].to_vec())
}

/// One request, one summary. Errors are the provider's message, verbatim.
/// `session_id` carries the conversation's stable id so the summarization
/// request rides the same gateway session as the turn it compacts (empty for
/// an unsaved session).
pub async fn summarize(
    model: Model,
    history: &[ChatMessage],
    session_id: String,
) -> Result<String, String> {
    let flattened = budget_transcript(history, model.context_window)?;
    let request = Request {
        model,
        system: SYSTEM.into(),
        messages: vec![ChatMessage::user(format!("{flattened}\n\n{INSTRUCTION}"))],
        effort: None,
        session_id,
        tools: Vec::new(),
    };
    let (mut rx, handle) = providers::stream(request);
    let _stream = StreamGuard(handle);
    let mut summary = String::new();
    let mut complete = false;
    while let Some(event) = rx.recv().await {
        match event {
            Event::TextDelta(d) => summary.push_str(&d),
            Event::Error(err) => return Err(err.message),
            Event::Done(end) => {
                if !matches!(end.finish, providers::FinishReason::Normal) || end.malformed > 0 {
                    return Err("compaction did not produce a complete, valid response".into());
                }
                complete = true;
                break;
            }
            _ => {}
        }
    }
    if !complete {
        return Err("compaction stream closed before completion".into());
    }
    if summary.trim().is_empty() {
        return Err("the model returned an empty summary".into());
    }
    Ok(summary.trim().to_string())
}

/// Cancel the provider task when a summarization future is dropped.
struct StreamGuard(tokio::task::JoinHandle<()>);
impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Preserve every user instruction and previous checkpoint before spending
/// remaining space on recent execution details. Refuse to compact when the
/// protected text alone cannot fit; silently deleting instructions is worse.
fn budget_transcript(history: &[ChatMessage], window: u64) -> Result<String, String> {
    let budget = usize::try_from(window.saturating_mul(2))
        .unwrap_or(usize::MAX)
        .saturating_sub(SYSTEM.len() + INSTRUCTION.len() + 512);
    let segments: Vec<(bool, String)> = history
        .iter()
        .flat_map(|message| {
            transcript_segments(std::slice::from_ref(message))
                .into_iter()
                .map(move |text| (message.role() == "user" || message.role() == "system", text))
        })
        .collect();
    let protected: usize = segments
        .iter()
        .filter(|(keep, _)| *keep)
        .map(|(_, text)| text.len())
        .sum();
    if protected > budget {
        return Err("user instructions and previous checkpoints exceed the compaction budget; history was preserved".into());
    }
    let mut remaining = budget - protected;
    let mut selected = vec![false; segments.len()];
    for (index, (protected, text)) in segments.iter().enumerate().rev() {
        if *protected {
            selected[index] = true;
        } else if text.len() <= remaining {
            selected[index] = true;
            remaining -= text.len();
        }
    }
    let mut out = if selected.iter().any(|selected| !selected) {
        "[Some execution details omitted to fit; user instructions and checkpoints are intact.]\n\n"
            .to_string()
    } else {
        String::new()
    };
    for ((_, text), selected) in segments.into_iter().zip(selected) {
        if selected {
            out.push_str(&text);
        }
    }
    Ok(out)
}

/// The first message of the fresh session, carrying the summary forward.
pub fn seed(summary: &str) -> String {
    format!(
        "This session continues from an earlier one that was compacted. Summary of the work so far:\n\n{summary}\n\nThe most recent messages follow verbatim."
    )
}

/// Role-labeled plain text of the given history, one segment per message,
/// tool results trimmed. Segments let the caller drop the oldest whole
/// messages when the flatten must fit a budget.
fn transcript_segments(history: &[ChatMessage]) -> Vec<String> {
    let mut out = Vec::new();
    for message in history {
        let content = message.content.trim();
        if message.role() == "reasoning" {
            continue; // encrypted provider state, not conversation
        }
        if message.role() == "tool" {
            let kept: String = content.chars().take(TOOL_RESULT_KEEP).collect();
            let marker = if content.chars().count() > TOOL_RESULT_KEEP {
                "\n[trimmed]"
            } else {
                ""
            };
            out.push(format!("tool result:\n{kept}{marker}\n\n"));
            continue;
        }
        if content.is_empty() && message.tool_calls().is_empty() {
            continue;
        }
        let mut segment = format!("{}:\n{content}\n", message.role());
        if !message.images().is_empty() {
            segment.push_str(&format!(
                "[{} image attachment{}]\n",
                message.images().len(),
                if message.images().len() == 1 { "" } else { "s" }
            ));
        }
        for call in message.tool_calls() {
            let arguments: String = call.arguments.chars().take(TOOL_CALL_KEEP).collect();
            let marker = if call.arguments.chars().count() > TOOL_CALL_KEEP {
                "… [arguments trimmed]"
            } else {
                ""
            };
            segment.push_str(&format!("[called {} {arguments}{marker}]\n", call.name));
        }
        segment.push('\n');
        out.push(segment);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_preserves_original_constraints_and_previous_checkpoint() {
        let history = vec![
            ChatMessage::user(seed(
                "The original checkpoint: keep public APIs compatible.",
            )),
            ChatMessage::user("Never change the database schema."),
            ChatMessage::assistant("noise".repeat(20_000), Vec::new()),
            ChatMessage::assistant("Recent work finished.", Vec::new()),
        ];
        let text = budget_transcript(&history, 4_000).unwrap();
        assert!(text.contains("keep public APIs compatible"));
        assert!(text.contains("Never change the database schema"));
        assert!(text.contains("Recent work finished"));
        assert!(text.len() < 8_000);
    }

    #[test]
    fn oversized_instructions_fail_instead_of_being_trimmed() {
        assert!(
            budget_transcript(&[ChatMessage::user("constraint".repeat(2_000))], 4_000).is_err()
        );
    }
}
