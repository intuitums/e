//! `/compact`: summarize the session so work can continue in a fresh context.
//!
//! The history is flattened to a plain transcript and sent to the current
//! model as one message — no tools, no dialect-specific tool-message shapes —
//! and the reply becomes the seed of a fresh session. The old session file is
//! untouched and stays resumable in full.

use crate::core::model::Model;
use crate::core::provider::{self, ChatMessage, Event, Request};

/// Tool results are trimmed in the flattened transcript; the summary needs
/// what they meant, not their full bytes.
const TOOL_RESULT_KEEP: usize = 1500;

const SYSTEM: &str = "You summarize a coding-agent session so it can continue in a fresh context window. Write a dense, factual summary that preserves everything needed to keep working: the user's goals and standing decisions, the current state of the work, file paths and key symbols touched, what was tried and what failed, and the concrete next steps. Use plain prose and short lists. Do not add commentary about the summarization itself.";

const INSTRUCTION: &str = "Summarize the session above for continuation. Lead with the user's goal and standing instructions, then current state, then next steps.";

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
        "This session continues from an earlier one that was compacted. Summary of the work so far:\n\n{summary}"
    )
}

/// Role-labeled plain text of the whole history, tool results trimmed.
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
