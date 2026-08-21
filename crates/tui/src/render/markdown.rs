//! Markdown → styled terminal lines, in the reference design's shapes.
//!
//! Visual contract, byte-pinned by the parity tests:
//!   headings   level-specific SGR (see `ansi::heading_style`)
//!   bullets    dim `•`, two columns of indent per level, numbers kept
//!   code       shrink-wrapped `┌ lang ─┐` panel, palette highlight colors
//!   quotes     dim `│ ` rail, body upright
//!   rules      fixed 60 columns, SGR dim
//!   inline     bold/italic/strike as SGR; code spans in the palette's
//!              inline-code gray; links underline-only with OSC 8
//!
//! Parsing uses pulldown-cmark; rendering owns the width, so blocks land on
//! their final lines directly.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;

use crate::render::ansi::*;
use crate::render::highlight::highlight_line;
use crate::render::theme::Theme;

fn osc8(url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\")
}
const OSC8_CLOSE: &str = "\x1b]8;;\x1b\\";

/// Visible width of a styled string (ANSI SGR and OSC sequences are zero).
pub fn visible_width(styled: &str) -> usize {
    let mut width = 0;
    let mut chars = styled.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if n.is_ascii_alphabetic() { break; }
                    }
                }
                Some(']') => {
                    // OSC … terminated by BEL or ST (ESC \)
                    while let Some(n) = chars.next() {
                        if n == '\x07' { break; }
                        if n == '\x1b' { chars.next(); break; }
                    }
                }
                _ => {}
            }
            continue;
        }
        width += c.width().unwrap_or(0);
    }
    width
}

/// Word-wrap a styled string; ANSI runs travel with the word they precede.
pub fn wrap_styled(styled: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    for hard in styled.split('\n') {
        let mut row = String::new();
        let mut row_width = 0usize;
        for word in hard.split(' ') {
            let w = visible_width(word);
            if !row.is_empty() && row_width + 1 + w > width {
                rows.push(std::mem::take(&mut row));
                row_width = 0;
            }
            if row.is_empty() {
                row.push_str(word);
                row_width = w;
            } else {
                row.push(' ');
                row.push_str(word);
                row_width += 1 + w;
            }
        }
        rows.push(row);
    }
    if rows.is_empty() { rows.push(String::new()); }
    rows
}

const RESETS: &[&str] = &["\x1b[0m", "\x1b[39m", "\x1b[22m", "\x1b[24m"];

/// Hard-wrap one code line, closing and reopening any open color at the seam.
fn hard_wrap(line: &str, width: usize) -> Vec<String> {
    if visible_width(line) <= width {
        return vec![line.to_string()];
    }
    let mut rows = Vec::new();
    let mut open: Option<String> = None;
    let mut row = String::new();
    let mut row_width = 0usize;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            let mut seq = String::from("\x1b");
            while let Some(&n) = chars.peek() {
                seq.push(n);
                chars.next();
                if n.is_ascii_alphabetic() { break; }
            }
            open = if RESETS.contains(&seq.as_str()) { None } else { Some(seq.clone()) };
            row.push_str(&seq);
            continue;
        }
        let w = c.width().unwrap_or(0);
        if row_width + w > width {
            if open.is_some() { row.push_str("\x1b[0m"); }
            rows.push(std::mem::take(&mut row));
            if let Some(o) = &open { row.push_str(o); }
            row_width = 0;
        }
        row.push(c);
        row_width += w;
    }
    if row_width > 0 || rows.is_empty() {
        if open.is_some() { row.push_str("\x1b[0m"); }
        rows.push(row);
    }
    rows
}

/// The shrink-wrapped code panel; geometry byte-pinned by the parity tests.
pub fn code_panel(theme: &Theme, code: &str, language: &str, cols: usize) -> Vec<String> {
    let source = code.trim_end_matches('\n');
    let lines: Vec<String> = if language.is_empty() {
        source.split('\n').map(String::from).collect()
    } else {
        source.split('\n').map(|l| highlight_line(theme, language, l)).collect()
    };

    let max_code_width = lines.iter().map(|l| visible_width(l)).max().unwrap_or(0);
    let label_width = if language.is_empty() { 0 } else { language.chars().count().min(cols.saturating_sub(5)) };
    let panel_width = (max_code_width + 4).max(label_width + 5).max(6).min(cols);
    let inner_width = panel_width - 4;

    let mut out = Vec::new();
    if label_width > 0 {
        let label: String = language.chars().take(panel_width.saturating_sub(5)).collect();
        let label = if label.is_empty() { "?".to_string() } else { label };
        let edge = "─".repeat(panel_width.saturating_sub(4 + label.chars().count()));
        out.push(format!("┌ {DIM_ON}{label}{WEIGHT_OFF} {edge}┐"));
    } else {
        out.push(format!("┌{}┐", "─".repeat(panel_width - 2)));
    }
    for line in &lines {
        for row in hard_wrap(line, inner_width) {
            let pad = " ".repeat(inner_width.saturating_sub(visible_width(&row)));
            out.push(format!("│ {row}{pad} │"));
        }
    }
    out.push(format!("└{}┘", "─".repeat(panel_width - 2)));
    out
}

#[derive(Default)]
struct ListState {
    ordered: Option<u64>,
}

/// Emit the current item's inline text as glyph-prefixed, hanging-indented rows.
fn flush_item(rows: &mut Vec<String>, lists: &mut [ListState], inline: &mut String, width: usize) {
    let depth = lists.len().saturating_sub(1);
    let pad = "  ".repeat(depth);
    let Some(state) = lists.last_mut() else { return };
    let (glyph, glyph_width) = match &mut state.ordered {
        Some(n) => {
            let g = format!("{n}. ");
            let w = g.chars().count();
            *n += 1;
            (g, w)
        }
        None => (format!("{}{}", dim("•"), " "), 2),
    };
    let hanging = format!("{pad}{}", " ".repeat(glyph_width));
    let body_width = width.saturating_sub(pad.chars().count() + glyph_width).max(8);
    for (i, row) in wrap_styled(inline.trim_end(), body_width).into_iter().enumerate() {
        if i == 0 { rows.push(format!("{pad}{glyph}{row}")); }
        else { rows.push(format!("{hanging}{row}")); }
    }
    inline.clear();
}

/// Render a markdown document to lines at `width`, one blank row between blocks.
pub fn render_markdown(theme: &Theme, markdown: &str, width: usize) -> Vec<String> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    let parser = Parser::new_ext(markdown, opts);

    let mut out: Vec<String> = Vec::new();
    let push_block = |out: &mut Vec<String>, lines: Vec<String>| {
        if lines.is_empty() { return; }
        if !out.is_empty() { out.push(String::new()); }
        out.extend(lines);
    };

    // Inline accumulation state.
    let mut inline = String::new();
    let mut heading: Option<u8> = None;
    let mut lists: Vec<ListState> = Vec::new();
    let mut item_first_lines: Vec<String> = Vec::new(); // rendered rows of current list block
    // One flag per open item: has its own inline text been emitted yet?
    let mut item_stack: Vec<bool> = Vec::new();
    let mut quote_depth = 0usize;
    let mut code: Option<(String, String)> = None; // (lang, buffer)
    let mut table: Option<(Vec<String>, Vec<Vec<String>>, bool)> = None; // header, rows, in_header

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(match level {
                    HeadingLevel::H1 => 1, HeadingLevel::H2 => 2, HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4, HeadingLevel::H5 => 5, HeadingLevel::H6 => 6,
                });
                inline.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                let text = heading_style(heading.take().unwrap_or(2), &inline);
                push_block(&mut out, vec![text]);
                inline.clear();
            }
            Event::Start(Tag::Paragraph) => inline.clear(),
            Event::End(TagEnd::Paragraph) => {
                if !item_stack.is_empty() {
                    // handled at item end via `inline`
                } else if quote_depth > 0 {
                    let body_width = width.saturating_sub(2).max(8);
                    let rail = quote_rail();
                    let rows: Vec<String> =
                        wrap_styled(&inline, body_width).into_iter().map(|r| format!("{rail}{r}")).collect();
                    push_block(&mut out, rows);
                    inline.clear();
                } else if table.is_none() {
                    push_block(&mut out, wrap_styled(&inline, width));
                    inline.clear();
                }
            }
            Event::Start(Tag::BlockQuote(_)) => { quote_depth += 1; }
            Event::End(TagEnd::BlockQuote(_)) => { quote_depth = quote_depth.saturating_sub(1); }
            Event::Start(Tag::List(start)) => {
                // A list opening inside an item means the item's own text is
                // done — emit it now so children render below their parent.
                if let Some(flushed) = item_stack.last_mut() {
                    if !*flushed {
                        flush_item(&mut item_first_lines, &mut lists, &mut inline, width);
                        *flushed = true;
                    }
                }
                lists.push(ListState { ordered: start });
                if lists.len() == 1 { item_first_lines.clear(); }
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    push_block(&mut out, std::mem::take(&mut item_first_lines));
                }
            }
            Event::Start(Tag::Item) => { item_stack.push(false); inline.clear(); }
            Event::End(TagEnd::Item) => {
                let flushed = item_stack.pop().unwrap_or(false);
                if !flushed {
                    flush_item(&mut item_first_lines, &mut lists, &mut inline, width);
                }
                inline.clear();
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(l) => l.split_whitespace().next().unwrap_or("").to_string(),
                    _ => String::new(),
                };
                code = Some((lang, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((lang, buffer)) = code.take() {
                    push_block(&mut out, code_panel(theme, &buffer, &lang, width));
                }
            }
            Event::Start(Tag::Table(_)) => { table = Some((Vec::new(), Vec::new(), false)); }
            Event::Start(Tag::TableHead) => { if let Some(t) = &mut table { t.2 = true; } }
            Event::End(TagEnd::TableHead) => { if let Some(t) = &mut table { t.2 = false; } }
            Event::Start(Tag::TableRow) => { if let Some(t) = &mut table { if !t.2 { t.1.push(Vec::new()); } } }
            Event::Start(Tag::TableCell) => inline.clear(),
            Event::End(TagEnd::TableCell) => {
                if let Some((header, rows, in_header)) = &mut table {
                    if *in_header { header.push(std::mem::take(&mut inline)); }
                    else if let Some(last) = rows.last_mut() { last.push(std::mem::take(&mut inline)); }
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some((header, rows, _)) = table.take() {
                    push_block(&mut out, render_table(&header, &rows));
                }
            }
            Event::Rule => push_block(&mut out, vec![rule()]),
            Event::Start(Tag::Strong) => inline.push_str(BOLD_ON),
            Event::End(TagEnd::Strong) => inline.push_str(WEIGHT_OFF),
            Event::Start(Tag::Emphasis) => inline.push_str(ITALIC_ON),
            Event::End(TagEnd::Emphasis) => inline.push_str(ITALIC_OFF),
            Event::Start(Tag::Strikethrough) => inline.push_str(STRIKE_ON),
            Event::End(TagEnd::Strikethrough) => inline.push_str(STRIKE_OFF),
            Event::Start(Tag::Link { dest_url, .. }) => {
                inline.push_str(&osc8(&dest_url));
                inline.push_str(UNDERLINE_ON);
            }
            Event::End(TagEnd::Link) => {
                inline.push_str(UNDERLINE_OFF);
                inline.push_str(OSC8_CLOSE);
            }
            Event::Code(text) => inline.push_str(&theme.fg("mdCode", &text)),
            Event::Text(text) => {
                if let Some((_, buffer)) = &mut code { buffer.push_str(&text); }
                else { inline.push_str(&text); }
            }
            Event::SoftBreak => inline.push(' '),
            Event::HardBreak => inline.push('\n'),
            Event::Html(html) | Event::InlineHtml(html) => inline.push_str(&html),
            _ => {}
        }
    }
    out
}

/// Table: ` │ ` separators, dim `─┼─` junction row, bold header.
fn render_table(header: &[String], rows: &[Vec<String>]) -> Vec<String> {
    let cols = header.len();
    let mut widths: Vec<usize> = header.iter().map(|h| visible_width(h)).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < cols { widths[i] = widths[i].max(visible_width(cell)); }
        }
    }
    let line = |cells: &[String], bold_row: bool| -> String {
        cells.iter().enumerate().map(|(i, c)| {
            let pad = " ".repeat(widths.get(i).copied().unwrap_or(0).saturating_sub(visible_width(c)));
            let cell = format!("{c}{pad}");
            if bold_row { bold(&cell) } else { cell }
        }).collect::<Vec<_>>().join(&format!(" {} ", dim("│")))
    };
    let mut out = vec![line(header, true)];
    out.push(dim(&widths.iter().map(|w| "─".repeat(*w)).collect::<Vec<_>>().join("─┼─")));
    for row in rows { out.push(line(row, false)); }
    out
}
