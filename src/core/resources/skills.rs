//! Skills: `SKILL.md` directories under `~/.e/skills/` and, for trusted
//! directories, `<repo>/.e/skills/`.
//!
//! Each skill is a folder with a `SKILL.md`: YAML-ish frontmatter (name,
//! description, optional `disable-model-invocation`) then a markdown body.
//! Progressive disclosure, the reference design's way: auto-invocable skills
//! are advertised in the system prompt as a catalog of name, description, and
//! the SKILL.md path; the model pages a body in with the ordinary `read` tool
//! — no dedicated skill tool. `$` in the composer opens a picker over all of
//! them. A repo skill shadows a global skill of the same name — the closer
//! context wins.

use std::path::Path;

use crate::core::config::{home, trust};

pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    /// When true, the skill stays out of the system-prompt catalog — only
    /// the user reaches it, through the `$name` picker.
    pub disable_model_invocation: bool,
    /// The skill's own folder: a body that references bundled files
    /// (references, templates, scripts) is useless without it.
    pub dir: std::path::PathBuf,
}

/// Global skills plus, when `cwd` is trusted, its own `.e/skills/`; on a
/// name clash the repo's skill wins.
pub fn list(cwd: &Path) -> Vec<Skill> {
    let mut skills = read_dir(&home::skills_dir());
    if trust::trusted(cwd) {
        let local = read_dir(&cwd.join(".e").join("skills"));
        skills.retain(|g| !local.iter().any(|l| l.name == g.name));
        skills.extend(local);
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub fn get(name: &str, cwd: &Path) -> Option<Skill> {
    list(cwd).into_iter().find(|s| s.name == name)
}

fn read_dir(dir: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| load(&e.path().join("SKILL.md")))
        .collect()
}

fn load(path: &Path) -> Option<Skill> {
    let text = std::fs::read_to_string(path).ok()?;
    let (frontmatter, body) = split_frontmatter(&text);
    let mut name = path.parent()?.file_name()?.to_string_lossy().into_owned();
    let mut description = String::new();
    let mut disable = false;
    for (key, value) in parse_frontmatter(&frontmatter) {
        match key.as_str() {
            "name" => name = value,
            "description" => description = value,
            "disable-model-invocation" => disable = value == "true",
            _ => {}
        }
    }
    Some(Skill {
        name,
        description,
        body: body.trim().to_string(),
        disable_model_invocation: disable,
        dir: path.parent()?.to_path_buf(),
    })
}

/// YAML-ish field parsing that survives the multi-line descriptions the
/// SKILL.md convention encourages: block scalars (`description: >` / `|`,
/// with an optional chomp sign) and indented plain-scalar continuations both
/// fold into the value, single-spaced. Descriptions render on one-line
/// surfaces (the catalog, the `$` picker rows), so folding loses nothing.
fn parse_frontmatter(frontmatter: &str) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in frontmatter.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            let text = line.trim();
            if let Some((_, value)) = fields.last_mut() {
                if !text.is_empty() {
                    if !value.is_empty() {
                        value.push(' ');
                    }
                    value.push_str(text);
                }
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        let value = match value {
            "|" | ">" | "|-" | ">-" | "|+" | ">+" => "",
            v => v,
        };
        fields.push((key.trim().to_string(), value.to_string()));
    }
    fields
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

/// The catalog for the system prompt (auto-invocable skills only): name,
/// description, and where the full instructions live. Only this much stays
/// in context; the model reads a SKILL.md when its skill matches the task —
/// and because it loads by path, the skill's own directory (references,
/// templates, scripts riding along) is never a mystery.
pub fn catalog(cwd: &Path) -> Option<String> {
    let auto: Vec<Skill> = list(cwd)
        .into_iter()
        .filter(|s| !s.disable_model_invocation)
        .collect();
    if auto.is_empty() {
        return None;
    }
    let mut out = String::from(
        "<skills>\nAvailable skills. When one matches the task, read its SKILL.md \
with the `read` tool and follow it; files a skill references live beside its \
SKILL.md:\n",
    );
    for s in auto {
        out.push_str(&format!(
            "- {}: {} ({})\n",
            s.name,
            s.description,
            s.dir.join("SKILL.md").display()
        ));
    }
    out.push_str("</skills>");
    Some(out)
}
