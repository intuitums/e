//! The composer's wrapping contract: a draft longer than the width wraps to
//! more rail rows (the reference shape — never a horizontally scrolled single
//! row), the rail heads every visual row, and the cursor stays visible on the
//! row it logically occupies.

use e::tui::composer::{Editor, Key};
use e::tui::markdown::visible_width;
use e::tui::theme;

fn rows(text: &str, width: usize) -> Vec<String> {
    let mut editor = Editor::new();
    editor.set_text(text);
    editor.render(&theme::resolve("dark", false), width, 24)
}

#[test]
fn long_draft_wraps_with_a_rail_on_every_row() {
    let text = "a".repeat(50);
    let rendered = rows(&text, 22); // inner width 20
                                    // Leading blank row, then ceil(50/20) = 3 rail rows (cursor sits in the last).
    assert_eq!(rendered.len(), 1 + 3);
    for row in &rendered[1..] {
        assert!(row.contains("┃"), "rail missing on a wrapped row");
        assert!(visible_width(row) <= 22, "row exceeds the terminal width");
    }
    // All 50 chars survive across the rows — nothing scrolled away.
    let total_a: usize = rendered.iter().map(|r| r.matches('a').count()).sum();
    assert_eq!(total_a, 50);
}

#[test]
fn cursor_lands_on_its_visual_row() {
    // 50 chars, cursor at the end: the reverse-video cell is on the last row.
    let rendered = rows(&"a".repeat(50), 22);
    assert!(rendered.last().unwrap().contains("\x1b[7m"));
    assert!(!rendered[1].contains("\x1b[7m"));
}

#[test]
fn exact_multiple_gets_a_row_for_the_cursor() {
    // 40 chars at inner width 20: two full rows, and the cursor needs a third.
    let rendered = rows(&"b".repeat(40), 22);
    assert_eq!(rendered.len(), 1 + 3);
    assert!(rendered.last().unwrap().contains("\x1b[7m"));
}

#[test]
fn newlines_still_break_rows() {
    let rendered = rows("one\ntwo", 40);
    assert_eq!(rendered.len(), 1 + 2);
    assert!(rendered[1].contains("one"));
    assert!(rendered[2].contains("two"));
}

#[test]
fn words_wrap_whole() {
    // "hello world extra" at inner width 8: each word lands whole on its
    // own row — no letter-level tearing.
    let rendered = rows("hello world extra", 10);
    assert!(rendered[1].contains("hello"));
    assert!(!rendered[1].contains("world"));
    assert!(rendered[2].contains("world"));
    assert!(!rendered[2].contains("extra"));
    assert!(rendered[3].contains("extra"));
}

#[test]
fn up_down_move_between_visual_rows_and_fall_back_to_history() {
    let theme = theme::resolve("dark", false);

    // Wrapped single line: Up leaves the draft alone and moves the cursor
    // into the first visual row.
    let mut editor = Editor::new();
    editor.set_text("hello world extra");
    editor.render(&theme, 10, 24); // establishes the layout width
    let end = editor.cursor();
    editor.key(Key::Up);
    assert!(editor.cursor() < end, "cursor moved up a visual row");
    assert_eq!(editor.text(), "hello world extra");

    // Single-row drafts fall through to history at the top edge.
    let mut editor = Editor::new();
    editor.set_text("draft");
    editor.push_history("older".into());
    editor.render(&theme, 40, 24);
    editor.key(Key::Up);
    assert_eq!(editor.text(), "older");
}

#[test]
fn paste_replaces_selection_and_clears_it() {
    let mut editor = Editor::new();
    editor.set_text("abcd");
    editor.key(Key::SelectLeft);
    editor.key(Key::SelectLeft);

    editor.insert_paste("X");

    assert_eq!(editor.text(), "abX");
    assert_eq!(editor.cursor(), 3);
    editor.key(Key::Left);
    assert_eq!(editor.cursor(), 2, "paste must not leave a stale selection");
}
