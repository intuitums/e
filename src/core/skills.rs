//! Skills: `SKILL.md` directories under `~/.e/skills/`.
//!
//! Each skill is a folder with a `SKILL.md`: YAML-ish frontmatter (name,
//! description, optional `disable-model-invocation`) then a markdown body.
//! Auto-invocable skills are advertised in the system prompt as a
//! name+description catalog; the model pages a body in with the `skill` tool.
//! `$` in the composer opens a picker over all of them.

use std::path::PathBuf;

use crate::core::home;

pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    /// When true, only the user may invoke it (`$name`), never the model.
    pub disable_model_invocation: bool,
}

pub fn list() -> Vec<Skill> {
    let dir = home::skills_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut skills: Vec<Skill> = entries
        .flatten()
        .filter_map(|e| load(&e.path().join("SKILL.md")))
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub fn get(name: &str) -> Option<Skill> {
    list().into_iter().find(|s| s.name == name)
}

fn load(path: &PathBuf) -> Option<Skill> {
    let text = std::fs::read_to_string(path).ok()?;
    let (frontmatter, body) = split_frontmatter(&text);
    let mut name = path.parent()?.file_name()?.to_string_lossy().into_owned();
    let mut description = String::new();
    let mut disable = false;
    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else { continue };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "name" => name = value.to_string(),
            "description" => description = value.to_string(),
            "disable-model-invocation" => disable = value == "true",
            _ => {}
        }
    }
    Some(Skill { name, description, body: body.trim().to_string(), disable_model_invocation: disable })
}

fn split_frontmatter(text: &str) -> (String, String) {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            return (rest[..end].to_string(), rest[end + 4..].to_string());
        }
    }
    (String::new(), text.to_string())
}

/// The catalog line for the system prompt (auto-invocable skills only).
pub fn catalog() -> Option<String> {
    let auto: Vec<Skill> = list().into_iter().filter(|s| !s.disable_model_invocation).collect();
    if auto.is_empty() {
        return None;
    }
    let mut out = String::from(
        "<skills>\nAvailable skills — call the `skill` tool with the name to load one:\n",
    );
    for s in auto {
        out.push_str(&format!("- {}: {}\n", s.name, s.description));
    }
    out.push_str("</skills>");
    Some(out)
}
