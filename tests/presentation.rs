//! Wrapping and shell-gutter contracts for the connected tool presentation.

use e::core::tools::{strip_ansi, ToolOutcome};
use e::tui::composer::{Editor, EditorResult, Key};
use e::tui::theme::load_bundled;
use e::tui::transcript::{Block, ToolChild, ToolDetail};

/// Make one command without involving execution or a provider.
fn command(id: u64, target: &str) -> ToolChild {
    ToolChild::pending(
        id,
        "command".into(),
        "Running".into(),
        "Ran".into(),
        target.into(),
    )
}

#[test]
fn shell_prefix_moves_into_the_gutter_without_changing_submission() {
    let theme = load_bundled(false).unwrap();
    let mut editor = Editor::new();
    editor.set_text("! cargo test");
    let rows = editor.render(&theme, 80, 8);
    assert_eq!(
        rows[1],
        format!("{} cargo test\x1b[7m \x1b[27m", theme.fg("bashMode", "!"))
    );
    assert_eq!(editor.cursor(), 12);
    assert!(matches!(editor.key(Key::Enter), EditorResult::Submit(text) if text == "! cargo test"));

    editor.set_text("!");
    assert_eq!(strip_ansi(&editor.render(&theme, 80, 8)[1]), "!  ");
    editor.key(Key::Backspace);
    assert_eq!(
        editor.render(&theme, 80, 8)[1],
        format!("{} \x1b[7m \x1b[27m", theme.fg("userMessageText", "┃"))
    );
    editor.set_text(" ! cargo test");
    assert!(!editor.render(&theme, 80, 8)[1].contains(theme.fg_prefix("bashMode")));
}

#[test]
fn shell_wrapping_and_vertical_motion_share_raw_indices() {
    let theme = load_bundled(false).unwrap();
    let mut editor = Editor::new();
    editor.set_text("!abcdefghij");
    let rows = editor.render(&theme, 8, 8);
    assert_eq!(strip_ansi(&rows[1]), "! abcdef");
    assert_eq!(strip_ansi(&rows[2]), "┃ ghij ");
    assert!(!rows[2].contains(theme.fg_prefix("bashMode")));
    editor.key(Key::Up);
    assert_eq!(editor.cursor(), 5);
    editor.key(Key::Down);
    assert_eq!(editor.cursor(), 11);
    assert_eq!(editor.text(), "!abcdefghij");
}

#[test]
fn wrapped_calls_use_one_branch_and_attach_details_after_the_last_row() {
    let theme = load_bundled(false).unwrap();
    let target = "abcdefghijklmnopqrstuv";
    let mut group = Block::tool_group(vec![command(1, target)]);
    group.start_tool(1);
    group.finish_tool(1, ToolOutcome::Completed, "done".into(), "output");
    group.tool_children[0].detail = Some(42);
    let rows = group.review_lines(&theme, 12);
    let plain: Vec<_> = rows
        .iter()
        .skip(1)
        .map(|(row, _)| strip_ansi(row))
        .collect();
    assert_eq!(plain, ["└ Ran", "│ abcdefghij", "│ klmnopqrst", "│ uv"]);
    assert!(rows[1..rows.len() - 1]
        .iter()
        .all(|(_, detail)| detail.is_none()));
    assert_eq!(rows.last().unwrap().1, Some(ToolDetail::Stored(42)));
}

#[test]
fn concurrent_commands_keep_order_and_preview_the_wrapped_tail() {
    let theme = load_bundled(false).unwrap();
    let mut group = Block::tool_group(vec![command(1, "first"), command(2, "second")]);
    group.live_preview_rows = 2;
    group.start_tool(2);
    group.start_tool(1);
    group.append_tool_output(1, "old\nabcdefghijklmnopqrst\nLATEST\n");
    let rows = group.lines_for_test(&theme, 12);
    let plain: Vec<_> = rows.iter().map(|row| strip_ansi(row)).collect();
    let first = plain.iter().position(|s| s.contains("first")).unwrap();
    let second = plain.iter().position(|s| s.contains("second")).unwrap();
    assert!(first < second);
    assert!(!plain
        .iter()
        .any(|s| s.contains("old") || s.contains("abcdefghij")));
    assert!(plain.iter().any(|s| s == "│ klmnopqrst"));
    assert!(plain.iter().any(|s| s == "│ LATEST"));
    assert_eq!(plain.iter().filter(|s| s.starts_with('├')).count(), 2);
    assert_eq!(plain.iter().filter(|s| s.starts_with('└')).count(), 1);
    assert!(rows
        .iter()
        .all(|row| e::tui::markdown::visible_width(row) <= 12));
    let review = group.review_lines(&theme, 12);
    assert!(review
        .iter()
        .any(|(_, detail)| *detail == Some(ToolDetail::Live(0))));
}

#[test]
fn live_output_keeps_new_bytes_after_reaching_its_memory_cap() {
    let theme = load_bundled(false).unwrap();
    let mut group = Block::tool_group(vec![command(1, "long job")]);
    group.start_tool(1);
    group.append_tool_output(1, &"界".repeat(30_000));
    group.append_tool_output(1, "\nNEWEST\n");
    assert!(group.tool_children[0].output.len() <= 64 * 1024);
    let rows = group.lines_for_test(&theme, 80).join("\n");
    assert!(rows.contains("NEWEST"));
    assert!(rows.contains("earlier output omitted"));
}
