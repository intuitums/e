//! Terminal background detection.
//!
//! `auto` follows the terminal. We prefer an OSC 11 background query — the
//! terminal's real RGB background — then fall back to the `COLORFGBG` env
//! report, and finally assume dark. The OSC 11 probe runs where the TUI owns
//! the terminal reader (after raw mode) with a short timeout, so it can't
//! block startup or swallow keystrokes; if no clean reply arrives we fall
//! back. (The libc poll/read here is the audited terminal-poll site —
//! `guard.sh` permits `unsafe` only in this file and the bash tool.)

use std::time::{Duration, Instant};

/// How long to wait for the terminal's OSC 11 reply before giving up.
const PROBE_TIMEOUT: Duration = Duration::from_millis(100);

/// True if the terminal background is light. None when undetectable.
pub fn detect_light() -> Option<bool> {
    if stdout_is_tty() {
        if let Some((r, g, b)) = query_background_rgb() {
            return Some(is_light_rgb(r, g, b));
        }
    }
    colorfgbg()
}

/// Query the terminal background color over OSC 11 and return it as RGB.
/// Returns None if the terminal doesn't answer within [`PROBE_TIMEOUT`] or the
/// reply isn't a recognizable color.
fn query_background_rgb() -> Option<(u8, u8, u8)> {
    #[cfg(unix)]
    {
        use std::io::Write;
        // OSC 11 ; ?  — ask for the background color, terminated with ST.
        let _ = std::io::stdout().write_all(b"\x1b]11;?\x1b\\");
        let _ = std::io::stdout().flush();

        let fd = libc::STDIN_FILENO;
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return None;
        }
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }

        let deadline = Instant::now() + PROBE_TIMEOUT;
        let mut buf: Vec<u8> = Vec::new();
        let result = 'probe: {
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break 'probe None;
                }
                let timeout_ms = (deadline - now).as_millis() as libc::c_int;
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN as libc::c_short,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut pfd, 1 as libc::nfds_t, timeout_ms) } <= 0 {
                    break 'probe None;
                }
                let mut chunk = [0u8; 256];
                let r =
                    unsafe { libc::read(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len()) };
                if r <= 0 {
                    break 'probe None;
                }
                buf.extend_from_slice(&chunk[..r as usize]);
                // A background query ends with BEL (0x07) or ST (ESC \).
                if buf.ends_with(b"\x1b\\") || buf.ends_with(b"\x07") {
                    break 'probe parse_osc11(&buf);
                }
                if buf.len() > 1024 {
                    break 'probe None;
                }
            }
        };
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags);
        }
        result
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Parse an OSC 11 background reply (`…]11;[N;]color…`) into RGB.
fn parse_osc11(buf: &[u8]) -> Option<(u8, u8, u8)> {
    let s = String::from_utf8_lossy(buf);
    let marker = s.find("]11;")? + 4;
    let rest = &s[marker..];
    // Optional "N;" dynamic-color prefix some terminals prepend.
    let spec = match rest.find(';') {
        Some(idx) if !rest[..idx].is_empty() && rest[..idx].chars().all(|c| c.is_ascii_digit()) => {
            &rest[idx + 1..]
        }
        _ => rest,
    };
    let spec = spec.trim();
    let spec = spec.strip_suffix('\u{7}').unwrap_or(spec);
    let spec = spec.strip_suffix("\u{1b}\\").unwrap_or(spec);
    parse_color_spec(spec.trim())
}

/// Parse a color spec: `rgb:r/g/b` (8- or 16-bit), `#rrggbb`, `xrrggbb`,
/// or bare `rrggbb` / `rgb`.
fn parse_color_spec(spec: &str) -> Option<(u8, u8, u8)> {
    let spec = spec.trim();
    if let Some(rest) = spec.strip_prefix("rgb:") {
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() != 3 {
            return None;
        }
        let mut out = [0u8; 3];
        for (i, p) in parts.iter().enumerate() {
            let v = u16::from_str_radix(p, 16).ok()?;
            // 8-bit components are 0–255; 16-bit (kitty) are 0–65535 — take
            // the high byte so both forms land in 0–255.
            out[i] = if v > 255 { (v >> 8) as u8 } else { v as u8 };
        }
        return Some((out[0], out[1], out[2]));
    }
    let hex = spec
        .strip_prefix('#')
        .or_else(|| spec.strip_prefix('x'))
        .unwrap_or(spec)
        .trim();
    let bytes = hex.as_bytes();
    if bytes.iter().all(|b| b.is_ascii_hexdigit()) {
        if bytes.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some((r, g, b));
        }
        if bytes.len() == 3 {
            let r = u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?;
            let g = u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?;
            let b = u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?;
            return Some((r, g, b));
        }
    }
    None
}

/// WCAG relative luminance; a background is "light" at >= 0.5 (matches Pi).
fn is_light_rgb(r: u8, g: u8, b: u8) -> bool {
    let lin = |c: u8| {
        let v = c as f64 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let lum = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    lum >= 0.5
}

/// `COLORFGBG` is `fg;bg` (a few terminals add a middle flag); the last
/// segment is the background, light when 7 (light gray) or above.
fn colorfgbg() -> Option<bool> {
    let value = std::env::var("COLORFGBG").ok()?;
    parse_colorfgbg(&value)
}

fn parse_colorfgbg(value: &str) -> Option<bool> {
    let bg: u8 = value.rsplit(';').next()?.parse().ok()?;
    Some(bg >= 7)
}

/// Whether stdout is a terminal: styling and background detection apply only
/// when a human is watching, not when output is piped.
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

    #[test]
    fn osc11_reply_parsing() {
        // xterm 8-bit rgb.
        assert_eq!(
            super::parse_osc11(b"\x1b]11;rgb:1e/1e/1e\x07"),
            Some((30, 30, 30))
        );
        // Hex form.
        assert_eq!(
            super::parse_osc11(b"\x1b]11;#e5e5e7\x07"),
            Some((229, 229, 231))
        );
        // kitty 16-bit rgb with the dynamic-color "1;" prefix, ST terminator.
        assert_eq!(
            super::parse_osc11(b"\x1b]11;1;rgb:ffff/ffff/ffff\x1b\\"),
            Some((255, 255, 255))
        );
        // Nonsense yields nothing.
        assert_eq!(super::parse_osc11(b"garbage"), None);
    }

    #[test]
    fn luminance_split() {
        assert!(super::is_light_rgb(255, 255, 255));
        assert!(!super::is_light_rgb(0, 0, 0));
        // Mid gray (~128) is below the 0.5 threshold -> dark.
        assert!(!super::is_light_rgb(128, 128, 128));
        // Near-white is light.
        assert!(super::is_light_rgb(230, 230, 230));
    }
}
