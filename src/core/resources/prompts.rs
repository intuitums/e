//! Prompt templates: `~/.e/prompts/<name>.md` becomes the `/name` command,
//! and a trusted repo's `.e/prompts/<name>.md` adds `/name` too — shadowing a
//! global template of the same name, the closer context wins.
//!
//! Frontmatter (optional): `description:` for the picker, `argument-hint:`
//! shown after the name. The body is submitted as the prompt after bash-style
//! substitution: `$1`..`$9`, `$@` / `$ARGUMENTS` (all args), `${N:-default}`,
//! `${@:-default}`, and `${@:N}` (args from N on).

use std::path::Path;

use crate::core::config::{home, trust};

pub struct Template {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    pub content: String,
}

/// Global templates plus, when `cwd` is trusted, its own `.e/prompts/`; on
/// a name clash the repo's template wins.
pub fn list(cwd: &Path) -> Vec<Template> {
    let mut templates = read_dir(&home::prompts_dir());
    if trust::trusted(cwd) {
        let local = read_dir(&cwd.join(".e").join("prompts"));
        templates.retain(|g| !local.iter().any(|l| l.name == g.name));
        templates.extend(local);
    }
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    templates
}

pub fn find(name: &str, cwd: &Path) -> Option<Template> {
    list(cwd).into_iter().find(|t| t.name == name)
}

fn read_dir(dir: &Path) -> Vec<Template> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "md").unwrap_or(false))
        .filter_map(|e| {
            let name = e.path().file_stem()?.to_string_lossy().into_owned();
            let raw = std::fs::read_to_string(e.path()).ok()?;
            Some(parse(name, &raw))
        })
        .collect()
}

fn parse(name: String, raw: &str) -> Template {
    let mut description = String::new();
    let mut argument_hint = String::new();
    let mut content = raw;
    if let Some(rest) = raw.strip_prefix("---\n") {
        if let Some(end) = rest.find("\n---") {
            for line in rest[..end].lines() {
                if let Some(v) = line.strip_prefix("description:") {
                    description = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("argument-hint:") {
                    argument_hint = v.trim().to_string();
                }
            }
            content = rest[end + 4..].trim_start_matches('\n');
        }
    }
    Template {
        name,
        description,
        argument_hint,
        content: content.trim().to_string(),
    }
}

/// Bash-style argument substitution over a template body. `args` is the raw
/// text after the command name; words split on whitespace, quotes respected.
pub fn substitute(content: &str, args: &str) -> String {
    let words = split_args(args);
    let all = words.join(" ");
    let mut out = String::new();
    let mut chars = content.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('@') => {
                chars.next();
                out.push_str(&all);
            }
            Some('{') => {
                chars.next();
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == '}' {
                        break;
                    }
                    inner.push(c);
                }
                out.push_str(&expand_braced(&inner, &words, &all));
            }
            Some(d) if d.is_ascii_digit() => {
                let n = chars.next().unwrap().to_digit(10).unwrap() as usize;
                out.push_str(
                    words
                        .get(n.wrapping_sub(1))
                        .map(String::as_str)
                        .unwrap_or(""),
                );
            }
            _ => {
                // $ARGUMENTS, or a literal dollar.
                let rest: String = chars.clone().take(9).collect();
                if rest.starts_with("ARGUMENTS") {
                    for _ in 0..9 {
                        chars.next();
                    }
                    out.push_str(&all);
                } else {
                    out.push('$');
                }
            }
        }
    }
    out
}

/// `${N:-default}`, `${@:-default}`, `${@:N}`, `${ARGUMENTS:-default}`.
fn expand_braced(inner: &str, words: &[String], all: &str) -> String {
    if let Some((name, default)) = inner.split_once(":-") {
        let value = match name {
            "@" | "ARGUMENTS" => all.to_string(),
            n => n
                .parse::<usize>()
                .ok()
                .and_then(|n| words.get(n.wrapping_sub(1)))
                .cloned()
                .unwrap_or_default(),
        };
        return if value.is_empty() {
            default.to_string()
        } else {
            value
        };
    }
    if let Some(from) = inner.strip_prefix("@:") {
        if let Ok(n) = from.parse::<usize>() {
            return words
                .iter()
                .skip(n.saturating_sub(1))
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    match inner {
        "@" | "ARGUMENTS" => all.to_string(),
        n => n
            .parse::<usize>()
            .ok()
            .and_then(|n| words.get(n.wrapping_sub(1)))
            .cloned()
            .unwrap_or_default(),
    }
}

/// Whitespace split with single- and double-quote grouping.
fn split_args(args: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in args.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            (None, c) => current.push(c),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}
