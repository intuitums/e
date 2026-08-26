//! The e home: `~/.e/`, the single place all of e's state lives.
//!
//! Its formats are open conventions other tools can read: AGENTS.md, SKILL.md
//! directories, and JSONL sessions. e does not read another tool's
//! configuration or state at runtime.

use std::path::PathBuf;

pub fn home() -> PathBuf {
    if let Ok(custom) = std::env::var("E_HOME") {
        return PathBuf::from(custom);
    }
    let base = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join(".e")
}

pub fn settings_path() -> PathBuf {
    home().join("settings.json")
}
pub fn auth_path() -> PathBuf {
    home().join("auth.json")
}
pub fn agents_md_path() -> PathBuf {
    home().join("AGENTS.md")
}
pub fn sessions_dir() -> PathBuf {
    home().join("sessions")
}
pub fn extensions_dir() -> PathBuf {
    home().join("extensions")
}
pub fn skills_dir() -> PathBuf {
    home().join("skills")
}
pub fn prompts_dir() -> PathBuf {
    home().join("prompts")
}
pub fn themes_dir() -> PathBuf {
    home().join("themes")
}
pub fn keybindings_path() -> PathBuf {
    home().join("keybindings.json")
}

/// Make sure the home directory exists before a write lands in it, seeding
/// one bare `AGENTS.md` — the signpost for global instructions, which unlike
/// themes or skills has no command that creates it. It stays empty: anything
/// readable in it becomes system-prompt instructions (see context.rs), so
/// there is no template to ship. Everything else appears when first written,
/// so every other entry in `~/.e` is something the user (or a session) caused.
pub fn ensure() -> std::io::Result<()> {
    std::fs::create_dir_all(home())?;
    let agents = agents_md_path();
    if !agents.exists() {
        std::fs::File::create(&agents)?;
    }
    Ok(())
}
