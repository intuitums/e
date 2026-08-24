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

/// The paint thread's single-slot mailbox: a newer frame replaces the
/// undelivered one, so a terminal blocked mid-write bounds the backlog to
/// exactly one pending frame — an unbounded queue would grow by a full
/// transcript copy per tick for as long as the write stalls.
#[derive(Default)]
struct PaintMailbox {
    frame: Option<Vec<String>>,
    /// Latest wins here too; the resize clears the screen, so painting the
    /// pre-resize frame once at the new size is a one-frame blip at most.
    resize: Option<(u16, u16)>,
    shutdown: bool,
}

/// The paint thread: owns the `Screen` and its blocking stdout writes so a
/// slow terminal can never stall the event loop.
pub struct Painter {
    mailbox: std::sync::Arc<(std::sync::Mutex<PaintMailbox>, std::sync::Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Painter {
    pub fn spawn(cols: u16, rows: u16) -> Self {
        let mailbox = std::sync::Arc::new((
            std::sync::Mutex::new(PaintMailbox::default()),
            std::sync::Condvar::new(),
        ));
        let shared = mailbox.clone();
        let thread = std::thread::spawn(move || {
            let mut screen = Screen::new(cols, rows);
            let (lock, wake) = &*shared;
            loop {
                let (frame, resize, shutdown) = {
                    let mut box_ = lock.lock().unwrap();
                    while box_.frame.is_none() && box_.resize.is_none() && !box_.shutdown {
                        box_ = wake.wait(box_).unwrap();
                    }
                    (box_.frame.take(), box_.resize.take(), box_.shutdown)
                };
                if let Some((cols, rows)) = resize {
                    screen.resize(cols, rows);
                }
                if let Some(frame) = frame {
                    let _ = screen.paint(&frame);
                }
                if shutdown {
                    // The final frame (taken above) has landed; done.
                    break;
                }
            }
        });
        Painter {
            mailbox,
            thread: Some(thread),
        }
    }

    fn post(&self, update: impl FnOnce(&mut PaintMailbox)) {
        let (lock, wake) = &*self.mailbox;
        update(&mut lock.lock().unwrap());
        wake.notify_one();
    }

    pub fn frame(&self, lines: Vec<String>) {
        self.post(|mailbox| mailbox.frame = Some(lines));
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.post(|mailbox| mailbox.resize = Some((cols, rows)));
    }

    /// Flush and stop: the pending frame lands before terminal teardown.
    pub fn shutdown(&mut self) {
        self.post(|mailbox| mailbox.shutdown = true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
