//! Terminal background detection.
//!
//! Theme detection is deliberately stdin-free. An OSC 11 query is answered
//! on stdin, and reading that reply would race whatever reader owns the
//! terminal — at startup it swallows keystrokes typed during the probe
//! window (audit #93). So detection reads only `COLORFGBG` (the
//! rxvt/iTerm-style report), falling back to dark.

/// True if the terminal background is light. None when undetectable.
pub fn detect_light() -> Option<bool> {
    colorfgbg()
}

fn colorfgbg() -> Option<bool> {
    let value = std::env::var("COLORFGBG").ok()?;
    parse_colorfgbg(&value)
}

/// `COLORFGBG` is `fg;bg` (a few terminals add a middle flag); the last
/// segment is the background, light when 7 (light gray) or above.
fn parse_colorfgbg(value: &str) -> Option<bool> {
    let bg: u8 = value.rsplit(';').next()?.parse().ok()?;
    Some(bg >= 7)
}

/// Whether stdout is a terminal — `e ask` styles for a human, streams plain
/// for a pipe.
pub fn stdout_is_tty() -> bool {
    unsafe { libc::isatty(1) == 1 }
}

#[cfg(test)]
mod tests {
    use super::parse_colorfgbg;

    #[test]
    fn light_and_dark_reports() {
        // Standard `fg;bg` pairs.
        assert_eq!(parse_colorfgbg("15;0"), Some(false));
        assert_eq!(parse_colorfgbg("0;15"), Some(true));
        // Light gray (7) counts as light, matching the original threshold.
        assert_eq!(parse_colorfgbg("0;7"), Some(true));
        // A three-part report with a middle flag.
        assert_eq!(parse_colorfgbg("0;0;15"), Some(true));
        // Garbage falls back to None, never panics.
        assert_eq!(parse_colorfgbg("garbage"), None);
        assert_eq!(parse_colorfgbg(""), None);
    }
}
