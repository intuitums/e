//! Terminal background detection.
//!
//! Asks the terminal for its background color (OSC 11 query) and reads the
//! reply off stdin with a bounded poll — no threads, nothing consumed on
//! terminals that never answer. Must run in raw mode, before the event
//! stream takes over stdin. Falls back to COLORFGBG, then dark.

use std::io::Write;

/// True if the terminal background is light (None when undetectable), plus
/// any keyboard bytes typed during the probe window — they share stdin with
/// the reply and must reach the composer, not vanish.
pub fn detect_light() -> (Option<bool>, Vec<u8>) {
    let (luminance, typed) = osc11_luminance();
    (luminance.map(|l| l > 0.5).or_else(colorfgbg), typed)
}

fn colorfgbg() -> Option<bool> {
    let value = std::env::var("COLORFGBG").ok()?;
    let bg: u8 = value.rsplit(';').next()?.parse().ok()?;
    Some(bg >= 7)
}

fn osc11_luminance() -> (Option<f32>, Vec<u8>) {
    // Query with a BEL terminator; terminals reply on stdin:
    //   ESC ] 11 ; rgb:RRRR/GGGG/BBBB (BEL | ESC \)
    let mut out = std::io::stdout();
    if out.write_all(b"\x1b]11;?\x07").is_err() || out.flush().is_err() {
        return (None, Vec::new());
    }

    // Keyboard input shares this fd: bytes before the OSC reply (and after
    // its terminator) are keystrokes, returned to the caller — never eaten.
    let mut buffer = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(400);
    loop {
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return (None, buffer);
        };
        let mut fds = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fds, 1, remaining.as_millis() as i32) };
        if ready <= 0 {
            return (None, buffer);
        }
        let mut byte = [0u8; 64];
        let n = unsafe { libc::read(0, byte.as_mut_ptr().cast(), byte.len()) };
        if n <= 0 {
            return (None, buffer);
        }
        buffer.extend_from_slice(&byte[..n as usize]);
        if let Some((reply, typed)) = split_reply(&buffer) {
            return (parse_osc11(&reply), typed);
        }
        if buffer.len() > 512 {
            return (None, buffer);
        }
    }
}

/// Cut a complete OSC reply out of the read bytes. Returns the reply text
/// and everything else (leading and trailing keystrokes), or None while the
/// reply is still incomplete.
fn split_reply(buffer: &[u8]) -> Option<(String, Vec<u8>)> {
    let start = buffer.windows(2).position(|w| w == b"\x1b]")?;
    let osc = &buffer[start..];
    // Terminated by BEL or ST (ESC \).
    let end = osc
        .iter()
        .position(|b| *b == 0x07)
        .map(|i| i + 1)
        .or_else(|| {
            osc.windows(2)
                .skip(1)
                .position(|w| w == b"\x1b\\")
                .map(|i| i + 3)
        })?;
    let mut typed = buffer[..start].to_vec();
    typed.extend_from_slice(&osc[end..]);
    Some((String::from_utf8_lossy(&osc[..end]).into_owned(), typed))
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

/// Whether stdout is a terminal — `e ask` styles for a human, streams plain
/// for a pipe.
pub fn stdout_is_tty() -> bool {
    unsafe { libc::isatty(1) == 1 }
}

#[cfg(test)]
mod tests {
    use super::{parse_osc11, split_reply};

    /// Keystrokes sharing stdin with the OSC reply come back out — typed
    /// text during startup must reach the composer, not vanish.
    #[test]
    fn split_reply_returns_keystrokes_around_the_reply() {
        let mut bytes = b"hel".to_vec();
        bytes.extend_from_slice(b"\x1b]11;rgb:ffff/ffff/ffff\x07");
        bytes.extend_from_slice(b"lo");
        let (reply, typed) = split_reply(&bytes).unwrap();
        assert!(reply.contains("rgb:ffff"));
        assert_eq!(typed, b"hello");
        // Incomplete replies wait for more bytes.
        assert!(split_reply(b"typed\x1b]11;rgb:ff").is_none());
    }

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
