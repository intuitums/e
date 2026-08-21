//! Context assembly: the system prompt and AGENTS.md instructions.
//!
//! The prompt is e's identity plus the working environment, then any
//! AGENTS.md rules: global `~/.e/AGENTS.md` and the project's, in open-spec
//! `<global-rules>` / `<project-rules>` sections.

use std::path::Path;

use crate::core::home;

const IDENTITY: &str = "\
You are e, a coding agent working in a terminal alongside the user.

- Answer in GitHub-flavored markdown; be direct and concise.
- You have tools: read, write, edit, ls, grep, and bash. Use them to inspect \
and change the workspace rather than guessing or asking.
- Prefer small, focused changes. Preserve unrelated code and formatting.
- When you finish a task, stop — don't narrate what you could do next.";

pub fn system_prompt(cwd: &Path) -> String {
    let mut prompt = String::from(IDENTITY);

    prompt.push_str(&format!(
        "\n\n<environment>\nWorking directory: {}\nDate: {}\n</environment>",
        cwd.display(),
        today(),
    ));

    if let Some(rules) = read_trimmed(&home::agents_md_path()) {
        prompt.push_str(&format!("\n\n<global-rules>\n{rules}\n</global-rules>"));
    }
    if let Some(catalog) = crate::core::skills::catalog() {
        prompt.push_str(&format!("\n\n{catalog}"));
    }
    if let Some(rules) = read_trimmed(&cwd.join("AGENTS.md")) {
        prompt.push_str(&format!(
            "\n\n<project-rules from=\"{}/AGENTS.md\">\n{rules}\n</project-rules>",
            xml_escape(&cwd.to_string_lossy()),
        ));
    }
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

/// UTC date, no external time crate — derived from the unix epoch.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
