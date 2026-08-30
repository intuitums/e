//! Built-in command-line options.
//!
//! Parsing lives in the terminal-free core so the binary, tests, and future
//! machine protocol all share one contract. Long descriptive names are the
//! canonical surface; compact aliases normalize to the same fields.
//!
//! Unknown flags are errors, not prompt text: extensions consume their
//! declared flags before parsing runs, so anything left over is a typo.
//! `--` remains the escape hatch that turns flag-spelling text into a
//! prompt, and near misses get a did-you-mean suggestion.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolMode {
    #[default]
    All,
    ReadOnly,
    None,
}

impl ToolMode {
    pub fn allows(self, name: &str) -> bool {
        match self {
            ToolMode::All => true,
            // Gate by the known-safe built-ins, never by name alone: an
            // extension could ship a mutating tool under any name, so only
            // the tools whose behaviour we control earn read-only trust.
            ToolMode::ReadOnly => matches!(name, "read" | "grep"),
            ToolMode::None => false,
        }
    }

    /// Combine a process-wide ceiling with a per-request preference. A
    /// nested protocol may remove capabilities, but must never restore ones
    /// disabled by the process that owns it.
    pub fn restrict(self, requested: Self) -> Self {
        match (self, requested) {
            (Self::None, _) | (_, Self::None) => Self::None,
            (Self::ReadOnly, _) | (_, Self::ReadOnly) => Self::ReadOnly,
            (Self::All, Self::All) => Self::All,
        }
    }
}

/// Built-in flags that consume the following token when one is present.
const VALUE_FLAGS: &[&str] = &["--model", "-m", "--effort", "--ef", "--image", "-i"];

/// Every built-in flag name, canonical and alias: suggestion candidates and
/// the bool/value split for raw-argv scans.
const ALL_FLAGS: &[&str] = &[
    "--no-extensions",
    "--ne",
    "--no-save",
    "--ns",
    "--read-only",
    "--ro",
    "--no-tools",
    "--nt",
    "--no-network",
    "--json",
    "-j",
    "--model",
    "-m",
    "--effort",
    "--ef",
    "--image",
    "-i",
    "--continue",
    "-c",
    "--resume",
    "-r",
    "--help",
    "-h",
    "--version",
    "-v",
];

/// Every subcommand word, in help order.
pub const SUBCOMMANDS: &[&str] = &[
    "ask",
    "rpc",
    "docs",
    "update",
    "auth",
    "doctor",
    "providers",
];

/// One-line usage for a subcommand, so error messages can point somewhere
/// actionable instead of at generic help.
pub fn subcommand_usage(sub: &str) -> Option<&'static str> {
    match sub {
        "ask" => Some("usage: e ask \"prompt\""),
        "rpc" => Some("usage: e rpc"),
        "docs" => Some("usage: e docs [topic]"),
        "update" => Some("usage: e update"),
        "auth" => Some("usage: e auth"),
        "doctor" => Some("usage: e doctor [--no-network]"),
        "providers" => Some("usage: e providers"),
        _ => None,
    }
}

/// Optimal-string-alignment edit distance: insert, delete, substitute, and
/// adjacent transposition each cost 1. Transpositions matter because flag
/// typos are overwhelmingly transpositions (`--modle`, `--hlep`).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut rows: Vec<Vec<usize>> = (0..=a.len())
        .map(|i| {
            let mut row = vec![0; b.len() + 1];
            row[0] = i;
            row
        })
        .collect();
    for (j, cell) in rows[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let substitute = rows[i - 1][j - 1] + usize::from(a[i - 1] != b[j - 1]);
            let mut best = substitute.min(rows[i - 1][j] + 1).min(rows[i][j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(rows[i - 2][j - 2] + 1);
            }
            rows[i][j] = best;
        }
    }
    rows[a.len()][b.len()]
}

/// The closest candidate within one edit, for did-you-mean hints. Exact
/// matches are not suggestions — the caller handles those as known words.
pub fn did_you_mean(input: &str, candidates: &[String]) -> Option<String> {
    let mut best: Option<(usize, &String)> = None;
    for candidate in candidates {
        let distance = edit_distance(input, candidate);
        if distance > 0 && distance <= 1 && best.is_none_or(|(best_d, _)| distance < best_d) {
            best = Some((distance, candidate));
        }
    }
    best.map(|(_, candidate)| candidate.clone())
}

/// The leading subcommand word in raw argv, for callers that must classify
/// argv before extensions start (and before typed extension flags are
/// stripped). Flags ride along: known bool flags are skipped bare, known
/// value flags and undeclared extension flags take the next token unless it
/// is itself a flag or the value was inline (`--x=…`). `--` ends the scan —
/// everything after it is prompt text, never a command.
pub fn leading_subcommand(args: &[String]) -> Option<&str> {
    let mut i = 0;
    while i < args.len() {
        let token = args[i].as_str();
        if token == "--" {
            return None;
        }
        if token.starts_with('-') {
            i += 1;
            // Known value flags and undeclared extension flags take the next
            // token as their value; known bool flags never do.
            let takes_value = !token.contains('=')
                && (VALUE_FLAGS.contains(&token) || !ALL_FLAGS.contains(&token))
                && args.get(i).is_some_and(|next| !next.starts_with('-'));
            if takes_value {
                i += 1;
            }
            continue;
        }
        return Some(token);
    }
    None
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub no_extensions: bool,
    pub no_save: bool,
    pub tool_mode: ToolMode,
    pub json: bool,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub images: Vec<String>,
    pub continue_session: bool,
    pub resume_session: bool,
    /// Accepted for compatibility; diagnostics are always local-only.
    pub no_network: bool,
    pub positional: Vec<String>,
    /// A `--` delimiter appeared, so positional text is prompt even when it
    /// names a subcommand.
    pub delimited: bool,
}

/// The one option that must be known before extensions start. `--` ends
/// option parsing, so prompt text after it can contain either spelling.
pub fn extensions_disabled(args: &[String]) -> bool {
    has_flag(args, &["--no-extensions", "--ne"])
}

pub fn has_flag(args: &[String], names: &[&str]) -> bool {
    args.iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| names.contains(&arg.as_str()))
}

fn take_value(
    args: &[String],
    index: &mut usize,
    inline: Option<&str>,
    name: &str,
) -> Result<String, String> {
    if let Some(value) = inline {
        if value.is_empty() {
            return Err(format!("{name} requires a value"));
        }
        return Ok(value.to_string());
    }
    let Some(value) = args.get(*index + 1) else {
        return Err(format!("{name} requires a value"));
    };
    if value.starts_with('-') {
        return Err(format!("{name} requires a value"));
    }
    *index += 1;
    Ok(value.clone())
}

/// Parse argv into options. `extension_flags` carries the flags extensions
/// declare, purely as did-you-mean candidates: declared typed flags are
/// stripped from argv before parsing runs, so a flag that reaches this
/// function is undeclared or misspelled either way.
pub fn parse(args: Vec<String>, extension_flags: &[String]) -> Result<Options, String> {
    let mut out = Options::default();
    let mut index = 0usize;
    let mut positional_only = false;
    while index < args.len() {
        let arg = &args[index];
        if positional_only {
            out.positional.push(arg.clone());
            index += 1;
            continue;
        }
        if arg == "--" {
            positional_only = true;
            out.delimited = true;
            index += 1;
            continue;
        }

        let (name, inline) = arg
            .split_once('=')
            .map(|(name, value)| (name, Some(value)))
            .unwrap_or((arg.as_str(), None));
        match name {
            "--no-extensions" | "--ne" => out.no_extensions = true,
            "--no-save" | "--ns" => out.no_save = true,
            "--read-only" | "--ro" => {
                if out.tool_mode != ToolMode::None {
                    out.tool_mode = ToolMode::ReadOnly;
                }
            }
            "--no-tools" | "--nt" => out.tool_mode = ToolMode::None,
            "--json" | "-j" => out.json = true,
            "--model" | "-m" => out.model = Some(take_value(&args, &mut index, inline, "--model")?),
            "--effort" | "--ef" => {
                out.effort = Some(take_value(&args, &mut index, inline, "--effort")?)
            }
            "--image" | "-i" => out
                .images
                .push(take_value(&args, &mut index, inline, "--image")?),
            "--continue" | "-c" => out.continue_session = true,
            "--resume" | "-r" => out.resume_session = true,
            "--no-network" => out.no_network = true,
            // Extensions have already had their chance to consume their
            // declared flags, so a leftover flag is a typo: fail with a
            // suggestion instead of silently prompting with it.
            _ if name.starts_with('-') => {
                let mut candidates: Vec<String> = ALL_FLAGS.iter().map(|f| f.to_string()).collect();
                candidates.extend(extension_flags.iter().cloned());
                // Terse single-dash aliases would make every wrong letter a
                // one-edit match; short inputs only get long-name candidates.
                if name.chars().count() <= 2 {
                    candidates.retain(|candidate| candidate.starts_with("--"));
                }
                return Err(match did_you_mean(name, &candidates) {
                    Some(near) => format!("unknown option {name} — did you mean {near}?"),
                    None => format!("unknown option {name} (run `e --help` for the options)"),
                });
            }
            _ => out.positional.push(arg.clone()),
        }
        index += 1;
    }
    if out.continue_session && out.resume_session {
        return Err("--continue and --resume cannot be used together".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn parsed(values: &[&str]) -> Options {
        parse(args(values), &[]).unwrap()
    }

    #[test]
    fn aliases_are_the_same_options_as_canonical_names() {
        let long = parsed(&[
            "--no-extensions",
            "--no-save",
            "--read-only",
            "--json",
            "--model",
            "openai/gpt",
            "--effort=high",
            "--image=one.png",
            "ask",
            "hello",
        ]);
        let short = parsed(&[
            "--ne",
            "--ns",
            "--ro",
            "-j",
            "-m=openai/gpt",
            "--ef",
            "high",
            "-i",
            "one.png",
            "ask",
            "hello",
        ]);
        assert_eq!(long, short);
    }

    #[test]
    fn no_tools_wins_over_read_only_regardless_of_order() {
        assert_eq!(parsed(&["--nt", "--ro"]).tool_mode, ToolMode::None);
        assert_eq!(parsed(&["--ro", "--nt"]).tool_mode, ToolMode::None);
    }

    #[test]
    fn nested_tool_modes_can_only_become_more_restrictive() {
        assert_eq!(ToolMode::None.restrict(ToolMode::All), ToolMode::None);
        assert_eq!(
            ToolMode::ReadOnly.restrict(ToolMode::All),
            ToolMode::ReadOnly
        );
        assert_eq!(
            ToolMode::All.restrict(ToolMode::ReadOnly),
            ToolMode::ReadOnly
        );
    }

    #[test]
    fn delimiter_makes_flag_spelling_prompt_text() {
        let delimited = parsed(&["--ne", "--", "--no-save"]);
        assert!(delimited.no_extensions);
        assert!(!delimited.no_save);
        assert!(delimited.delimited);
        assert_eq!(delimited.positional, vec!["--no-save"]);
        assert!(!extensions_disabled(&args(&["--", "--ne"])));
        assert!(!has_flag(&args(&["--", "--version"]), &["--version", "-v"]));
        assert!(!parsed(&["hello"]).delimited);
    }

    #[test]
    fn unknown_flags_are_errors_with_suggestions_not_prompts() {
        let error = parse(args(&["--modle", "x"]), &[]).unwrap_err();
        assert!(error.contains("unknown option --modle"));
        assert!(error.contains("did you mean --model?"));
        assert_eq!(
            parse(args(&["-x"]), &[]).unwrap_err(),
            "unknown option -x (run `e --help` for the options)"
        );
        // Anywhere in argv, not just in the leading position.
        assert!(parse(args(&["hello", "--junk"]), &[]).is_err());
        // Declared extension flags only shape the suggestion.
        let error = parse(args(&["--worktre"]), &["--worktree".to_string()]).unwrap_err();
        assert!(error.contains("did you mean --worktree?"));
    }

    #[test]
    fn value_flags_fail_when_bare_even_among_known_flags() {
        assert_eq!(
            parse(args(&["--model"]), &[]).unwrap_err(),
            "--model requires a value"
        );
        assert_eq!(
            parse(args(&["--effort", "--json"]), &[]).unwrap_err(),
            "--effort requires a value"
        );
    }

    #[test]
    fn raw_scan_finds_subcommands_and_stops_at_the_delimiter() {
        assert_eq!(
            leading_subcommand(&args(&["doctor", "--no-network"])),
            Some("doctor")
        );
        // A known bool flag before the command must not swallow it: with
        // --no-network absent from ALL_FLAGS this returned None, so doctor was
        // never detected and extensions launched, breaking its no-network
        // contract.
        assert_eq!(
            leading_subcommand(&args(&["--no-network", "doctor"])),
            Some("doctor")
        );
        // An undeclared extension flag takes the next token as its value.
        assert_eq!(
            leading_subcommand(&args(&["--worktree", "feature", "doctor"])),
            Some("doctor")
        );
        // Bool flags do not swallow the command; inline values skip cleanly.
        assert_eq!(leading_subcommand(&args(&["-c", "doctor"])), Some("doctor"));
        assert_eq!(
            leading_subcommand(&args(&["--model=opa", "doctor"])),
            Some("doctor")
        );
        assert_eq!(
            leading_subcommand(&args(&["--effort", "high", "doctor"])),
            Some("doctor")
        );
        assert_eq!(leading_subcommand(&args(&["--", "doctor"])), None);
        assert_eq!(leading_subcommand(&args(&["hello"])), Some("hello"));
    }

    #[test]
    fn suggestions_need_one_edit_and_never_match_exactly() {
        assert_eq!(
            did_you_mean("--modle", &["--model".to_string()]).as_deref(),
            Some("--model")
        );
        assert_eq!(
            did_you_mean("docss", &["docs".to_string(), "ask".to_string()]).as_deref(),
            Some("docs")
        );
        assert_eq!(did_you_mean("docs", &["docs".to_string()]), None);
        assert_eq!(did_you_mean("unrelated", &["docs".to_string()]), None);
    }
}
