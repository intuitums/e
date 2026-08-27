//! Built-in command-line options.
//!
//! Parsing lives in the terminal-free core so the binary, tests, and future
//! machine protocol all share one contract. Long descriptive names are the
//! canonical surface; compact aliases normalize to the same fields.

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
    pub positional: Vec<String>,
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

pub fn parse(args: Vec<String>) -> Result<Options, String> {
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
            // Unknown options remain prompt/subcommand content. Extensions
            // have already had their chance to consume their declared flags.
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

    #[test]
    fn aliases_are_the_same_options_as_canonical_names() {
        let long = parse(args(&[
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
        ]))
        .unwrap();
        let short = parse(args(&[
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
        ]))
        .unwrap();
        assert_eq!(long, short);
    }

    #[test]
    fn no_tools_wins_over_read_only_regardless_of_order() {
        assert_eq!(
            parse(args(&["--nt", "--ro"])).unwrap().tool_mode,
            ToolMode::None
        );
        assert_eq!(
            parse(args(&["--ro", "--nt"])).unwrap().tool_mode,
            ToolMode::None
        );
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
        let parsed = parse(args(&["--ne", "--", "--no-save"])).unwrap();
        assert!(parsed.no_extensions);
        assert!(!parsed.no_save);
        assert_eq!(parsed.positional, vec!["--no-save"]);
        assert!(!extensions_disabled(&args(&["--", "--ne"])));
        assert!(!has_flag(&args(&["--", "--version"]), &["--version", "-v"]));
    }

    #[test]
    fn recognized_value_flags_fail_when_bare() {
        assert_eq!(
            parse(args(&["--model"])).unwrap_err(),
            "--model requires a value"
        );
        assert_eq!(
            parse(args(&["--effort", "--json"])).unwrap_err(),
            "--effort requires a value"
        );
    }
}
