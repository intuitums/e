//! The composer's wrapping contract: a draft longer than the width wraps to
//! more rail rows (the reference shape — never a horizontally scrolled single
//! row), the rail heads every visual row, and the cursor stays visible on the
//! row it logically occupies.

use e::tui::composer::Editor;
use e::tui::markdown::visible_width;
use e::tui::theme;

fn rows(text: &str, width: usize) -> Vec<String> {
    let mut editor = Editor::new();
    editor.set_text(text);
    editor.render(&theme::resolve("dark", false), width)
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
