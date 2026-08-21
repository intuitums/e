//! A tiny keyword-class syntax highlighter for code panels.
//!
//! Four styles only — keyword, string, number, comment — matching the
//! reference design's approach: enough color to read code, nothing that
//! fights the grayscale ramp. Colors come from the active theme's `accent`,
//! `code`, and `comment` tokens, so panels stay palette-correct in both
//! light and dark.

use crate::tui::theme::Theme;

const KEYWORDS_COMMON: &[&str] = &[
    "if", "else", "for", "while", "return", "break", "continue", "match",
    "switch", "case", "default", "try", "catch", "finally", "throw", "new",
    "in", "of", "do", "not", "and", "or",
];

fn keywords_for(lang: &str) -> Vec<&'static str> {
    let extra: &[&str] = match lang {
        "rust" | "rs" => &["fn", "let", "mut", "pub", "impl", "struct", "enum", "trait", "use", "mod", "async", "await", "loop", "self", "Self", "const", "static", "where", "dyn", "ref", "move", "unsafe", "crate", "super", "type"],
        "ts" | "tsx" | "typescript" | "js" | "jsx" | "javascript" => &["const", "let", "var", "function", "class", "extends", "implements", "interface", "type", "enum", "import", "export", "from", "async", "await", "yield", "this", "super", "static", "public", "private", "protected", "readonly", "typeof", "instanceof", "void", "delete", "null", "undefined", "true", "false"],
        "py" | "python" => &["def", "class", "import", "from", "as", "with", "lambda", "yield", "global", "nonlocal", "pass", "raise", "assert", "del", "is", "None", "True", "False", "elif", "except", "async", "await", "self"],
        "go" | "golang" => &["func", "package", "import", "type", "struct", "interface", "map", "chan", "go", "defer", "select", "range", "var", "const", "fallthrough", "nil", "true", "false"],
        "zig" => &["fn", "pub", "const", "var", "comptime", "inline", "defer", "errdefer", "struct", "enum", "union", "test", "usingnamespace", "try", "orelse", "unreachable", "null", "undefined", "true", "false"],
        "sh" | "bash" | "zsh" | "shell" => &["echo", "export", "local", "function", "then", "fi", "elif", "done", "esac", "source", "exit", "cd", "set"],
        "c" | "h" | "cpp" | "cc" | "hpp" => &["int", "char", "void", "long", "short", "unsigned", "signed", "float", "double", "struct", "union", "enum", "typedef", "sizeof", "static", "extern", "inline", "const", "goto", "auto", "bool", "true", "false", "NULL", "nullptr", "class", "namespace", "template", "public", "private", "using"],
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
    let comment_marker = line_comment_for(lang);
    let kw = theme.fg_prefix("syntaxKeyword");
    let strn = theme.fg_prefix("syntaxString");
    let com = theme.fg_prefix("syntaxComment");
    const RESET: &str = "\x1b[39m";

    let mut out = String::with_capacity(line.len() + 16);
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Comment to end of line.
        if line[byte_at(&chars, i)..].starts_with(comment_marker) {
            let rest: String = chars[i..].iter().collect();
            if com.is_empty() { out.push_str(&rest); } else { out.push_str(com); out.push_str(&rest); out.push_str(RESET); }
            break;
        }
        // Strings.
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' { i += 2; continue; }
                if chars[i] == quote { i += 1; break; }
                i += 1;
            }
            let tok: String = chars[start..i.min(chars.len())].iter().collect();
            if strn.is_empty() { out.push_str(&tok); } else { out.push_str(strn); out.push_str(&tok); out.push_str(RESET); }
            continue;
        }
        // Numbers.
        if c.is_ascii_digit() && (i == 0 || !is_word_char(chars[i - 1])) {
            let start = i;
            while i < chars.len() && (is_word_char(chars[i]) || chars[i] == '.') { i += 1; }
            let tok: String = chars[start..i].iter().collect();
            if strn.is_empty() { out.push_str(&tok); } else { out.push_str(strn); out.push_str(&tok); out.push_str(RESET); }
            continue;
        }
        // Words → maybe keywords.
        if is_word_char(c) {
            let start = i;
            while i < chars.len() && is_word_char(chars[i]) { i += 1; }
            let tok: String = chars[start..i].iter().collect();
            if !kw.is_empty() && keywords.contains(&tok.as_str()) {
                out.push_str(kw); out.push_str(&tok); out.push_str(RESET);
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

fn byte_at(chars: &[char], idx: usize) -> usize {
    chars[..idx].iter().map(|c| c.len_utf8()).sum()
}
