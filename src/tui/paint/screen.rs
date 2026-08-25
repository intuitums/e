//! The main-screen line renderer.
//!
//! The frame is a flat list of styled lines. The visible window is the last
//! `rows` lines of the frame (a short frame sits at the top of the screen,
//! as the reference launch does). Each paint diffs the window row-by-row
//! against a shadow of what every screen row currently shows, and rewrites
//! only the rows that changed, at absolute positions. When the frame
//! overflows further down the screen, the display scrolls up first so the
//! transcript keeps flowing into the terminal's scrollback, and only the
//! blank rows at the bottom get painted. Paints are wrapped in synchronized
//! output (?2026) to kill flicker where supported.
//!
//! There is deliberately no cursor arithmetic. Every row run starts with a
//! `\r` followed by an absolute position, so no relative move can ever
//! interact with the terminal's pending-wrap state — the `\x1b[F` off-by-one
//! bug class (issue #123) cannot occur, because no motion depends on where
//! the cursor was left.

use std::io::{self, Write};

use crate::tui::markdown::{clip_styled, visible_width};

pub struct Screen {
    /// The last painted frame, for the no-op fast path.
    prev: Vec<String>,
    /// Per-screen-row shadow: what each screen row currently shows; None
    /// when unknown (never painted, or scrolled-in blank).
    shadow: Vec<Option<String>>,
    /// True after a resize: rows outside the window must be cleared even
    /// when the shadow never painted them — the terminal reflowed stale
    /// content into them.
    clear_unpainted: bool,
    pub cols: u16,
    pub rows: u16,
}

/// What one screen row of the next paint must do.
pub(crate) struct RowAction<'a> {
    /// The screen row (0-based) to rewrite.
    pub row: usize,
    /// The content to paint; None clears the row.
    pub write: Option<&'a str>,
}

/// Which screen rows need a rewrite for `lines` to be visible, given the
/// `shadow` of what each row currently shows. The window is the frame's
/// last `height` lines; rows outside it are cleared when they may hold
/// stale content (`clear_unpainted`), and left alone when they are already
/// blank or were never painted (the pre-launch terminal content stays).
pub(crate) fn plan<'a>(
    lines: &'a [String],
    height: usize,
    shadow: &[Option<String>],
    clear_unpainted: bool,
) -> Vec<RowAction<'a>> {
    let len = lines.len();
    let n0 = len.saturating_sub(height);
    let in_window = len.saturating_sub(n0); // min(len, height)
    let mut actions = Vec::new();
    for row in 0..height {
        let action = if row < in_window {
            let line = &lines[n0 + row];
            if shadow
                .get(row)
                .and_then(|s| s.as_deref())
                .is_some_and(|s| s == line.as_str())
            {
                None
            } else {
                Some(RowAction {
                    row,
                    write: Some(line),
                })
            }
        } else {
            match shadow.get(row).and_then(|s| s.as_deref()) {
                Some("") => None,
                Some(_) => Some(RowAction { row, write: None }),
                None if clear_unpainted => Some(RowAction { row, write: None }),
                None => None,
            }
        };
        if let Some(a) = action {
            actions.push(a);
        }
    }
    actions
}

/// How many rows the display must scroll up so the frame's visible window
/// (its last `height` lines) sits at screen rows 0..height. Zero when the
/// window did not move downward — shrinking frames repaint in place rather
/// than pulling from scrollback.
pub(crate) fn scroll_rows(prev_len: usize, len: usize, height: usize) -> usize {
    let prev_overflow = prev_len.saturating_sub(height);
    let overflow = len.saturating_sub(height);
    overflow.saturating_sub(prev_overflow)
}

impl Screen {
    pub fn new(cols: u16, rows: u16) -> Self {
        Screen {
            prev: Vec::new(),
            shadow: vec![None; rows as usize],
            clear_unpainted: false,
            cols,
            rows,
        }
    }

    /// A resize never clears the display: the shadow goes unknown and the
    /// next paint rewrites the whole window in place (and clears anything
    /// below a short frame) — no blank-flash frame.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.prev.clear();
        self.shadow = vec![None; rows as usize];
        self.clear_unpainted = true;
    }

    pub fn paint(&mut self, lines: &[String]) -> io::Result<()> {
        if lines == self.prev.as_slice() {
            return Ok(());
        }
        if std::env::var("E_DEBUG_FRAMES").is_ok() {
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
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let scroll = scroll_rows(self.prev.len(), lines.len(), rows);

        // A line that fills the row leaves the cursor in the pending-wrap
        // state, where erase-to-end clears the cell under it — eating the
        // line's last character. Full rows need no erase at all.
        let put = |out: &mut std::io::StdoutLock<'_>, line: &str| -> io::Result<()> {
            if visible_width(line) > cols {
                // An overlong line would wrap physically and desync the row
                // differ — clip it; producers should wrap, this is the net.
                write!(out, "{}", clip_styled(line, cols))
            } else if visible_width(line) == cols {
                write!(out, "{line}")
            } else {
                write!(out, "{line}\x1b[K")
            }
        };

        let mut out = io::stdout().lock();
        write!(out, "\x1b[?2026h\x1b[?25l")?;

        // The window moved down the frame: scroll the display up by that
        // many rows so the top rows enter the terminal's scrollback and
        // blank rows appear at the bottom for the new content. The `\r`
        // homes the cursor without resolving a pending wrap first (#123).
        if scroll > 0 {
            write!(out, "\r\x1b[{rows};1H")?;
            for _ in 0..scroll {
                writeln!(out)?;
            }
            self.shadow.drain(0..scroll);
            self.shadow.extend((0..scroll).map(|_| None));
        }

        let actions = plan(lines, rows, &self.shadow, self.clear_unpainted);
        // Runs of adjacent dirty rows: position once, then `\r\n` between
        // rows. The `\r` before the absolute position is what keeps the
        // differ independent of the terminal's pending-wrap state.
        let mut at = 0usize;
        while at < actions.len() {
            let mut end = at + 1;
            while end < actions.len() && actions[end].row == actions[end - 1].row + 1 {
                end += 1;
            }
            write!(out, "\r\x1b[{};1H", actions[at].row + 1)?;
            for (i, action) in actions[at..end].iter().enumerate() {
                if i > 0 {
                    write!(out, "\r\n")?;
                }
                match action.write {
                    Some(line) => {
                        put(&mut out, line)?;
                        self.shadow[action.row] = Some(line.to_string());
                    }
                    None => {
                        // Below the frame: leave the row blank.
                        write!(out, "\x1b[2K")?;
                        self.shadow[action.row] = Some(String::new());
                    }
                }
            }
            at = end;
        }
        self.clear_unpainted = false;
        write!(out, "\x1b[?2026l")?;
        out.flush()?;
        self.prev = lines.to_vec();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shadow(s: &[Option<&str>]) -> Vec<Option<String>> {
        s.iter().map(|o| o.map(str::to_string)).collect()
    }

    #[test]
    fn short_frame_paints_the_blank_screen_rows() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let actions = plan(&lines, 3, &shadow(&[None, None, None]), false);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].row, 0);
        assert_eq!(actions[0].write, Some("a"));
        assert_eq!(actions[1].row, 1);
        assert_eq!(actions[1].write, Some("b"));
    }

    #[test]
    fn unchanged_rows_are_skipped_but_dirty_rows_rewrite() {
        let lines = vec!["a".to_string(), "b".to_string()];
        let actions = plan(&lines, 3, &shadow(&[Some("a"), None, None]), false);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].row, 1);
    }

    #[test]
    fn tall_frame_maps_its_tail_onto_the_screen() {
        let lines: Vec<String> = (0..5).map(|i| format!("row{i}")).collect();
        let actions = plan(&lines, 3, &shadow(&[None, None, None]), false);
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].write, Some("row2"));
        assert_eq!(actions[2].write, Some("row4"));
    }

    #[test]
    fn rows_below_a_short_frame_clear_only_when_painted() {
        let lines = vec!["a".to_string()];
        // Painted content below the last row: cleared.
        let actions = plan(&lines, 3, &shadow(&[Some("a"), Some("stale"), None]), false);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].row, 1);
        assert_eq!(actions[0].write, None);
        // Already blank: left alone.
        assert!(plan(&lines, 3, &shadow(&[Some("a"), Some(""), None]), false).is_empty());
        // Never painted and not forced: left alone (launch keeps the
        // pre-existing terminal content, the reference behavior).
        assert!(plan(&lines, 3, &shadow(&[Some("a"), None, None]), false).is_empty());
        // Never painted but forced (after a resize): cleared.
        let actions = plan(&lines, 3, &shadow(&[Some("a"), None, None]), true);
        assert_eq!(actions.len(), 2);
        assert!(actions.iter().all(|a| a.write.is_none()));
    }

    #[test]
    fn scroll_rows_tracks_window_movement_only() {
        // Both short: no scroll.
        assert_eq!(scroll_rows(2, 3, 10), 0);
        // Tall growth overflows one further row.
        assert_eq!(scroll_rows(20, 23, 10), 3);
        // Short→tall scrolls the overflow.
        assert_eq!(scroll_rows(2, 13, 10), 3);
        // Shrinking never scrolls — the window repaints in place.
        assert_eq!(scroll_rows(13, 10, 10), 0);
        assert_eq!(scroll_rows(23, 20, 10), 0);
    }
}
