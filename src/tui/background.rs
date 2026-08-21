//! Terminal background detection.
//!
//! Asks the terminal for its background color (OSC 11 query) and reads the
//! reply off stdin with a bounded poll — no threads, nothing consumed on
//! terminals that never answer. Must run in raw mode, before the event
//! stream takes over stdin. Falls back to COLORFGBG, then dark.

use std::io::Write;

/// True if the terminal background is light. None when undetectable.
pub fn detect_light() -> Option<bool> {
    osc11_luminance().map(|l| l > 0.5).or_else(colorfgbg)
}

fn colorfgbg() -> Option<bool> {
    let value = std::env::var("COLORFGBG").ok()?;
    let bg: u8 = value.rsplit(';').next()?.parse().ok()?;
    Some(bg >= 7)
}

fn osc11_luminance() -> Option<f32> {
    // Query with a BEL terminator; terminals reply on stdin:
    //   ESC ] 11 ; rgb:RRRR/GGGG/BBBB (BEL | ESC \)
    let mut out = std::io::stdout();
    out.write_all(b"\x1b]11;?\x07").ok()?;
    out.flush().ok()?;

    let mut reply = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
    loop {
        let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
        let mut fds = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut fds, 1, remaining.as_millis() as i32) };
        if ready <= 0 {
            return None;
        }
        let mut byte = [0u8; 64];
        let n = unsafe { libc::read(0, byte.as_mut_ptr().cast(), byte.len()) };
        if n <= 0 {
            return None;
        }
        reply.extend_from_slice(&byte[..n as usize]);
        // Terminated by BEL or ST.
        if reply.contains(&0x07) || reply.windows(2).any(|w| w == b"\x1b\\") {
            break;
        }
        if reply.len() > 256 {
            return None;
        }
    }
    parse_osc11(&String::from_utf8_lossy(&reply))
}

/// Parse `rgb:RRRR/GGGG/BBBB` (or 2-digit components) out of the reply.
fn parse_osc11(reply: &str) -> Option<f32> {
    let start = reply.find("rgb:")? + 4;
    let body: String = reply[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '/')
        .collect();
    let mut parts = body.split('/');
    let mut channel = || -> Option<f32> {
        let hex = parts.next()?;
        let max = (16f32).powi(hex.len() as i32) - 1.0;
        Some(u32::from_str_radix(hex, 16).ok()? as f32 / max)
    };
    let (r, g, b) = (channel()?, channel()?, channel()?);
    Some(0.2126 * r + 0.7152 * g + 0.0722 * b)
}

#[cfg(test)]
mod tests {
    use super::parse_osc11;

    #[test]
    fn parses_standard_and_short_replies() {
        // Near-white background → high luminance.
        let l = parse_osc11("\x1b]11;rgb:ffff/ffff/ffff\x07").unwrap();
        assert!(l > 0.99);
        // Near-black → low.
        let l = parse_osc11("\x1b]11;rgb:1c1c/1c1c/1c1c\x1b\\").unwrap();
        assert!(l < 0.15);
        // Two-digit components some terminals send.
        let l = parse_osc11("\x1b]11;rgb:ff/ff/ff\x07").unwrap();
        assert!(l > 0.99);
        assert!(parse_osc11("garbage").is_none());
    }
}
