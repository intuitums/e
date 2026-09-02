//! The main-screen line renderer, anchored where the user launched e.
//!
//! The frame is a flat list of styled lines — the transcript with the
//! composer dock at its tail. The screen shows a window of that buffer:
//! buffer row `b` sits at screen row `anchor + b - viewport_top`, where
//! `anchor` is the cursor row at launch and `viewport_top` counts the rows
//! the display has scrolled up as the frame outgrew the screen. Rows above
//! the anchor hold whatever the terminal showed before e started and are
//! never touched: the UI grows downward from where the user launched it,
//! exactly like a plain program appending below its prompt (pi's
//! regular-mode renderer, which this model mirrors). Once the frame
//! overflows, the display scrolls and the transcript flows into the
//! terminal's scrollback above the dock.
//!
//! Three paint routes:
//!
//! - Diff: rewrite the changed rows at absolute positions, scrolling first
//!   when the frame's tail must move the display up.
//! - Flow: the changed range starts between the old and new screen tops —
//!   a large append, or the first frame of a session whose transcript
//!   already overflows. The range prints sequentially from where it starts
//!   and the terminal scrolls as the rows flow, so the head lands in the
//!   scrollback in order: no gap, no clear.
//! - Replay: a resize reflows everything on screen, so row positions are
//!   unknowable — the screen (and scrollback, which would otherwise stack
//!   replayed heads) is cleared and the whole frame is printed from the
//!   screen top. Changes that land above the screen top without a resize
//!   (a compaction shrink, an edit above the window) patch only the
//!   visible range and leave the stale rows above stale: the pre-launch
//!   content there is worth more than a perfect scrollback.
//!
//! There is deliberately no cursor arithmetic. Every absolute write starts
//! with a `\r` followed by a position, so no relative move can ever
//! interact with the terminal's pending-wrap state — the `\x1b[F` off-by-one
//! bug class (issue #123) cannot occur, because no motion depends on where
//! the cursor was left.

use std::io::{self, Write};

use crate::tui::markdown::{clip_styled, visible_width};

pub struct Screen {
    /// The last painted frame, for the no-op fast path.
    prev: Vec<String>,
    /// Per-screen-row shadow: what each screen row currently shows; None
    /// when unknown (never painted).
    shadow: Vec<Option<String>>,
    /// The launch cursor row (0-based). Buffer row 0 paints here until the
    /// display scrolls; a replay resets it to 0 with the screen cleared.
    anchor: usize,
    /// Buffer rows scrolled off above the screen; never decreases.
    viewport_top: usize,
    /// Set by `resize`: the next paint replays from a cleared screen.
    replay_pending: bool,
    pub cols: u16,
    pub rows: u16,
    debug_frames: bool,
}

/// What one screen row of the next paint must do.
pub(crate) struct RowAction<'a> {
    /// The screen row (0-based) to rewrite.
    pub row: usize,
    /// The content to paint; None clears the row.
    pub write: Option<&'a str>,
}

/// Which screen rows need a rewrite for the frame to be visible. Buffer row
/// `b` lives at screen row `anchor + b - viewport_top`; rows above the
/// buffer's visible start hold pre-launch content and are left alone, rows
/// below the frame's end are cleared when they hold painted content.
pub(crate) fn plan<'a>(
    lines: &'a [String],
    height: usize,
    anchor: usize,
    viewport_top: usize,
    shadow: &[Option<String>],
) -> Vec<RowAction<'a>> {
    let len = lines.len();
    let mut actions = Vec::new();
    for row in 0..height {
        let b = row as i64 - anchor as i64 + viewport_top as i64;
        if b < 0 {
            // Above the buffer's visible start: pre-launch content, or rows
            // that scrolled away — never touched.
            continue;
        }
        let b = b as usize;
        let action = if b < len {
            let line = &lines[b];
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
                Some("") | None => None,
                Some(_) => Some(RowAction { row, write: None }),
            }
        };
        if let Some(a) = action {
            actions.push(a);
        }
    }
    actions
}

/// The first and last buffer rows that differ between two frames. Appends
/// extend the range to the new tail; deletions end at the old tail.
pub(crate) fn diff_range(prev: &[String], lines: &[String]) -> (usize, usize) {
    let mut first = usize::MAX;
    let mut last = 0;
    for i in 0..prev.len().max(lines.len()) {
        let old = prev.get(i).map(String::as_str).unwrap_or("");
        let new = lines.get(i).map(String::as_str).unwrap_or("");
        if old != new {
            first = first.min(i);
            last = last.max(i);
        }
    }
    (first, last)
}

/// How the next paint reaches its frame.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// Rewrite the changed rows at absolute positions, scrolling `scroll`
    /// display rows up first.
    Diff { scroll: usize },
    /// Print the changed range sequentially from where it starts; the
    /// terminal scrolls as the rows flow past the bottom, so the head lands
    /// in the scrollback in order.
    Flow,
    /// Clear the screen and scrollback, then print the whole frame from the
    /// screen top (a resize reflowed every row position).
    Replay,
}

/// Pick the route for a frame. `anchor` is the launch row; `viewport_top`
/// the rows already scrolled off. The new viewport top is monotonic: the
/// display only ever scrolls up, and a shrinking frame repaints in place.
pub(crate) fn route(
    first_changed: usize,
    len: usize,
    anchor: usize,
    viewport_top: usize,
    height: usize,
    replay_pending: bool,
) -> Route {
    if replay_pending {
        return Route::Replay;
    }
    let top = (anchor + len).saturating_sub(height).max(viewport_top);
    let pos_first = anchor as i64 + first_changed as i64 - top as i64;
    if pos_first < 0 && first_changed as i64 >= viewport_top as i64 - anchor as i64 {
        // The changed range starts between the old and new screen tops —
        // unreachable by absolute positioning, but painted before: repaint
        // it flowing, so nothing lands in the scrollback out of order.
        return Route::Flow;
    }
    Route::Diff {
        scroll: top - viewport_top,
    }
}

impl Screen {
    pub fn new(cols: u16, rows: u16, anchor: usize) -> Self {
        Screen {
            prev: Vec::new(),
            shadow: vec![None; rows as usize],
            anchor: anchor.min(rows.saturating_sub(1) as usize),
            viewport_top: 0,
            replay_pending: false,
            cols,
            rows,
            debug_frames: std::env::var("E_DEBUG_FRAMES").is_ok(),
        }
    }

    /// A resize reflows every row on screen — ours and the user's — so the
    /// shadow's positions are meaningless. The next paint clears the screen
    /// and scrollback and replays the whole frame from the top (the replayed
    /// heads would otherwise stack in the scrollback on every resize).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        self.prev.clear();
        self.shadow = vec![None; rows as usize];
        self.replay_pending = true;
    }

    pub fn paint(&mut self, lines: &[String]) -> io::Result<()> {
        if lines == self.prev.as_slice() {
            return Ok(());
        }
        if self.debug_frames {
            use std::io::Write as _;
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/e-frames.log");
            if let Ok(mut f) = f {
                let _ = writeln!(f, "== frame {} rows ==", lines.len());
                for l in lines {
                    let _ = writeln!(f, "{:?}", l);
                }
            }
        }
        let cols = self.cols as usize;
        let rows = self.rows as usize;
        let anchor = self.anchor;
        let (first_changed, _last_changed) = diff_range(&self.prev, lines);
        let len = lines.len();

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

        match route(
            first_changed,
            len,
            anchor,
            self.viewport_top,
            rows,
            self.replay_pending,
        ) {
            Route::Replay => {
                write!(out, "\x1b[2J\x1b[H\x1b[3J")?;
                self.anchor = 0;
                for (i, line) in lines.iter().enumerate() {
                    if i > 0 {
                        write!(out, "\r\n")?;
                    }
                    put(&mut out, line)?;
                }
                self.viewport_top = len.saturating_sub(rows);
                self.shadow = (0..rows)
                    .map(|r| {
                        let b = r + self.viewport_top;
                        if b < len {
                            Some(lines[b].clone())
                        } else {
                            Some(String::new())
                        }
                    })
                    .collect();
            }
            Route::Flow => {
                // Bring the first changed row to a paintable position: one
                // past the bottom means it scrolls in with a single newline;
                // otherwise it is already on screen where it was painted.
                let pos = (anchor + first_changed).saturating_sub(self.viewport_top);
                if pos >= rows {
                    // One past the bottom: the first changed row scrolls in
                    // with a single newline and paints at the bottom row.
                    write!(out, "\r\x1b[{rows};1H")?;
                    writeln!(out)?;
                } else {
                    write!(out, "\r\x1b[{};1H", pos + 1)?;
                }
                for (i, line) in lines[first_changed..len].iter().enumerate() {
                    if i > 0 {
                        write!(out, "\r\n")?;
                    }
                    put(&mut out, line)?;
                }
                let top = (anchor + len).saturating_sub(rows);
                let total = top - self.viewport_top;
                self.viewport_top = top;
                let drained = total.min(rows);
                self.shadow.drain(0..drained);
                self.shadow
                    .extend((0..drained).map(|_| Some(String::new())));
                // The flowed rows landed at their mapping positions; rows
                // the flow pushed into the scrollback leave the screen.
                for (offset, line) in lines[first_changed..len].iter().enumerate() {
                    let r = (anchor + first_changed + offset) as i64 - top as i64;
                    if r >= 0 && (r as usize) < rows {
                        self.shadow[r as usize] = Some(line.clone());
                    }
                }
            }
            Route::Diff { scroll } => {
                // The window moved down the frame: scroll the display up so
                // the top rows enter the terminal's scrollback and blank
                // rows appear at the bottom for the new content. The `\r`
                // homes the cursor without resolving a pending wrap (#123).
                if scroll > 0 {
                    write!(out, "\r\x1b[{rows};1H")?;
                    for _ in 0..scroll {
                        writeln!(out)?;
                    }
                    let drained = scroll.min(rows);
                    self.shadow.drain(0..drained);
                    self.shadow
                        .extend((0..drained).map(|_| Some(String::new())));
                }
                self.viewport_top += scroll;
                let actions = plan(lines, rows, anchor, self.viewport_top, &self.shadow);
                // Runs of adjacent dirty rows: position once, then `\r\n`
                // between rows. The `\r` before the absolute position is what
                // keeps the differ independent of the terminal's pending-wrap
                // state.
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
            }
        }
        self.replay_pending = false;
        write!(out, "\x1b[?2026l")?;
        out.flush()?;
        self.prev = lines.to_vec();
        Ok(())
    }
}

/// One sequenced frame in the paint thread's single-slot mailbox.
struct PendingFrame {
    sequence: u64,
    posted_at: std::time::Instant,
    lines: Vec<String>,
}

/// Paint progress as observed by the app loop. "Completed" means stdout
/// accepted and flushed the write; terminals do not acknowledge display.
#[derive(Clone, Debug)]
pub struct PaintStatus {
    pub posted: u64,
    pub completed: u64,
    pub pending_since: Option<std::time::Instant>,
    pub failure: Option<(u64, String)>,
    pub stopped: bool,
}

impl PaintStatus {
    pub fn delayed(&self, threshold: std::time::Duration) -> bool {
        self.pending_since
            .is_some_and(|since| since.elapsed() >= threshold)
    }
}

/// The paint thread's single-slot mailbox: a newer frame replaces the
/// undelivered one, so a terminal blocked mid-write bounds the backlog to
/// exactly one pending frame — an unbounded queue would grow by a full
/// transcript copy per tick for as long as the write stalls.
#[derive(Default)]
struct PaintMailbox {
    frame: Option<PendingFrame>,
    /// Latest wins here too; the resize replays, so painting the
    /// pre-resize frame once at the new size is a one-frame blip at most.
    resize: Option<(u16, u16)>,
    posted: u64,
    completed: u64,
    pending_since: Option<std::time::Instant>,
    failure: Option<(u64, String)>,
    stopped: bool,
    shutdown: bool,
}

impl PaintMailbox {
    fn status(&self) -> PaintStatus {
        PaintStatus {
            posted: self.posted,
            completed: self.completed,
            pending_since: self.pending_since,
            failure: self.failure.clone(),
            stopped: self.stopped,
        }
    }

    fn complete(&mut self, sequence: u64) {
        self.completed = self.completed.max(sequence);
        self.failure = None;
        self.pending_since = if self.completed >= self.posted {
            None
        } else {
            self.frame.as_ref().map(|frame| frame.posted_at)
        };
    }

    fn fail(&mut self, sequence: u64, error: String) {
        self.failure = Some((sequence, error));
        self.pending_since
            .get_or_insert_with(std::time::Instant::now);
    }
}

/// Marks an unexpected painter exit even when it unwinds outside `paint()`.
struct PaintThreadGuard {
    mailbox: std::sync::Arc<(std::sync::Mutex<PaintMailbox>, std::sync::Condvar)>,
}

impl Drop for PaintThreadGuard {
    fn drop(&mut self) {
        let (lock, wake) = &*self.mailbox;
        let mut mailbox = lock.lock().unwrap_or_else(|e| e.into_inner());
        mailbox.stopped = true;
        if !mailbox.shutdown && mailbox.failure.is_none() {
            let sequence = mailbox.posted;
            mailbox.fail(sequence, "paint worker stopped unexpectedly".into());
        }
        wake.notify_all();
    }
}

/// The paint thread: owns the `Screen` and its blocking stdout writes so a
/// slow terminal can never stall the event loop. `anchor` is the launch
/// cursor row — the frame paints below it, never over what came before.
pub struct Painter {
    mailbox: std::sync::Arc<(std::sync::Mutex<PaintMailbox>, std::sync::Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
    next_sequence: u64,
}

impl Painter {
    pub fn spawn(cols: u16, rows: u16, anchor: usize) -> Self {
        let mailbox = std::sync::Arc::new((
            std::sync::Mutex::new(PaintMailbox::default()),
            std::sync::Condvar::new(),
        ));
        let shared = mailbox.clone();
        let thread = std::thread::spawn(move || {
            let _guard = PaintThreadGuard {
                mailbox: shared.clone(),
            };
            let mut screen = Screen::new(cols, rows, anchor);
            let (lock, wake) = &*shared;
            loop {
                let (frame, resize, shutdown) = {
                    let mut box_ = lock.lock().unwrap_or_else(|e| e.into_inner());
                    while box_.frame.is_none() && box_.resize.is_none() && !box_.shutdown {
                        box_ = wake.wait(box_).unwrap_or_else(|e| e.into_inner());
                    }
                    (box_.frame.take(), box_.resize.take(), box_.shutdown)
                };
                if let Some((cols, rows)) = resize {
                    screen.resize(cols, rows);
                }
                // A panic in the paint path must cost one garbled frame —
                // not the session. The screen is marked unknown so the next
                // frame repaints everything over whatever the panicking
                // write left behind.
                if let Some(frame) = frame {
                    let sequence = frame.sequence;
                    let painted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        screen.paint(&frame.lines)
                    }));
                    let mut mailbox = lock.lock().unwrap_or_else(|e| e.into_inner());
                    match painted {
                        Ok(Ok(())) => mailbox.complete(sequence),
                        Ok(Err(error)) => {
                            screen.resize(screen.cols, screen.rows);
                            mailbox.fail(sequence, format!("terminal write failed: {error}"));
                        }
                        Err(_) => {
                            screen.resize(screen.cols, screen.rows);
                            mailbox.fail(sequence, "paint worker panicked".into());
                        }
                    }
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
            next_sequence: 0,
        }
    }

    fn post(&self, update: impl FnOnce(&mut PaintMailbox)) {
        let (lock, wake) = &*self.mailbox;
        update(&mut lock.lock().unwrap_or_else(|e| e.into_inner()));
        wake.notify_one();
    }

    pub fn frame(&mut self, lines: Vec<String>) {
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let sequence = self.next_sequence;
        let posted_at = std::time::Instant::now();
        self.post(|mailbox| {
            mailbox.posted = sequence;
            mailbox.pending_since.get_or_insert(posted_at);
            mailbox.frame = Some(PendingFrame {
                sequence,
                posted_at,
                lines,
            });
        });
    }

    pub fn status(&self) -> PaintStatus {
        let (lock, _) = &*self.mailbox;
        lock.lock().unwrap_or_else(|e| e.into_inner()).status()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn shadow(s: &[Option<&str>]) -> Vec<Option<String>> {
        s.iter().map(|o| o.map(str::to_string)).collect()
    }

    fn lines(n: usize, tag: &str) -> Vec<String> {
        (0..n).map(|i| format!("{tag}{i}")).collect()
    }

    #[test]
    fn a_short_frame_paints_from_the_anchor_down() {
        // Launch row 20 of a 30-row screen: buffer rows sit at 20, 21, 22;
        // rows above hold the user's pre-launch content and stay untouched.
        let ls = lines(3, "row");
        let actions = plan(&ls, 30, 20, 0, &shadow(&[None; 30]));
        assert_eq!(actions.len(), 3);
        assert_eq!(actions[0].row, 20);
        assert_eq!(actions[0].write, Some("row0"));
        assert_eq!(actions[2].row, 22);
    }

    #[test]
    fn the_frame_never_touches_the_rows_above_the_anchor() {
        let ls = lines(5, "row");
        let actions = plan(&ls, 10, 5, 0, &shadow(&[None; 10]));
        assert!(actions.iter().all(|a| a.row >= 5));
    }

    #[test]
    fn unchanged_rows_are_skipped_but_dirty_rows_rewrite() {
        let ls = lines(2, "row");
        // Buffer row 0 is at screen row 5 and already matches; row 1 at 6
        // holds stale content and rewrites.
        let mut sh = shadow(&[None; 10]);
        sh[5] = Some("row0".into());
        sh[6] = Some("stale".into());
        let actions = plan(&ls, 10, 5, 0, &sh);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].row, 6);
        assert_eq!(actions[0].write, Some("row1"));
    }

    #[test]
    fn scrolled_rows_map_linearly_across_the_screen() {
        // anchor 5, viewport_top 4: buffer row b sits at screen row b + 1;
        // the frame's head is above the visible start and not visited.
        let ls = lines(12, "row");
        let actions = plan(&ls, 10, 5, 4, &shadow(&[None; 10]));
        assert_eq!(actions.len(), 9);
        assert_eq!(actions[0].row, 1);
        assert_eq!(actions[0].write, Some("row0"));
        assert_eq!(actions[8].row, 9);
        assert_eq!(actions[8].write, Some("row8"));
    }

    #[test]
    fn rows_below_a_shrunk_frame_clear_only_when_painted() {
        let ls = lines(1, "row");
        // The frame's row paints; painted content below the frame end clears.
        let actions = plan(&ls, 10, 5, 0, &shadow(&[None; 10]).with_row(6, "stale"));
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].row, 5);
        assert_eq!(actions[0].write, Some("row0"));
        assert_eq!(actions[1].row, 6);
        assert_eq!(actions[1].write, None);
        // Already blank below: left alone.
        assert!(plan(&ls, 10, 5, 0, &shadow(&[None; 10]).with_row(6, "")).len() == 1);
    }

    #[test]
    fn the_viewport_tail_maps_across_the_whole_screen() {
        // anchor 8, viewport_top 20: buffer rows 12.. sit at screen rows
        // 0.. — every screen row is a buffer row, the frame's head long
        // since scrolled away.
        let ls = lines(30, "row");
        let actions = plan(&ls, 10, 8, 20, &shadow(&[None; 10]));
        assert_eq!(actions.len(), 10);
        assert_eq!(actions[0].row, 0);
        assert_eq!(actions[0].write, Some("row12"));
        assert_eq!(actions[9].write, Some("row21"));
    }

    trait WithRow {
        fn with_row(self, row: usize, content: &str) -> Self;
    }
    impl WithRow for Vec<Option<String>> {
        fn with_row(mut self, row: usize, content: &str) -> Self {
            self[row] = Some(content.to_string());
            self
        }
    }

    #[test]
    fn paint_progress_distinguishes_posted_completed_and_failed_frames() {
        let mut mailbox = PaintMailbox::default();
        let posted_at = std::time::Instant::now();
        mailbox.posted = 2;
        mailbox.pending_since = Some(posted_at);
        mailbox.frame = Some(PendingFrame {
            sequence: 2,
            posted_at,
            lines: vec!["new".into()],
        });

        mailbox.complete(1);
        let pending = mailbox.status();
        assert_eq!((pending.posted, pending.completed), (2, 1));
        assert_eq!(pending.pending_since, Some(posted_at));

        mailbox.fail(2, "broken pipe".into());
        assert_eq!(mailbox.status().failure.unwrap().0, 2);
        mailbox.complete(2);
        let recovered = mailbox.status();
        assert_eq!(recovered.completed, 2);
        assert!(recovered.pending_since.is_none());
        assert!(recovered.failure.is_none());
    }

    #[test]
    fn diff_range_covers_appends_deletions_and_edits() {
        let prev = lines(5, "a");
        // Pure append: the range starts at the old tail.
        assert_eq!(diff_range(&prev, &lines(8, "a")), (5, 7));
        // Pure deletion: the range ends at the old tail.
        assert_eq!(diff_range(&prev, &lines(2, "a")), (2, 4));
        // An edit that replaces one row: the range is that row alone.
        let mut next = lines(5, "a");
        next[1] = "edited".into();
        assert_eq!(diff_range(&prev, &next), (1, 1));
        // An insertion shifts every row below it.
        let mut next = lines(5, "a");
        next.insert(1, "inserted".into());
        assert_eq!(diff_range(&prev, &next), (1, 5));
    }

    #[test]
    fn a_fitting_first_frame_diffs_from_the_anchor() {
        // First frame, fits below the anchor: plain diff, no scroll.
        assert!(matches!(
            route(0, 5, 20, 0, 30, false),
            Route::Diff { scroll: 0 }
        ));
    }

    #[test]
    fn an_overflowing_first_frame_flows_below_the_cursor() {
        // First frame of a restored session: print from the anchor and let
        // the terminal scroll — the pre-launch content above stays put.
        assert!(matches!(route(0, 100, 20, 0, 30, false), Route::Flow));
    }

    #[test]
    fn a_small_append_diffs_without_scrolling_past_the_screen() {
        // anchor 5, viewport_top 4, three more rows: top 8, scroll 4, and
        // the changed range [9..12) lands at screen rows 6..9 — in reach.
        assert_eq!(route(9, 13, 5, 4, 10, false), Route::Diff { scroll: 4 });
    }

    #[test]
    fn a_huge_append_flows_instead_of_skipping_rows() {
        // Appending 700 rows would put the first new row 647 rows above the
        // new screen top — the diff route would never paint them.
        assert!(matches!(route(10, 710, 0, 0, 10, false), Route::Flow));
    }

    #[test]
    fn a_change_above_the_old_screen_top_replays_only_on_resize() {
        // A compaction shrink above the viewport patches in place; the
        // pre-launch content above survives.
        assert!(matches!(
            route(2, 4, 0, 20, 10, false),
            Route::Diff { scroll: 0 }
        ));
        // A resize reflowed every position: replay from a cleared screen.
        assert!(matches!(route(2, 4, 0, 20, 10, true), Route::Replay));
    }

    #[test]
    fn the_viewport_never_scrolls_back_for_a_shrinking_frame() {
        assert_eq!(route(9, 11, 0, 4, 10, false), Route::Diff { scroll: 0 });
    }

    #[test]
    fn the_route_is_stable_when_nothing_changed() {
        // An unchanged frame takes the caller's fast path; route() is only
        // consulted when the diff range is non-empty, but a same-length
        // same-tail edit still diffs in place.
        assert_eq!(route(3, 10, 0, 4, 10, false), Route::Diff { scroll: 0 });
    }
}
