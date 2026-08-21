//! The e home: `~/.e/`, the single place all of e's state lives.
//!
//! Sovereignty with open formats (DESIGN.md §2): everything here is a
//! convention other tools could read — AGENTS.md, SKILL.md directories,
//! JSONL sessions — and nothing outside this directory is consulted at
//! runtime. Migration from other tools is explicit via `e import`.

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

/// Create the home skeleton if absent. Idempotent; never touches contents.
pub fn ensure() -> std::io::Result<()> {
    for dir in [
        home(),
        sessions_dir(),
        skills_dir(),
        prompts_dir(),
        themes_dir(),
        extensions_dir(),
    ] {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}
