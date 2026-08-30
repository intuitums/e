//! Context assembly: the system prompt, following the reference's
//! layered structure.
//!
//! The base prompt is e's identity, an explicit tools list, and the
//! guidelines. A user's `settings.json` `system_prompt` replaces that base
//! wholesale (the custom-prompt path). Either way, the layered context is
//! appended in the reference's order: project instructions (AGENTS.md), then
//! the working directory. AGENTS.md ships as nothing — the user fills it.

use serde::Deserialize;
use std::path::Path;

use crate::core::config::home;

const GUIDELINES: &[&str] = &[
    "Prefer small, focused changes; preserve unrelated code and formatting",
    "Show file paths clearly when working with files",
    "Be concise in your responses",
    "When you finish a task, stop — don't narrate what you could do next",
];

/// The default base: identity, tools, guidelines (the reference's shape,
/// e's name).
fn default_base() -> String {
    // The tool list comes from the same table that registers the tools —
    // the prompt can't advertise a tool that doesn't exist or miss one that
    // does.
    let tools = crate::core::tools::snippets()
        .map(|(name, snippet)| format!("- {name}: {snippet}"))
        .collect::<Vec<_>>()
        .join("\n");
    let guidelines = GUIDELINES
        .iter()
        .map(|g| format!("- {g}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "You are an expert coding assistant operating inside e, a coding agent \
harness. You help users by reading files, executing commands, editing code, \
and writing new files.\n\n\
Available tools:\n{tools}\n\n\
In addition to the tools above, you may have access to other custom tools \
depending on the project.\n\n\
Guidelines:\n{guidelines}\n\n\
e documentation (read only when the user asks about e itself — its \
extensions, themes, skills, prompt templates, keybindings, or models):\n\
- Run `e docs` to list the built-in guides, `e docs <topic>` to print one\n\
- Topics: extensions (the protocol and a worked example), themes, models, \
prompt-templates, skills, keybindings\n\
- When working on an e topic, print and follow the guide before implementing \
— the formats are exact"
    )
}

#[derive(Deserialize, Default)]
struct Settings {
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    no_tools_notice: Option<String>,
}

fn settings() -> Settings {
    std::fs::read_to_string(home::settings_path())
        .ok()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// A user override from `~/.e/settings.json`, if a non-empty one is set.
fn custom_prompt() -> Option<String> {
    settings().system_prompt.filter(|p| !p.trim().is_empty())
}

/// The suffix appended to the system prompt when no tools are available —
/// a `~/.e/settings.json` `no_tools_notice` overrides it.
pub fn no_tools_notice() -> String {
    settings()
        .no_tools_notice
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "This run has no tools. Answer without attempting tool calls.".into())
}

/// The system prompt for the process's current directory — the entry point
/// for frontends that don't carry a cwd of their own.
pub fn system_prompt_here() -> String {
    system_prompt(&std::env::current_dir().unwrap_or_default())
}

pub fn system_prompt(cwd: &Path) -> String {
    let mut prompt = custom_prompt().unwrap_or_else(default_base);

    // Skills catalog (auto-invocable), like the reference's skills section.
    if let Some(catalog) = crate::core::resources::skills::catalog(cwd) {
        prompt.push_str(&format!("\n\n{catalog}"));
    }

    // Project instructions: AGENTS.md, global then project, in the
    // reference's block shape.
    let mut context_files: Vec<(String, String)> = Vec::new();
    if let Some(rules) = read_trimmed(&home::agents_md_path()) {
        context_files.push((home::agents_md_path().to_string_lossy().into_owned(), rules));
    }
    // The project's own instructions load only for trusted directories — an
    // untrusted repo must not steer the agent through its AGENTS.md.
    if crate::core::config::trust::trusted(cwd) {
        if let Some(rules) = read_trimmed(&cwd.join("AGENTS.md")) {
            context_files.push((cwd.join("AGENTS.md").to_string_lossy().into_owned(), rules));
        }
    }
    if !context_files.is_empty() {
        prompt
            .push_str("\n\n<project_context>\n\nProject-specific instructions and guidelines:\n\n");
        for (path, content) in context_files {
            prompt.push_str(&format!(
                "<project_instructions path=\"{}\">\n{content}\n</project_instructions>\n\n",
                xml_escape(&path)
            ));
        }
        prompt.push_str("</project_context>\n");
    }

    // A path is untrusted metadata even before its workspace is trusted.
    // Use JSON's escaping but omit its surrounding quotes, keeping ordinary
    // paths unchanged while control characters can never add prompt lines.
    let cwd_json =
        serde_json::to_string(cwd.to_string_lossy().as_ref()).unwrap_or_else(|_| "\"\"".into());
    let cwd_escaped = cwd_json
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(&cwd_json);
    prompt.push_str(&format!("\nCurrent working directory: {cwd_escaped}"));
    // Environment facts the model would otherwise guess (and guess wrong —
    // models confidently write their training cutoff's date into changelogs).
    prompt.push_str(&format!("\nPlatform: {}", std::env::consts::OS));
    if let Some(date) = utc_date() {
        prompt.push_str(&format!("\nToday's date: {date} (UTC)"));
    }
    prompt
}

/// Today as `YYYY-MM-DD` UTC, from the system clock alone (no date crate:
/// civil-from-days, Howard Hinnant's algorithm).
fn utc_date() -> Option<String> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
