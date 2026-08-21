//! The main-screen line renderer.
//!
//! The frame is a flat list of styled lines. Each paint diffs against the
//! previous frame, moves the cursor up to the first changed row still inside
//! the viewport, and rewrites from there — the transcript scrolls naturally
//! into the terminal's scrollback and only the live tail repaints. Paints are
//! wrapped in synchronized output (?2026) to kill flicker where supported.
//!
//! Invariant: after `paint`, the cursor rests at the end of the frame's last
//! row (row index `len - 1`), which is what the relative moves assume.

use std::io::{self, Write};

pub struct Screen {
    prev: Vec<String>,
    pub cols: u16,
    pub rows: u16,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Screen { prev: Vec::new(), cols, rows }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.prev.clear();
        let mut out = io::stdout();
        let _ = write!(out, "\x1b[2J\x1b[H");
        let _ = out.flush();
    }

    pub fn paint(&mut self, lines: &[String]) -> io::Result<()> {
        if lines == self.prev.as_slice() {
            return Ok(());
        }
        if std::env::var("E_DEBUG_FRAMES").is_ok() {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/e-frames.log").unwrap();
            let _ = writeln!(f, "== frame {} rows ==", lines.len());
            for l in lines { let _ = writeln!(f, "{:?}", l); }
        }
        let mut out = io::stdout().lock();
        write!(out, "\x1b[?2026h\x1b[?25l")?;

        if self.prev.is_empty() {
            for (i, line) in lines.iter().enumerate() {
                write!(out, "{line}\x1b[K")?;
                if i + 1 < lines.len() {
                    write!(out, "\r\n")?;
                }
            }
        } else {
            let viewport = self.rows.saturating_sub(1) as usize;
            let common = self.prev.iter().zip(lines.iter()).take_while(|(a, b)| a == b).count();
            // Rows above the viewport window are scrollback: unreachable.
            let window_start = self.prev.len().saturating_sub(viewport);
            let start = common.max(window_start).min(lines.len());

            // Cursor sits at the end of the previous last row.
            let cursor_row = self.prev.len() - 1;
            if start < cursor_row {
                write!(out, "\x1b[{}F", cursor_row - start)?;
            } else {
                write!(out, "\r")?;
            }
            for (i, line) in lines.iter().enumerate().skip(start) {
                write!(out, "{line}\x1b[K")?;
                if i + 1 < lines.len() {
                    write!(out, "\r\n")?;
                }
            }
            if lines.len() < self.prev.len() {
                // Wipe the leftover rows, then park back on the last real row.
                let extra = self.prev.len() - lines.len();
                for _ in 0..extra {
                    write!(out, "\r\n\x1b[K")?;
                }
                write!(out, "\x1b[{extra}A")?;
                if let Some(last) = lines.last() {
                    write!(out, "\r{last}\x1b[K")?;
                }
            }
        }
        write!(out, "\x1b[?2026l")?;
        out.flush()?;
        self.prev = lines.to_vec();
        Ok(())
    }
}
