//! TUI hardening pins: control-sequence injection is inert, width math is
//! display columns, overlong tokens wrap instead of vanishing, and the
//! composer draws exactly one cursor.

use e::tui::markdown::{clip_styled, visible_width, wrap_styled};
use e::tui::theme;
use e::tui::transcript::{Block, Kind};

fn dark() -> theme::Theme {
    theme::load_bundled(false).unwrap()
}

/// Model output carrying terminal controls (OSC 52 clipboard writes, screen
/// clears) must render as inert text, never reach the paint stream.
#[test]
fn untrusted_block_text_cannot_inject_terminal_controls() {
    let theme = dark();
    let payload = "hello \x1b]52;c;YXR0YWNr\x07 and \x1b[2J\x1b[H there";
    for kind in [Kind::Assistant, Kind::Notice, Kind::User] {
        let block = Block::new(kind, payload);
        for line in block.lines_for_test(&theme, 80) {
            // The remnant text may survive; an executable sequence may not.
            assert!(
                !line.contains("\x1b]"),
                "an OSC sequence leaked into a rendered line: {line:?}"
            );
            assert!(
                !line.contains("\x1b[2J"),
                "clear-screen leaked into a rendered line: {line:?}"
            );
        }
    }
}

/// clip_styled counts display columns: three CJK characters are six columns,
/// so a four-column clip keeps at most two of them.
#[test]
fn clip_styled_counts_display_columns() {
    let clipped = clip_styled("界界界", 4);
    assert!(
        visible_width(&clipped) <= 4,
        "clip left {} visible columns",
        visible_width(&clipped)
    );
}

/// An OSC 8 hyperlink passes through clipping intact — cutting it mid-URL
/// would spill the rest of the URL as visible text.
#[test]
fn clip_styled_preserves_osc8_hyperlinks() {
    let linked = "\x1b]8;;https://example.com\x1b\\verylongword\x1b]8;;\x1b\\tail";
    let clipped = clip_styled(linked, 6);
    assert!(visible_width(&clipped) <= 6);
    // The opening sequence survives whole — cut mid-URL, its remainder
    // would spill out as visible text — and the clip closes the link so it
    // cannot bleed into the next row.
    assert!(
        clipped.contains("\x1b]8;;https://example.com\x1b\\"),
        "hyperlink sequence was cut: {clipped:?}"
    );
    assert!(
        clipped.contains("\x1b]8;;\x1b\\"),
        "link left open: {clipped:?}"
    );
}

/// A single token wider than the line hard-wraps across rows; a clipped
/// suffix would be permanently invisible.
#[test]
fn wrap_styled_hard_wraps_overlong_tokens() {
    let rows = wrap_styled("abcdefghijklmnopqrstuvwxyz", 10);
    assert_eq!(rows, vec!["abcdefghij", "klmnopqrst", "uvwxyz"]);
    // Normal text is untouched.
    assert_eq!(wrap_styled("two words", 10), vec!["two words"]);
}

/// The composer lays out by display width, and a wrap-boundary cursor
/// renders in exactly one row.
#[test]
fn composer_uses_display_width_and_one_cursor() {
    use e::tui::composer::Editor;
    let theme = dark();

    // Inner width is 8; eight CJK chars are 16 columns → at least two rows.
    let mut editor = Editor::new();
    editor.set_text("界界界界界界界界");
    let rows = editor.render(&theme, 10);
    let content_rows = rows.len() - 1; // leading blank
    assert!(
        content_rows >= 2,
        "wide text must wrap by columns, got {content_rows} row(s)"
    );

    // A cursor at a wrap boundary paints exactly one reverse-video cell.
    let mut editor = Editor::new();
    editor.set_text("abcdefghijklmnop");
    let rows = editor.render(&theme, 10);
    let cursors: usize = rows.iter().map(|r| r.matches("\x1b[7m").count()).sum();
    assert_eq!(
        cursors, 1,
        "exactly one cursor cell, got {cursors}: {rows:?}"
    );
}

/// Submitting a draft retires its paste placeholders: a stale token typed
/// later must not re-expand an old payload.
#[test]
fn paste_placeholders_retire_on_submit() {
    use e::tui::composer::Editor;
    let mut editor = Editor::new();
    editor.insert_paste("line one\nline two\nline three");
    let draft = editor.text();
    assert!(draft.contains("[Pasted text #1"));
    let expanded = editor.expand_pastes(&draft);
    assert!(expanded.contains("line two"));
    // The mapping is gone: the same token now passes through literally.
    let again = editor.expand_pastes(&draft);
    assert!(
        again.contains("[Pasted text #1"),
        "stale payload re-expanded"
    );
}
