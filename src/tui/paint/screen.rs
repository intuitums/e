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

use crate::tui::markdown::visible_width;

pub struct Screen {
    prev: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    debug_frames: bool,
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Screen {
            prev: Vec::new(),
            cols,
            rows,
            debug_frames: std::env::var("E_DEBUG_FRAMES").is_ok(),
        }
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
        if self.debug_frames {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/e-frames.log")
                .unwrap();
            let _ = writeln!(f, "== frame {} rows ==", lines.len());
            for l in lines {
                let _ = writeln!(f, "{:?}", l);
            }
        }
        let mut out = io::stdout().lock();
        write!(out, "\x1b[?2026h\x1b[?25l")?;

        let cols = self.cols as usize;
        // A line that fills the row leaves the cursor in the pending-wrap
        // state, where erase-to-end clears the cell under it — eating the
        // line's last character. Full rows need no erase at all.
        let put = |out: &mut std::io::StdoutLock<'_>, line: &String| -> io::Result<()> {
            if visible_width(line) > cols {
                // An overlong line would wrap physically and desync the row
                // differ — clip it; producers should wrap, this is the net.
                write!(out, "{}", crate::tui::markdown::clip_styled(line, cols))
            } else if visible_width(line) == cols {
                write!(out, "{line}")
            } else {
                write!(out, "{line}\x1b[K")
            }
        };

        if self.prev.is_empty() {
            for (i, line) in lines.iter().enumerate() {
                put(&mut out, line)?;
                if i + 1 < lines.len() {
                    write!(out, "\r\n")?;
                }
            }
        } else {
            let viewport = self.rows.saturating_sub(1) as usize;
            let common = self
                .prev
                .iter()
                .zip(lines.iter())
                .take_while(|(a, b)| a == b)
                .count();
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
                put(&mut out, line)?;
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
                    write!(out, "\r")?;
                    put(&mut out, last)?;
                }
            }
        }
        write!(out, "\x1b[?2026l")?;
        out.flush()?;
        self.prev = lines.to_vec();
        Ok(())
    }
}

/// Messages to the paint thread.
pub enum PaintMsg {
    Frame(Vec<String>),
    Resize(u16, u16),
}

/// The paint thread: owns the `Screen` and its blocking stdout writes so a
/// slow terminal can never stall the event loop. Frames are latest-wins —
/// each wake drains the queue and paints only the newest frame; resizes are
/// applied in order.
pub struct Painter {
    tx: Option<std::sync::mpsc::Sender<PaintMsg>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Painter {
    pub fn spawn(cols: u16, rows: u16) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut screen = Screen::new(cols, rows);
            fn apply(screen: &mut Screen, latest: &mut Option<Vec<String>>, msg: PaintMsg) {
                match msg {
                    PaintMsg::Frame(frame) => *latest = Some(frame),
                    PaintMsg::Resize(cols, rows) => screen.resize(cols, rows),
                }
            }
            while let Ok(first) = rx.recv() {
                let mut latest = None;
                apply(&mut screen, &mut latest, first);
                while let Ok(next) = rx.try_recv() {
                    apply(&mut screen, &mut latest, next);
                }
                if let Some(frame) = latest {
                    let _ = screen.paint(&frame);
                }
            }
        });
        Painter {
            tx: Some(tx),
            thread: Some(thread),
        }
    }

    pub fn frame(&self, lines: Vec<String>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(PaintMsg::Frame(lines));
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(PaintMsg::Resize(cols, rows));
        }
    }

    /// Flush and stop: queued frames land before terminal teardown.
    pub fn shutdown(&mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
