//! SGR style primitives.
//!
//! Every emphasis in this design is an SGR attribute — bold, dim, underline —
//! never a hue; the whole palette is a grayscale ramp in the theme files.
//! These emit the exact byte sequences the reference design does, pinned by
//! the parity tests.

pub const BOLD_ON: &str = "\x1b[1m";
pub const DIM_ON: &str = "\x1b[2m";
pub const ITALIC_ON: &str = "\x1b[3m";
pub const ITALIC_OFF: &str = "\x1b[23m";
pub const UNDERLINE_ON: &str = "\x1b[4m";
/// SGR 22 clears bold *and* dim; SGR 24 clears underline.
pub const WEIGHT_OFF: &str = "\x1b[22m";
pub const UNDERLINE_OFF: &str = "\x1b[24m";
pub const STRIKE_ON: &str = "\x1b[9m";
pub const STRIKE_OFF: &str = "\x1b[29m";

pub fn bold(s: &str) -> String {
    format!("{BOLD_ON}{s}{WEIGHT_OFF}")
}

pub fn italic(s: &str) -> String {
    format!("{ITALIC_ON}{s}{ITALIC_OFF}")
}

pub fn dim(s: &str) -> String {
    format!("{DIM_ON}{s}{WEIGHT_OFF}")
}

/// Heading styles per level: bold+underline / bold / underline / bold+dim /
/// dim+underline / dim — the reference level table, byte-pinned.
pub fn heading_style(level: u8, text: &str) -> String {
    match level {
        1 => format!("{BOLD_ON}{UNDERLINE_ON}{text}{UNDERLINE_OFF}{WEIGHT_OFF}"),
        2 => format!("{BOLD_ON}{text}{WEIGHT_OFF}"),
        3 => format!("{UNDERLINE_ON}{text}{UNDERLINE_OFF}"),
        4 => format!("{BOLD_ON}{DIM_ON}{text}{WEIGHT_OFF}"),
        5 => format!("{DIM_ON}{UNDERLINE_ON}{text}{UNDERLINE_OFF}{WEIGHT_OFF}"),
        _ => format!("{DIM_ON}{text}{WEIGHT_OFF}"),
    }
}

/// Thematic rule: a fixed 60 columns of `─` in SGR dim — never the content
/// width, never a palette color.
pub const RULE_WIDTH: usize = 60;

pub fn rule() -> String {
    format!("{DIM_ON}{}{WEIGHT_OFF}", "─".repeat(RULE_WIDTH))
}

/// Blockquote rail: dim `│ `, with the quoted body left at default weight.
pub fn quote_rail() -> String {
    format!("{DIM_ON}│ {WEIGHT_OFF}")
}
