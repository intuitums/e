//! A profile-driven syntax highlighter for code panels.
//!
//! Four styles only — keyword, string/number, literal, comment — matching
//! the reference design's approach: enough color to read code, nothing that
//! fights the grayscale ramp. Colors come from the active theme's syntax
//! tokens, so panels stay palette-correct in both light and dark.
//!
//! The reference contract, pinned by tests: a language the profile table
//! doesn't know renders raw — no generic fallback coloring — and an
//! unlabeled fence may be inferred from its content (the caller prints the
//! inferred label). Literals (`true`, `nil`, `None`…) wear the number color,
//! one step dimmer than keywords.

use crate::tui::theme::Theme;

struct BlockComment {
    start: &'static str,
    end: &'static str,
}

struct Profile {
    label: &'static str,
    aliases: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<BlockComment>,
    quotes: &'static [char],
    keywords: &'static [&'static str],
    literals: &'static [&'static str],
    case_insensitive: bool,
}

const NO_WORDS: &[&str] = &[];
const DOUBLE_QUOTE: &[char] = &['"'];
const DOUBLE_SINGLE: &[char] = &['"', '\''];
const SHELL_QUOTES: &[char] = &['"', '\'', '`'];

macro_rules! profile {
    ($label:literal, $aliases:expr, $lc:expr, $bc:expr, $quotes:expr, $kw:expr, $lit:expr, $ci:expr) => {
        Profile {
            label: $label,
            aliases: $aliases,
            line_comments: $lc,
            block_comment: $bc,
            quotes: $quotes,
            keywords: $kw,
            literals: $lit,
            case_insensitive: $ci,
        }
    };
}

const SLASH_STAR: Option<BlockComment> = Some(BlockComment {
    start: "/*",
    end: "*/",
});

/// The reference's language table, value-for-value: labels, aliases,
/// comment markers, quote sets, keywords, and literals.
static PROFILES: &[Profile] = &[
    profile!(
        "zig",
        &["zig"],
        &["//"],
        None,
        DOUBLE_QUOTE,
        &[
            "const", "var", "fn", "pub", "return", "if", "else", "while", "for", "struct", "enum",
            "union", "try", "catch", "comptime", "defer", "errdefer", "async", "await", "anytype",
            "void"
        ],
        NO_WORDS,
        false
    ),
    profile!(
        "ts",
        &["js", "jsx", "javascript", "ts", "tsx", "typescript"],
        &["//"],
        SLASH_STAR,
        SHELL_QUOTES,
        &[
            "const",
            "let",
            "var",
            "function",
            "class",
            "interface",
            "type",
            "export",
            "import",
            "from",
            "return",
            "if",
            "else",
            "for",
            "while",
            "async",
            "await",
            "new",
            "extends",
            "implements",
            "public",
            "private",
            "readonly"
        ],
        &["true", "false", "null", "undefined"],
        false
    ),
    profile!(
        "json",
        &["json"],
        &[],
        None,
        DOUBLE_QUOTE,
        NO_WORDS,
        &["true", "false", "null"],
        false
    ),
    profile!(
        "sh",
        &["sh", "bash", "zsh", "shell"],
        &["#"],
        None,
        SHELL_QUOTES,
        &[
            "if", "then", "fi", "for", "do", "done", "in", "case", "esac", "function", "local",
            "export", "readonly", "return"
        ],
        &["true", "false", "null"],
        false
    ),
    profile!(
        "python",
        &["python", "py"],
        &["#"],
        None,
        DOUBLE_SINGLE,
        &[
            "def", "class", "return", "if", "elif", "else", "for", "while", "in", "import", "from",
            "as", "try", "except", "with", "lambda", "async", "await", "pass", "raise", "yield",
            "match", "case"
        ],
        &["True", "False", "None"],
        false
    ),
    profile!(
        "yaml",
        &["yaml", "yml"],
        &["#"],
        None,
        DOUBLE_SINGLE,
        NO_WORDS,
        &["true", "false", "null", "yes", "no", "on", "off"],
        false
    ),
    profile!(
        "toml",
        &["toml"],
        &["#"],
        None,
        DOUBLE_SINGLE,
        NO_WORDS,
        &["true", "false"],
        false
    ),
    profile!(
        "sql",
        &["sql"],
        &["--"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "select", "from", "where", "join", "left", "right", "inner", "outer", "on", "insert",
            "into", "values", "update", "set", "delete", "create", "alter", "drop", "table",
            "index", "group", "by", "order", "having", "limit", "as", "and", "or", "not",
            "distinct", "union"
        ],
        &["true", "false", "null"],
        true
    ),
    profile!(
        "dockerfile",
        &["dockerfile", "docker"],
        &["#"],
        None,
        DOUBLE_SINGLE,
        &[
            "from",
            "run",
            "cmd",
            "entrypoint",
            "copy",
            "add",
            "workdir",
            "env",
            "arg",
            "expose",
            "volume",
            "user",
            "label",
            "onbuild",
            "stopsignal",
            "healthcheck",
            "shell",
            "maintainer"
        ],
        NO_WORDS,
        true
    ),
    profile!(
        "rust",
        &["rust", "rs"],
        &["//"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "use", "mod", "crate",
            "return", "if", "else", "match", "for", "while", "loop", "async", "await", "move",
            "where", "self", "super"
        ],
        &["true", "false", "None", "Some"],
        false
    ),
    profile!(
        "go",
        &["go"],
        &["//"],
        SLASH_STAR,
        &['"', '`'],
        &[
            "package",
            "import",
            "func",
            "var",
            "const",
            "type",
            "struct",
            "interface",
            "return",
            "if",
            "else",
            "for",
            "range",
            "switch",
            "case",
            "go",
            "defer",
            "select",
            "chan",
            "map"
        ],
        &["true", "false", "nil"],
        false
    ),
    profile!(
        "c",
        &["c", "h"],
        &["//"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "auto", "break", "case", "char", "const", "continue", "default", "do", "double",
            "else", "enum", "extern", "float", "for", "goto", "if", "int", "long", "return",
            "short", "signed", "sizeof", "static", "struct", "switch", "typedef", "union",
            "unsigned", "void", "volatile", "while"
        ],
        &["true", "false", "NULL"],
        false
    ),
    profile!(
        "cpp",
        &["cpp", "c++", "cc", "cxx", "hpp"],
        &["//"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "auto",
            "bool",
            "class",
            "const",
            "constexpr",
            "decltype",
            "delete",
            "enum",
            "explicit",
            "friend",
            "inline",
            "namespace",
            "new",
            "nullptr",
            "private",
            "protected",
            "public",
            "template",
            "this",
            "typename",
            "using",
            "virtual",
            "void"
        ],
        &["true", "false", "nullptr", "NULL"],
        false
    ),
    profile!(
        "csharp",
        &["csharp", "cs"],
        &["//"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "class",
            "namespace",
            "using",
            "public",
            "private",
            "protected",
            "internal",
            "static",
            "void",
            "string",
            "int",
            "var",
            "new",
            "return",
            "if",
            "else",
            "for",
            "foreach",
            "while",
            "async",
            "await",
            "interface",
            "record",
            "get",
            "set"
        ],
        &["true", "false", "null"],
        false
    ),
    profile!(
        "java",
        &["java"],
        &["//"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "class",
            "interface",
            "package",
            "import",
            "public",
            "private",
            "protected",
            "static",
            "final",
            "void",
            "new",
            "return",
            "if",
            "else",
            "for",
            "while",
            "try",
            "catch",
            "throws",
            "extends",
            "implements",
            "record",
            "var"
        ],
        &["true", "false", "null"],
        false
    ),
    profile!(
        "kotlin",
        &["kotlin", "kt", "kts"],
        &["//"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "fun",
            "val",
            "var",
            "class",
            "object",
            "interface",
            "package",
            "import",
            "public",
            "private",
            "return",
            "if",
            "else",
            "when",
            "for",
            "while",
            "try",
            "catch",
            "data",
            "sealed",
            "suspend"
        ],
        &["true", "false", "null"],
        false
    ),
    profile!(
        "php",
        &["php"],
        &["//", "#"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "function",
            "class",
            "public",
            "private",
            "protected",
            "namespace",
            "use",
            "return",
            "if",
            "else",
            "foreach",
            "for",
            "while",
            "try",
            "catch",
            "new",
            "static",
            "const",
            "echo",
            "yield"
        ],
        &["true", "false", "null"],
        false
    ),
    profile!(
        "ruby",
        &["ruby", "rb"],
        &["#"],
        None,
        DOUBLE_SINGLE,
        &[
            "def",
            "class",
            "module",
            "end",
            "return",
            "if",
            "elsif",
            "else",
            "unless",
            "case",
            "when",
            "do",
            "while",
            "for",
            "in",
            "begin",
            "rescue",
            "require",
            "attr_reader"
        ],
        &["true", "false", "nil"],
        false
    ),
    profile!(
        "swift",
        &["swift"],
        &["//"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "func",
            "let",
            "var",
            "class",
            "struct",
            "enum",
            "protocol",
            "extension",
            "import",
            "public",
            "private",
            "return",
            "if",
            "else",
            "guard",
            "for",
            "while",
            "switch",
            "case",
            "async",
            "await",
            "throws",
            "try"
        ],
        &["true", "false", "nil"],
        false
    ),
    profile!(
        "powershell",
        &["powershell", "ps1", "pwsh", "ps"],
        &["#"],
        Some(BlockComment {
            start: "<#",
            end: "#>"
        }),
        DOUBLE_SINGLE,
        &[
            "function", "param", "if", "else", "elseif", "foreach", "for", "while", "switch",
            "return", "throw", "try", "catch", "finally", "begin", "process", "end", "filter",
            "class", "enum"
        ],
        &["true", "false", "null"],
        true
    ),
    profile!(
        "lua",
        &["lua"],
        &["--"],
        Some(BlockComment {
            start: "--[[",
            end: "]]"
        }),
        DOUBLE_SINGLE,
        &[
            "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto",
            "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until",
            "while"
        ],
        &["true", "false", "nil"],
        false
    ),
    profile!(
        "html",
        &["html", "htm"],
        &[],
        Some(BlockComment {
            start: "<!--",
            end: "-->"
        }),
        DOUBLE_SINGLE,
        &[
            "html", "head", "body", "main", "header", "footer", "section", "article", "div",
            "span", "a", "p", "script", "style", "link", "meta", "title", "button", "input",
            "form", "img", "ul", "li"
        ],
        NO_WORDS,
        false
    ),
    profile!(
        "xml",
        &["xml"],
        &[],
        Some(BlockComment {
            start: "<!--",
            end: "-->"
        }),
        DOUBLE_SINGLE,
        &["xml", "version", "encoding", "DOCTYPE", "CDATA"],
        NO_WORDS,
        false
    ),
    profile!(
        "css",
        &["css"],
        &[],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "color",
            "background",
            "display",
            "position",
            "margin",
            "padding",
            "border",
            "font",
            "width",
            "height",
            "flex",
            "grid",
            "align",
            "justify",
            "transition",
            "transform",
            "animation",
            "media"
        ],
        NO_WORDS,
        false
    ),
    profile!(
        "hcl",
        &["hcl", "terraform", "tf"],
        &["#", "//"],
        SLASH_STAR,
        DOUBLE_SINGLE,
        &[
            "resource",
            "module",
            "variable",
            "output",
            "provider",
            "terraform",
            "locals",
            "data",
            "dynamic",
            "for_each",
            "count"
        ],
        &["true", "false", "null"],
        false
    ),
];

fn resolve(label: &str) -> Option<&'static Profile> {
    PROFILES
        .iter()
        .find(|p| p.aliases.iter().any(|a| a.eq_ignore_ascii_case(label)))
}

/// A detector's verdict, resolved back through the profile table so the
/// returned label is always a real profile's canonical name.
fn canonical(label: &str) -> Option<&'static str> {
    resolve(label).map(|p| p.label)
}

/// Infer a language for an unlabeled fence from its content, the reference's
/// detector ladder. The caller prints the returned label in the panel rule.
pub fn infer_language(source: &str) -> Option<&'static str> {
    let first = first_nonblank_line(source);
    // TypeScript: a `} as Upper` type assertion anywhere.
    if source.match_indices("} as ").any(|(i, _)| {
        source[i + 5..]
            .chars()
            .next()
            .map(|c| c.is_ascii_uppercase())
            .unwrap_or(false)
    }) {
        return canonical("ts");
    }
    // JSON: a parseable object or array.
    let trimmed = source.trim();
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(trimmed)
            .map(|v| v.is_object() || v.is_array())
            .unwrap_or(false)
    {
        return canonical("json");
    }
    // Shell: a shebang naming a shell.
    if first.starts_with("#!")
        && (first.contains("bash") || first.contains("zsh") || first.contains("/sh"))
    {
        return canonical("sh");
    }
    // Python: a def/class header line.
    if (first.starts_with("def ") || first.starts_with("class ")) && first.ends_with(':') {
        return canonical("python");
    }
    // SQL: SELECT … FROM.
    if starts_with_ignore_case(first, "select ") && contains_word_ignore_case(source, "from") {
        return canonical("sql");
    }
    // Dockerfile: FROM ….
    if starts_with_ignore_case(first, "from ") {
        return canonical("dockerfile");
    }
    // Go: package header plus a func.
    if first.starts_with("package ") && source.lines().any(|l| l.trim_start().starts_with("func "))
    {
        return canonical("go");
    }
    // Rust: an fn header with a let/println!/-> in evidence.
    if (first.starts_with("fn ") || first.starts_with("pub fn "))
        && (source.contains("let ") || source.contains("println!") || first.contains("->"))
    {
        return canonical("rust");
    }
    None
}

fn first_nonblank_line(source: &str) -> &str {
    source
        .lines()
        .map(|l| l.trim_matches([' ', '\t', '\r']))
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn contains_word_ignore_case(source: &str, word: &str) -> bool {
    source
        .split(|c: char| !is_word_char(c))
        .any(|w| w.eq_ignore_ascii_case(word))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

const RESET: &str = "\x1b[39m";

/// Highlight a whole block at once, so block comments span lines. A language
/// the profile table doesn't know renders raw, byte-identical — the
/// reference colors nothing it cannot name.
pub fn highlight_block(theme: &Theme, lang: &str, source: &str) -> Vec<String> {
    let Some(profile) = resolve(lang) else {
        return source.split('\n').map(String::from).collect();
    };
    let kw = theme.fg_prefix("syntaxKeyword");
    let strn = theme.fg_prefix("syntaxString");
    let num = theme.fg_prefix("syntaxNumber");
    let com = theme.fg_prefix("syntaxComment");
    let mut in_block_comment = false;
    source
        .split('\n')
        .map(|line| highlight_one(profile, line, &mut in_block_comment, kw, strn, num, com))
        .collect()
}

/// Highlight one line (no cross-line comment state). Kept for single-line
/// callers and tests.
pub fn highlight_line(theme: &Theme, lang: &str, line: &str) -> String {
    highlight_block(theme, lang, line).pop().unwrap_or_default()
}

fn push_styled(out: &mut String, style: &str, tok: &str) {
    if style.is_empty() {
        out.push_str(tok);
    } else {
        out.push_str(style);
        out.push_str(tok);
        out.push_str(RESET);
    }
}

fn word_matches(profile: &Profile, words: &[&str], tok: &str) -> bool {
    if profile.case_insensitive {
        words.iter().any(|w| w.eq_ignore_ascii_case(tok))
    } else {
        words.contains(&tok)
    }
}

fn highlight_one(
    profile: &Profile,
    line: &str,
    in_block_comment: &mut bool,
    kw: &str,
    strn: &str,
    num: &str,
    com: &str,
) -> String {
    let mut out = String::with_capacity(line.len() + 16);
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Inside a block comment: consume to its terminator or line end.
        if *in_block_comment {
            let end: Vec<char> = profile
                .block_comment
                .as_ref()
                .map(|b| b.end.chars().collect())
                .unwrap_or_default();
            let start = i;
            let mut closed = false;
            while i < chars.len() {
                if !end.is_empty() && chars[i..].starts_with(&end[..]) {
                    i += end.len();
                    closed = true;
                    break;
                }
                i += 1;
            }
            if closed {
                *in_block_comment = false;
            }
            let tok: String = chars[start..i].iter().collect();
            push_styled(&mut out, com, &tok);
            continue;
        }
        let c = chars[i];
        // A block comment opener (checked before line markers: lua's
        // `--[[` must win over `--`).
        if let Some(block) = &profile.block_comment {
            let start_marker: Vec<char> = block.start.chars().collect();
            if chars[i..].starts_with(&start_marker[..]) {
                *in_block_comment = true;
                let start = i;
                i += start_marker.len();
                let end: Vec<char> = block.end.chars().collect();
                while i < chars.len() {
                    if chars[i..].starts_with(&end[..]) {
                        i += end.len();
                        *in_block_comment = false;
                        break;
                    }
                    i += 1;
                }
                let tok: String = chars[start..i].iter().collect();
                push_styled(&mut out, com, &tok);
                continue;
            }
        }
        // Comment to end of line. The marker comparison reads the char
        // slice at i directly — slicing the original line would re-sum the
        // UTF-8 prefix per char, an O(n²) stall on long single-line tool
        // output (minified JSON, log lines).
        if profile.line_comments.iter().any(|marker| {
            let m: Vec<char> = marker.chars().collect();
            chars[i..].starts_with(&m[..])
        }) {
            let rest: String = chars[i..].iter().collect();
            push_styled(&mut out, com, &rest);
            break;
        }
        // Strings, in the profile's own quote set.
        if profile.quotes.contains(&c) {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let tok: String = chars[start..i.min(chars.len())].iter().collect();
            push_styled(&mut out, strn, &tok);
            continue;
        }
        // Numbers.
        if c.is_ascii_digit() && (i == 0 || !is_word_char(chars[i - 1])) {
            let start = i;
            while i < chars.len() && (is_word_char(chars[i]) || chars[i] == '.') {
                i += 1;
            }
            let tok: String = chars[start..i].iter().collect();
            push_styled(&mut out, num, &tok);
            continue;
        }
        // Words → keywords (accent) or literals (the number gray).
        if is_word_char(c) {
            let start = i;
            while i < chars.len() && is_word_char(chars[i]) {
                i += 1;
            }
            let tok: String = chars[start..i].iter().collect();
            if !kw.is_empty() && word_matches(profile, profile.keywords, &tok) {
                push_styled(&mut out, kw, &tok);
            } else if !num.is_empty() && word_matches(profile, profile.literals, &tok) {
                push_styled(&mut out, num, &tok);
            } else {
                out.push_str(&tok);
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        crate::tui::theme::load_bundled(false).unwrap()
    }

    #[test]
    fn unstyled_text_passes_through_byte_identical() {
        let line = "plain text with no tokens";
        assert_eq!(highlight_line(&theme(), "", line), line);
    }

    #[test]
    fn unknown_languages_render_raw() {
        // The reference colors nothing it cannot name: no generic keyword
        // fallback, no default comment marker.
        let line = "if x return y // note";
        assert_eq!(highlight_line(&theme(), "brainfuck", line), line);
    }

    #[test]
    fn aliases_resolve_case_insensitively() {
        let t = theme();
        let kw = t.fg_prefix("syntaxKeyword");
        let out = highlight_line(&t, "Rust", "fn main() {}");
        assert!(out.starts_with(&format!("{kw}fn\x1b[39m")), "{out:?}");
    }

    #[test]
    fn styles_keywords_strings_and_comments() {
        let t = theme();
        let kw = t.fg_prefix("syntaxKeyword");
        let strn = t.fg_prefix("syntaxString");
        let com = t.fg_prefix("syntaxComment");
        let out = highlight_line(&t, "rust", r#"let s = "hi"; // note"#);
        assert!(out.starts_with(&format!("{kw}let\x1b[39m")));
        assert!(out.contains(&format!("{strn}\"hi\"\x1b[39m")));
        assert!(out.contains(&format!("{com}// note\x1b[39m")));
        // Unstyled runs stay verbatim.
        assert!(out.contains(" s = "));
        assert!(out.contains("; "));
    }

    #[test]
    fn literals_wear_the_number_color() {
        let t = theme();
        let num = t.fg_prefix("syntaxNumber");
        let out = highlight_line(&t, "json", r#"{"a": true, "n": 42}"#);
        assert!(out.contains(&format!("{num}true\x1b[39m")), "{out:?}");
        assert!(out.contains(&format!("{num}42\x1b[39m")), "{out:?}");
    }

    #[test]
    fn sql_keywords_match_case_insensitively() {
        let t = theme();
        let kw = t.fg_prefix("syntaxKeyword");
        let out = highlight_line(&t, "sql", "SELECT id FROM users");
        assert!(out.starts_with(&format!("{kw}SELECT\x1b[39m")), "{out:?}");
        assert!(out.contains(&format!("{kw}FROM\x1b[39m")), "{out:?}");
    }

    #[test]
    fn block_comments_span_lines() {
        let t = theme();
        let com = t.fg_prefix("syntaxComment");
        let rows = highlight_block(&t, "rust", "let a = 1;\n/* one\ntwo */ let b = 2;");
        assert!(
            rows[1].starts_with(&format!("{com}/* one\x1b[39m")),
            "{:?}",
            rows[1]
        );
        assert!(
            rows[2].starts_with(&format!("{com}two */\x1b[39m")),
            "{:?}",
            rows[2]
        );
        assert!(rows[2].contains("let"), "{:?}", rows[2]);
    }

    #[test]
    fn zig_single_quotes_stay_plain() {
        // Zig's profile quotes only `"` — an apostrophe must not swallow the
        // rest of the line as a string.
        let t = theme();
        let out = highlight_line(&t, "zig", "const c = 'a';");
        assert!(out.contains("'a';"), "{out:?}");
    }

    #[test]
    fn inference_names_the_obvious_languages() {
        assert_eq!(infer_language("{\"a\": 1}"), Some("json"));
        assert_eq!(infer_language("#!/bin/bash\necho hi"), Some("sh"));
        assert_eq!(infer_language("def main():\n    pass"), Some("python"));
        assert_eq!(infer_language("SELECT * FROM t"), Some("sql"));
        assert_eq!(infer_language("FROM alpine:3.20"), Some("dockerfile"));
        assert_eq!(infer_language("package main\n\nfunc main() {}"), Some("go"));
        assert_eq!(
            infer_language("fn main() {\n    let x = 1;\n}"),
            Some("rust")
        );
        assert_eq!(infer_language("just words"), None);
    }

    #[test]
    fn long_single_line_stays_linear() {
        // Regression: byte_at re-summed the UTF-8 prefix of the whole line
        // for every char, making cost quadratic in line length. Compare a
        // 4x-larger input against a baseline instead of pinning an absolute
        // wall-clock budget: a linear scan lands near 4x even under heavy
        // scheduling noise, while a quadratic one blows well past the 16x
        // margin asserted below — so this stays meaningful on a slow or
        // loaded CI runner instead of flaking on an arbitrary time limit.
        fn line_of(n: usize) -> String {
            let mut line = String::with_capacity(n + 12);
            line.push('"');
            line.push_str(&"a".repeat(n));
            line.push_str("\" // note");
            line
        }

        let t = theme();
        let small = line_of(16 * 1024);
        let large = line_of(64 * 1024);

        let start = std::time::Instant::now();
        let out_small = highlight_line(&t, "rust", &small);
        let small_elapsed = start.elapsed();

        let start = std::time::Instant::now();
        let out_large = highlight_line(&t, "rust", &large);
        let large_elapsed = start.elapsed();

        assert!(out_small.contains("// note"));
        assert!(out_large.contains("// note"));
        assert!(
            large_elapsed.as_nanos() < small_elapsed.as_nanos().max(1) * 16,
            "highlight_line scales worse than linear: {small_elapsed:?} -> {large_elapsed:?}"
        );
    }

    #[test]
    fn profile_labels_are_unique_and_aliases_unambiguous() {
        for (i, a) in PROFILES.iter().enumerate() {
            for b in &PROFILES[i + 1..] {
                assert_ne!(a.label, b.label);
                for alias in a.aliases {
                    assert!(
                        !b.aliases.iter().any(|x| x.eq_ignore_ascii_case(alias)),
                        "alias {alias} is claimed by {} and {}",
                        a.label,
                        b.label
                    );
                }
            }
        }
    }
}
