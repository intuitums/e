//! The ctrl+o review screen: the whole transcript with tool details spliced
//! in, at one of the reference's two depths — Review folds each detail to
//! three lines behind a `→ to expand` hint, Full shows all. Reading position
//! follows new output only while it remains at the tail.
use super::*;
use crate::tui::markdown::clip_styled;

/// The reference's navigation wording per depth; `transcript_hint` in
/// `~/.e/settings.json` replaces it outright when set.
const REVIEW_HINT: &str = "Review · ←/→ switch · ctrl o close · PgUp/PgDn scroll · Esc close";
const FULL_HINT: &str = "Full detail · ←/→ switch · ctrl o close · PgUp/PgDn scroll · Esc close";

#[derive(Clone)]
pub(super) struct Viewer {
    /// Review folds spliced details; Full expands them.
    pub full: bool,
    pub scroll: usize,
    pub follow_tail: bool,
    pub(super) hint: String,
}

impl Viewer {
    pub fn new() -> Self {
        Self {
            full: false,
            scroll: 0,
            follow_tail: true,
            hint: crate::core::config::settings::get_string("transcript_hint").unwrap_or_default(),
        }
    }

    /// The navigation line for the current depth: the user's override when
    /// set, the reference's wording otherwise.
    fn hint_for(&self, depth: bool) -> &str {
        if self.hint.is_empty() {
            if depth {
                FULL_HINT
            } else {
                REVIEW_HINT
            }
        } else {
            &self.hint
        }
    }
}

impl App {
    /// Rebuild the projection only when transcript, outputs, width, or depth change.
    pub(super) fn viewer_rows(&mut self, width: usize, full: bool) -> &[String] {
        let fingerprint = self.viewer_fingerprint();
        let current = self
            .viewer_cache
            .as_ref()
            .is_some_and(|(fp, cols, depth, _)| {
                *fp == fingerprint && *cols == width && *depth == full
            });
        if !current {
            let rows = self.project_rows(width, full);
            self.viewer_cache = Some((fingerprint, width, full, rows));
        }
        self.viewer_cache
            .as_ref()
            .map(|(_, _, _, rows)| rows.as_slice())
            .unwrap_or(&[])
    }

    /// Every state input used by the review projection.
    fn viewer_fingerprint(&self) -> u64 {
        self.transcript.fingerprint() ^ self.output_seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    /// The projection itself, pure over the current transcript: the whole
    /// transcript with each child row's stored detail railed beneath it —
    /// folded to three lines behind the reference's `→ to expand` hint at
    /// Review depth, whole at Full. Non-tool blocks render through the
    /// per-block cache, so a rebuild pays only for blocks that changed.
    fn project_rows(&mut self, width: usize, full: bool) -> Vec<String> {
        const REVIEW_DETAIL_LINES: usize = 3;
        let mut rows: Vec<String> = Vec::new();
        for block in &mut self.transcript.blocks {
            let lines = block.review_lines(&self.theme, width);
            if lines.is_empty() {
                continue;
            }
            if !rows.is_empty() {
                rows.push(String::new());
            }
            let group_start = rows.len();
            for (row, detail) in lines {
                rows.push(clip_styled(&row, width));
                use crate::tui::transcript::ToolDetail;
                let Some(detail) = detail else { continue };
                if let ToolDetail::Live(index) = detail {
                    if block
                        .tool_children
                        .get(index)
                        .is_some_and(|child| child.output_truncated)
                    {
                        rows.extend(crate::tui::transcript::tree_rows(
                            &self.theme,
                            width,
                            "│",
                            &self.theme.fg("dim", "Earlier live output omitted."),
                        ));
                    }
                }
                let body = match detail {
                    ToolDetail::Stored(id) => Self::output_body(&self.outputs, id),
                    ToolDetail::Live(index) => block
                        .tool_children
                        .get(index)
                        .map(|child| child.output.as_str()),
                };
                let Some(body) = body else {
                    rows.push(self.theme.fg("dim", "│  Full saved result unavailable."));
                    continue;
                };
                let body_rows: Vec<String> = body
                    .lines()
                    .flat_map(|line| {
                        let colored = Self::diff_row_color(&self.theme, line)
                            .unwrap_or_else(|| self.theme.fg("dim", line));
                        crate::tui::transcript::tree_rows(&self.theme, width, "│", &colored)
                    })
                    .collect();
                let hidden = if full {
                    0
                } else {
                    body_rows.len().saturating_sub(REVIEW_DETAIL_LINES)
                };
                let shown = body_rows.len() - hidden;
                if matches!(detail, ToolDetail::Live(_)) {
                    rows.extend(body_rows.into_iter().skip(hidden));
                } else {
                    rows.extend(body_rows.into_iter().take(shown));
                }
                if hidden > 0 {
                    rows.extend(crate::tui::transcript::tree_rows(
                        &self.theme,
                        width,
                        "│",
                        &self
                            .theme
                            .fg("dim", &format!("{hidden} more rows · → to expand")),
                    ));
                }
            }
            // Close after the final argument, output, or omission row. Closing
            // the action first would leave its inserted output disconnected.
            if block.kind == Kind::ToolGroup && width >= 3 && rows.len() > group_start + 1 {
                if let Some(last) = rows.last_mut() {
                    *last = last.replacen(['├', '│'], "└", 1);
                }
            }
        }
        rows
    }

    /// Show the latest output on open, with the reference's navigation, gap,
    /// and model row. The body can shrink under a deep scroll (the output
    /// store evicts; details disappear) — clamp so the screen never renders
    /// blank.
    pub(super) fn viewer_frame(&mut self, width: usize, height: usize) -> Vec<String> {
        let Some(viewer) = self.viewer.as_ref() else {
            return Vec::new();
        };
        let (offset, follow_tail, full) = (viewer.scroll, viewer.follow_tail, viewer.full);
        let hint = viewer.hint_for(full).to_string();
        let window = height.saturating_sub(3);
        let body = self.viewer_rows(width, full);
        let end = body.len().saturating_sub(window);
        let scroll = if follow_tail { end } else { offset.min(end) };
        let mut rows: Vec<String> = body.iter().skip(scroll).take(window).cloned().collect();
        rows.resize(window, String::new());
        rows.push(navigation_row(&self.theme, &hint, width));
        let (left, right) = self.status_segments();
        rows.extend(statusline(
            &self.theme,
            &left,
            self.overlay.as_deref().or(right.as_deref()),
            None,
            false,
            width,
        ));
        rows.truncate(height);
        if let Some(viewer) = self.viewer.as_mut() {
            viewer.scroll = scroll;
            viewer.follow_tail = scroll == end;
        }
        rows.into_iter()
            .map(|row| clip_styled(&row, width))
            .collect()
    }

    /// Clamp key and mouse scrolling to a full page, resuming follow at the bottom.
    pub(super) fn scroll_viewer(&mut self, down: bool, step: usize, width: usize, height: usize) {
        let full = self.viewer.as_ref().is_some_and(|viewer| viewer.full);
        let end = self
            .viewer_rows(width, full)
            .len()
            .saturating_sub(height.saturating_sub(3));
        if let Some(viewer) = self.viewer.as_mut() {
            let offset = if viewer.follow_tail {
                end
            } else {
                viewer.scroll.min(end)
            };
            viewer.scroll = if down {
                offset.saturating_add(step).min(end)
            } else {
                offset.saturating_sub(step)
            };
            viewer.follow_tail = viewer.scroll == end;
        }
    }

    /// The reader consumes navigation, leaving global cancellation to the app.
    pub(super) fn viewer_key(&mut self, key: KeyEvent, width: usize, height: usize) -> bool {
        if self.viewer.is_none() {
            return false;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Esc || (ctrl && matches!(key.code, KeyCode::Char('o' | 'c'))) {
            self.viewer = None;
            return !(ctrl && key.code == KeyCode::Char('c'));
        }
        match key.code {
            KeyCode::Up => self.scroll_viewer(false, 1, width, height),
            KeyCode::Down => self.scroll_viewer(true, 1, width, height),
            KeyCode::PageUp => {
                self.scroll_viewer(false, height.saturating_sub(3).max(1), width, height)
            }
            KeyCode::PageDown => {
                self.scroll_viewer(true, height.saturating_sub(3).max(1), width, height)
            }
            KeyCode::Home => self.scroll_viewer(false, usize::MAX, width, height),
            KeyCode::End => self.scroll_viewer(true, usize::MAX, width, height),
            // ←/→ switch between the reference's two depths: Review folds
            // details, Full expands everything.
            KeyCode::Left => {
                if let Some(viewer) = self.viewer.as_mut() {
                    viewer.full = false;
                }
            }
            KeyCode::Right => {
                if let Some(viewer) = self.viewer.as_mut() {
                    viewer.full = true;
                }
            }
            _ => {}
        }
        true
    }
}

/// The reference footer ellipsizes its hint instead of wrapping into the status row.
fn navigation_row(theme: &Theme, hint: &str, width: usize) -> String {
    let hint = crate::core::tools::sanitize_display(hint).replace('\n', " ");
    let available = width.saturating_sub(2);
    let hint = crate::tui::transcript::clip_plain(&hint, available);
    clip_styled(
        &format!(
            "{} {}",
            theme.fg("userMessageText", "┃"),
            theme.fg("muted", &hint)
        ),
        width,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_detail_navigation_matches_fx_and_ellipsizes_in_narrow_frames() {
        let theme = crate::tui::theme::load_bundled(false).unwrap();
        let hint = "full detail · ctrl+o close · pgup/pgdn scroll · esc close";
        assert_eq!(
            navigation_row(&theme, hint, 80),
            format!(
                "{} {}",
                theme.fg("userMessageText", "┃"),
                theme.fg("muted", hint)
            )
        );
        assert_eq!(
            navigation_row(&theme, hint, 16),
            format!(
                "{} {}",
                theme.fg("userMessageText", "┃"),
                theme.fg("muted", "full detail ·…")
            )
        );
    }

    /// The footer follows the reference's wording per depth; a user's
    /// `transcript_hint` replaces it outright at both depths.
    #[test]
    fn navigation_defaults_follow_depth_and_the_hint_setting_overrides() {
        let viewer = Viewer::new();
        assert_eq!(viewer.hint_for(false), REVIEW_HINT);
        assert_eq!(viewer.hint_for(true), FULL_HINT);
        let overridden = Viewer {
            hint: "custom".into(),
            ..Viewer::new()
        };
        assert_eq!(overridden.hint_for(false), "custom");
        assert_eq!(overridden.hint_for(true), "custom");
    }
}
