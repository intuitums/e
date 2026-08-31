//! The built-in documentation, embedded in the binary and served by
//! `e docs [topic]` — a single binary has no package directory to point at,
//! so the binary itself is the docs carrier. The system prompt tells the
//! agent to run it when asked about e's own surfaces.

pub const TOPICS: &[(&str, &str)] = &[
    (
        "extensions",
        "the extension protocol, with a worked shell example",
    ),
    (
        "themes",
        "theme JSON format; file wins over a built-in name",
    ),
    (
        "models",
        "models.json: extra models, context windows, dialects",
    ),
    (
        "automation",
        "JSON output and the long-lived JSONL RPC protocol",
    ),
    (
        "sdk",
        "the e-sdk crate: embed the agent core in Rust programs",
    ),
    (
        "prompt-templates",
        "/name templates with bash-style arguments",
    ),
    (
        "skills",
        "SKILL.md directories and how the model pages them in",
    ),
    (
        "keybindings",
        "keybindings.json: override the composer's editing keys",
    ),
    ("sandboxing", "e's trust model and how to isolate a session"),
    (
        "theme-dark",
        "the built-in dark theme, verbatim (a starting point)",
    ),
    ("theme-light", "the built-in light theme, verbatim"),
];

pub fn body(topic: &str) -> Option<&'static str> {
    Some(match topic {
        "extensions" => include_str!("../../../docs/extensions.md"),
        "themes" => include_str!("../../../docs/themes.md"),
        "models" => include_str!("../../../docs/models.md"),
        "automation" => include_str!("../../../docs/automation.md"),
        "sdk" => include_str!("../../../docs/sdk.md"),
        "prompt-templates" => include_str!("../../../docs/prompt-templates.md"),
        "skills" => include_str!("../../../docs/skills.md"),
        "keybindings" => include_str!("../../../docs/keybindings.md"),
        "sandboxing" => include_str!("../../../docs/sandboxing.md"),
        "theme-dark" => crate::tui::theme::DARK_JSON,
        "theme-light" => crate::tui::theme::LIGHT_JSON,
        _ => return None,
    })
}
