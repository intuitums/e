//! A tiny keyword-class syntax highlighter for code panels.
//!
//! Four styles only — keyword, string, number, comment — matching the
//! reference design's approach: enough color to read code, nothing that
//! fights the grayscale ramp. Colors come from the active theme's `accent`,
//! `code`, and `comment` tokens, so panels stay palette-correct in both
//! light and dark.

use crate::tui::theme::Theme;

const KEYWORDS_COMMON: &[&str] = &[
    "if", "else", "for", "while", "return", "break", "continue", "match", "switch", "case",
    "default", "try", "catch", "finally", "throw", "new", "in", "of", "do", "not", "and", "or",
];

fn keywords_for(lang: &str) -> Vec<&'static str> {
    let extra: &[&str] = match lang {
        "rust" | "rs" => &[
            "fn", "let", "mut", "pub", "impl", "struct", "enum", "trait", "use", "mod", "async",
            "await", "loop", "self", "Self", "const", "static", "where", "dyn", "ref", "move",
            "unsafe", "crate", "super", "type",
        ],
        "ts" | "tsx" | "typescript" | "js" | "jsx" | "javascript" => &[
            "const",
            "let",
            "var",
            "function",
            "class",
            "extends",
            "implements",
            "interface",
            "type",
            "enum",
            "import",
            "export",
            "from",
            "async",
            "await",
            "yield",
            "this",
            "super",
            "static",
            "public",
            "private",
            "protected",
            "readonly",
            "typeof",
            "instanceof",
            "void",
            "delete",
            "null",
            "undefined",
            "true",
            "false",
        ],
        "py" | "python" => &[
            "def", "class", "import", "from", "as", "with", "lambda", "yield", "global",
            "nonlocal", "pass", "raise", "assert", "del", "is", "None", "True", "False", "elif",
            "except", "async", "await", "self",
        ],
        "go" | "golang" => &[
            "func",
            "package",
            "import",
            "type",
            "struct",
            "interface",
            "map",
            "chan",
            "go",
            "defer",
            "select",
            "range",
            "var",
            "const",
            "fallthrough",
            "nil",
            "true",
            "false",
        ],
        "zig" => &[
            "fn",
            "pub",
            "const",
            "var",
            "comptime",
            "inline",
            "defer",
            "errdefer",
            "struct",
            "enum",
            "union",
            "test",
            "usingnamespace",
            "try",
            "orelse",
            "unreachable",
            "null",
            "undefined",
            "true",
            "false",
        ],
        "sh" | "bash" | "zsh" | "shell" => &[
            "echo", "export", "local", "function", "then", "fi", "elif", "done", "esac", "source",
            "exit", "cd", "set",
        ],
        "c" | "h" | "cpp" | "cc" | "hpp" => &[
            "int",
            "char",
            "void",
            "long",
            "short",
            "unsigned",
            "signed",
            "float",
            "double",
            "struct",
            "union",
            "enum",
            "typedef",
            "sizeof",
            "static",
            "extern",
            "inline",
            "const",
            "goto",
            "auto",
            "bool",
            "true",
            "false",
            "NULL",
            "nullptr",
            "class",
            "namespace",
            "template",
            "public",
            "private",
            "using",
        ],
        "json" => &["true", "false", "null"],
        _ => &[],
    };
    KEYWORDS_COMMON.iter().chain(extra).copied().collect()
}

fn line_comment_for(lang: &str) -> &'static str {
    match lang {
        "py" | "python" | "sh" | "bash" | "zsh" | "shell" | "yaml" | "yml" | "toml" => "#",
        "sql" | "lua" => "--",
        _ => "//",
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Highlight one line. Emits `<fg>token\x1b[39m` runs only around styled
/// tokens; unstyled text passes through byte-identical, which the panel
/// geometry tests depend on.
pub fn highlight_line(theme: &Theme, lang: &str, line: &str) -> String {
    let keywords = keywords_for(lang);
    let comment_marker: Vec<char> = line_comment_for(lang).chars().collect();
    let kw = theme.fg_prefix("syntaxKeyword");
    let strn = theme.fg_prefix("syntaxString");
    let com = theme.fg_prefix("syntaxComment");
    const RESET: &str = "\x1b[39m";

    let mut out = String::with_capacity(line.len() + 16);
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Comment to end of line. The marker comparison reads the char
        // slice at i directly — slicing the original line would re-sum the
        // UTF-8 prefix per char, an O(n²) stall on long single-line tool
        // output (minified JSON, log lines).
        if chars[i..].starts_with(&comment_marker) {
            let rest: String = chars[i..].iter().collect();
            if com.is_empty() {
                out.push_str(&rest);
            } else {
                out.push_str(com);
                out.push_str(&rest);
                out.push_str(RESET);
            }
            break;
        }
        // Strings.
        if c == '"' || c == '\'' || c == '`' {
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
            if strn.is_empty() {
                out.push_str(&tok);
            } else {
                out.push_str(strn);
                out.push_str(&tok);
                out.push_str(RESET);
            }
            continue;
        }
        // Numbers.
        if c.is_ascii_digit() && (i == 0 || !is_word_char(chars[i - 1])) {
            let start = i;
            while i < chars.len() && (is_word_char(chars[i]) || chars[i] == '.') {
                i += 1;
            }
            let tok: String = chars[start..i].iter().collect();
            if strn.is_empty() {
                out.push_str(&tok);
            } else {
                out.push_str(strn);
                out.push_str(&tok);
                out.push_str(RESET);
            }
            continue;
        }
        // Words → maybe keywords.
        if is_word_char(c) {
            let start = i;
            while i < chars.len() && is_word_char(chars[i]) {
                i += 1;
            }
            let tok: String = chars[start..i].iter().collect();
            if !kw.is_empty() && keywords.contains(&tok.as_str()) {
                out.push_str(kw);
                out.push_str(&tok);
                out.push_str(RESET);
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
}
