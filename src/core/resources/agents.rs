//! Agents: delegated-turn personas as `~/.e/agents/<name>.md` and, for trusted
//! directories, `<repo>/.e/agents/<name>.md`.
//!
//! An agent file is a markdown persona with YAML-ish frontmatter — `name`,
//! `description`, an optional `tools` allowlist, an optional `model` override —
//! and a body that is the delegated turn's system prompt. The format matches
//! the reference design's own agent files, so personas are portable between
//! harnesses.
//!
//! Discovery mirrors skills and prompts exactly: global personas always load;
//! a repo's own `.e/agents/` loads only when the directory is trusted, and a
//! repo persona shadows a global one of the same name (the closer context
//! wins). This reuses e's per-directory trust boundary — a project persona can
//! steer the model to read files or run bash, so an untrusted repo's personas
//! stay out of reach with no separate confirmation to maintain.

use std::path::{Path, PathBuf};

use crate::core::config::{home, trust};

/// A delegated-turn persona. `tools` is a positive allowlist (a delegated
/// turn sees only these built-ins); `None` means the full toolset. `model`
/// and effort fall back to the dispatching session's when omitted.
pub struct Agent {
    pub name: String,
    pub description: String,
    pub tools: Option<Vec<String>>,
    pub model: Option<String>,
    pub system_prompt: String,
    /// Where the persona was defined, so callers can report which one ran.
    pub source: Source,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    User,
    Project,
}

/// Global personas plus, when `cwd` is trusted, its own `.e/agents/`; on a
/// name clash the repo's persona wins.
pub fn list(cwd: &Path) -> Vec<Agent> {
    let mut agents = read_dir(&home::agents_dir(), Source::User);
    if trust::trusted(cwd) {
        let local = read_dir(&cwd.join(".e").join("agents"), Source::Project);
        agents.retain(|g| !local.iter().any(|l| l.name == g.name));
        agents.extend(local);
    }
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    agents
}

pub fn get(name: &str, cwd: &Path) -> Option<Agent> {
    list(cwd).into_iter().find(|a| a.name == name)
}

fn read_dir(dir: &Path, source: Source) -> Vec<Agent> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
        .filter_map(|e| load(&e.path(), source))
        .collect()
}

fn load(path: &Path, source: Source) -> Option<Agent> {
    let text = std::fs::read_to_string(path).ok()?;
    let (frontmatter, body) = split_frontmatter(&text);
    let mut name = path.file_stem()?.to_string_lossy().into_owned();
    let mut description = String::new();
    let mut tools = None;
    let mut model = None;
    for (key, value) in parse_frontmatter(&frontmatter) {
        match key.as_str() {
            "name" => name = value,
            "description" => description = value,
            "tools" => tools = parse_tool_list(&value),
            "model" => model = (!value.is_empty()).then_some(value),
            _ => {}
        }
    }
    Some(Agent {
        name,
        description,
        tools,
        model,
        system_prompt: body.trim().to_string(),
        source,
        path: path.to_path_buf(),
    })
}

/// A frontmatter `tools` value, in either spelling — both are valid YAML and
/// both appear in real files — yields the same allowlist:
///
/// ```text
/// tools: read, grep       # comma-separated string
/// tools: [read, grep]     # inline list
/// ```
///
/// Empty or malformed yields `None` (the full toolset) rather than an empty
/// allowlist that would silently mute a turn.
fn parse_tool_list(value: &str) -> Option<Vec<String>> {
    let inner = value.trim().trim_start_matches('[').trim_end_matches(']');
    let tools: Vec<String> = inner
        .split(',')
        .map(|t| t.trim().trim_matches('"').to_string())
        .filter(|t| !t.is_empty())
        .collect();
    (!tools.is_empty()).then_some(tools)
}

/// YAML-ish field parsing, single-spaced folding of indented continuations —
/// the same shape skills use, so a multi-line `description:` survives.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_accepts_both_yaml_spellings_and_rejects_empty() {
        assert_eq!(
            parse_tool_list("read, grep, bash"),
            Some(vec!["read".into(), "grep".into(), "bash".into()])
        );
        assert_eq!(
            parse_tool_list("[read, grep]"),
            Some(vec!["read".into(), "grep".into()])
        );
        assert_eq!(parse_tool_list(""), None);
        assert_eq!(parse_tool_list("  ,  "), None);
    }

    #[test]
    fn load_reads_frontmatter_and_body_with_name_from_filename() {
        let dir = std::env::temp_dir().join(format!("e-agents-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scout.md");
        std::fs::write(
            &path,
            "---\ndescription: Fast recon\ntools: read, grep\nmodel: p/m\n---\nYou are a scout.\n",
        )
        .unwrap();

        let agent = load(&path, Source::User).unwrap();
        assert_eq!(agent.name, "scout"); // from the filename, no `name:` field
        assert_eq!(agent.description, "Fast recon");
        assert_eq!(
            agent.tools.as_deref(),
            Some(&["read".into(), "grep".into()][..])
        );
        assert_eq!(agent.model.as_deref(), Some("p/m"));
        assert_eq!(agent.system_prompt, "You are a scout.");
        assert_eq!(agent.source, Source::User);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn a_persona_with_no_tools_field_is_unrestricted() {
        let dir = std::env::temp_dir().join(format!("e-agents-notools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("worker.md");
        std::fs::write(&path, "---\ndescription: General\n---\nDo the work.\n").unwrap();

        let agent = load(&path, Source::User).unwrap();
        assert_eq!(agent.tools, None); // None means the full toolset, not an empty allowlist
        std::fs::remove_file(&path).ok();
    }
}
