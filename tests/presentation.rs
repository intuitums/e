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
    assert_eq!(plain, ["├ Ran", "│ abcdefghij", "│ klmnopqrst", "│ uv"]);
    assert!(rows[1..rows.len() - 1]
        .iter()
        .all(|(_, detail)| detail.is_none()));
    assert_eq!(rows.last().unwrap().1, Some(ToolDetail::Stored(42)));
}

/// A completed single call stays connected through wrapping and closes below it.
#[test]
fn wrapped_single_tool_keeps_its_review_footer_after_completion() {
    let theme = load_bundled(false).unwrap();
    let mut group = Block::tool_group(vec![command(1, "abcdefghijklmnopqrstuv")]);
    group.start_tool(1);
    for completed in [false, true] {
        if completed {
            group.finish_tool(1, ToolOutcome::Completed, "done".into(), "output");
        }
        let rows = group.lines_for_test(&theme, 20);
        let plain: Vec<_> = rows.iter().skip(1).map(|row| strip_ansi(row)).collect();
        assert_eq!(
            plain,
            [
                if completed { "├ Ran" } else { "├ Running" },
                "│ abcdefghijklmnopq…",
                "└ ctrl+o to view",
            ]
        );
    }
    // Even when the footer itself wraps, no rail hangs below the closing elbow.
    let rows = group.lines_for_test(&theme, 12);
    let plain: Vec<_> = rows.iter().map(|row| strip_ansi(row)).collect();
    assert!(plain.last().unwrap().starts_with("└ "));
    assert_eq!(plain.iter().filter(|row| row.starts_with('└')).count(), 1);
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
    assert_eq!(plain.iter().filter(|s| s.starts_with('├')).count(), 3);
    assert_eq!(&plain[plain.len() - 2..], ["├ ctrl+o", "└ to view"]);
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

/// Shell offsets and hanging wrap whitespace must use the same cursor indices.
#[test]
fn shell_cursor_crosses_hanging_whitespace_without_losing_the_prefix() {
    let theme = load_bundled(false).unwrap();
    let mut editor = Editor::new();
    editor.set_text("!abcdef  gh");
    editor.render(&theme, 8, 8);
    editor.key(Key::Home);
    for _ in 0..8 {
        editor.key(Key::Right);
    }
    assert_eq!(editor.cursor(), 8);
    assert!(strip_ansi(&editor.render(&theme, 8, 8)[1]).starts_with("! abcdef"));
    editor.key(Key::Down);
    assert_eq!(editor.cursor(), 11);
    assert_eq!(editor.text(), "!abcdef  gh");
}

/// The optional prefix space remains a real editable cell in the shell gutter.
#[test]
fn shell_prefix_space_has_its_own_cursor_and_selection_cell() {
    let theme = load_bundled(false).unwrap();
    let mut editor = Editor::new();
    editor.set_text("! echo");
    editor.key(Key::Home);
    editor.key(Key::Right);
    assert_eq!(
        editor.render(&theme, 80, 8)[1],
        format!("{}\x1b[7m \x1b[27mecho", theme.fg("bashMode", "!"))
    );
    editor.key(Key::SelectRight);
    assert_eq!(
        editor.render(&theme, 80, 8)[1],
        format!("{}\x1b[7m \x1b[27mecho", theme.fg("bashMode", "!"))
    );
    assert_eq!(editor.text(), "! echo");
}

/// Width changes alter only the preview; Unicode labels and full review survive.
#[test]
fn tool_labels_reflow_from_source_with_a_display_row_budget() {
    let theme = load_bundled(false).unwrap();
    let target = format!("{} {} TAIL_MARKER", "界".repeat(24), "e\u{301}".repeat(12));
    let mut group = Block::tool_group(vec![command(1, &target)]);
    group.start_tool(1);
    group.finish_tool(1, ToolOutcome::Completed, "done".into(), "");
    for width in [20, 160, 20] {
        let rows = group.lines_for_test(&theme, width);
        let plain: Vec<_> = rows.iter().map(|row| strip_ansi(row)).collect();
        assert!(plain.len() <= 4, "{plain:?}");
        assert!(rows
            .iter()
            .all(|row| e::tui::markdown::visible_width(row) <= width));
        assert_eq!(plain.last().unwrap(), "└ ctrl+o to view");
        if width == 20 {
            assert!(plain[2].ends_with('…'), "{plain:?}");
            assert!(!plain.join("\n").contains("TAIL_MARKER"));
        } else {
            assert_eq!(plain[1], format!("├ Ran {target}"));
        }
    }
    group.tool_label_rows = 1;
    assert_eq!(group.lines_for_test(&theme, 20).len(), 3);
    assert_eq!(group.tool_children[0].target, target);
    assert!(group
        .review_lines(&theme, 20)
        .iter()
        .any(|(row, _)| row.contains("TAIL_MARKER")));
}

/// Shell presentation retains newlines; only real heredoc bodies leave the preview.
#[test]
fn heredoc_bodies_stay_in_review_not_in_the_transcript() {
    let theme = load_bundled(false).unwrap();
    for (source, hidden) in [
        ("python3 - <<'PY'\nBODY_MARKER\nPY", true),
        ("cat <<-EOF\nBODY_MARKER\nEOF", true),
        ("echo '<<EOF'\necho BODY_MARKER", false),
        ("cat <<< hello\necho BODY_MARKER", false),
        ("echo $((1 << 2))\necho BODY_MARKER", false),
        ("echo ok # <<EOF\necho BODY_MARKER", false),
        ("((value = (1 << 2)))\necho BODY_MARKER", false),
        ("echo $((1 << 2)); cat <<EOF\nBODY_MARKER\nEOF", true),
        ("echo ok # <<FAKE\ncat <<EOF\nBODY_MARKER\nEOF", true),
        ("cat <<EOF # don't parse this quote\nBODY_MARKER\nEOF", true),
        ("echo word#suffix; cat <<EOF\nBODY_MARKER\nEOF", true),
        ("echo \\<\\<EOF\necho BODY_MARKER", false),
    ] {
        let presentation = e::core::tools::present("bash", &serde_json::json!({"command": source}));
        assert_eq!(presentation.target, source);
        let mut group = Block::tool_group(vec![command(1, &presentation.target)]);
        group.start_tool(1);
        group.finish_tool(1, ToolOutcome::Completed, "done".into(), "");
        let rows = group.lines_for_test(&theme, 200).join("\n");
        assert_eq!(!rows.contains("BODY_MARKER"), hidden, "{rows}");
        if hidden {
            assert!(rows.contains('…'));
        }
        assert!(group
            .review_lines(&theme, 200)
            .iter()
            .any(|(row, _)| row.contains("BODY_MARKER")));
    }
}

/// A truncated path cannot consume the edit's colored change counts.
#[test]
fn tool_label_budget_preserves_edit_counts() {
    let theme = load_bundled(false).unwrap();
    let mut group = Block::tool_group(vec![ToolChild::pending(
        1,
        "edit".into(),
        "Editing".into(),
        "Edited".into(),
        "long/path/".repeat(20),
    )]);
    group.start_tool(1);
    group.finish_tool(1, ToolOutcome::Completed, "+2 -1".into(), "");
    let rows = group.lines_for_test(&theme, 24);
    assert!(strip_ansi(&rows[2]).ends_with('…'));
    assert_eq!(strip_ansi(&rows[3]), "│ +2 / -1");
    assert!(rows[3].contains(&theme.fg(e::tui::theme::Theme::diff_marker_token(true), "+2")));
}
