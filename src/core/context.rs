//! Context assembly: the system prompt, following pi's `buildSystemPrompt`.
//!
//! The base prompt is e's identity, an explicit tools list, and the
//! guidelines. A user's `settings.json` `system_prompt` replaces that base
//! wholesale (pi's `customPrompt` path). Either way, the layered context is
//! appended in pi's order: project instructions (AGENTS.md), then the working
//! directory. AGENTS.md ships as nothing — the user fills it.

use serde::Deserialize;
use std::path::Path;

use crate::core::home;

/// One-line description of each tool, for the Available tools list.
const TOOL_SNIPPETS: &[(&str, &str)] = &[
    ("read", "Read the contents of a file. Use offset/limit for large files."),
    ("write", "Write content to a file, creating it if needed, overwriting if it exists."),
    ("edit", "Replace an exact string in a file; the old text must match once."),
    ("ls", "List the entries of a directory."),
    ("grep", "Search file contents by regular expression across the workspace."),
    ("bash", "Execute a bash command in the working directory. Returns combined output."),
    ("skill", "Load a skill's instructions by name when a listed skill fits the task."),
];

const GUIDELINES: &[&str] = &[
    "Prefer small, focused changes; preserve unrelated code and formatting",
    "Show file paths clearly when working with files",
    "Be concise in your responses",
    "When you finish a task, stop — don't narrate what you could do next",
];

/// The default base: identity, tools, guidelines (pi's shape, e's name).
fn default_base() -> String {
    let tools = TOOL_SNIPPETS
        .iter()
        .map(|(name, snippet)| format!("- {name}: {snippet}"))
        .collect::<Vec<_>>()
        .join("\n");
    let guidelines = GUIDELINES.iter().map(|g| format!("- {g}")).collect::<Vec<_>>().join("\n");
    format!(
        "You are an expert coding assistant operating inside e, a coding agent \
harness. You help users by reading files, executing commands, editing code, \
and writing new files.\n\n\
Available tools:\n{tools}\n\n\
In addition to the tools above, you may have access to other custom tools \
depending on the project.\n\n\
Guidelines:\n{guidelines}"
    )
}

#[derive(Deserialize, Default)]
struct Settings {
    #[serde(default)]
    system_prompt: Option<String>,
}

/// A user override from `~/.e/settings.json`, if a non-empty one is set.
fn custom_prompt() -> Option<String> {
    let json = std::fs::read_to_string(home::settings_path()).ok()?;
    let settings: Settings = serde_json::from_str(&json).ok()?;
    settings.system_prompt.filter(|p| !p.trim().is_empty())
}

pub fn system_prompt(cwd: &Path) -> String {
    let mut prompt = custom_prompt().unwrap_or_else(default_base);

    // Skills catalog (auto-invocable), like pi's skills section.
    if let Some(catalog) = crate::core::skills::catalog() {
        prompt.push_str(&format!("\n\n{catalog}"));
    }

    // Project instructions: AGENTS.md, global then project, in pi's block shape.
    let mut context_files: Vec<(String, String)> = Vec::new();
    if let Some(rules) = read_trimmed(&home::agents_md_path()) {
        context_files.push((home::agents_md_path().to_string_lossy().into_owned(), rules));
    }
    if let Some(rules) = read_trimmed(&cwd.join("AGENTS.md")) {
        context_files.push((cwd.join("AGENTS.md").to_string_lossy().into_owned(), rules));
    }
    if !context_files.is_empty() {
        prompt.push_str("\n\n<project_context>\n\nProject-specific instructions and guidelines:\n\n");
        for (path, content) in context_files {
            prompt.push_str(&format!(
                "<project_instructions path=\"{}\">\n{content}\n</project_instructions>\n\n",
                xml_escape(&path)
            ));
        }
        prompt.push_str("</project_context>\n");
    }

    prompt.push_str(&format!("\nCurrent working directory: {}", cwd.display()));
    prompt
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
