//! The conformance suite: e's terminal rendering, pinned byte-for-byte so
//! the look cannot drift. Each assertion encodes a deliberate choice of the
//! design (heading emphasis per level, divider tone, selection rules); an
//! intentional rendering change updates the pinned literal and says why.

use e::core::output::{compact_model_label, format_duration, format_tokens};
use e::tui::render::heading_style;
use e::tui::theme::Theme;

#[test]
fn heading_styles_match_the_level_table() {
    assert_eq!(
        heading_style(1, "Workspace overview"),
        "\x1b[1m\x1b[4mWorkspace overview\x1b[24m\x1b[22m"
    );
    assert_eq!(
        heading_style(2, "Installation"),
        "\x1b[1mInstallation\x1b[22m"
    );
    assert_eq!(heading_style(3, "macOS"), "\x1b[4mmacOS\x1b[24m");
    assert_eq!(
        heading_style(4, "Shell setup"),
        "\x1b[1m\x1b[2mShell setup\x1b[22m"
    );
    assert_eq!(
        heading_style(5, "Optional tools"),
        "\x1b[2m\x1b[4mOptional tools\x1b[24m\x1b[22m"
    );
    assert_eq!(
        heading_style(6, "Troubleshooting"),
        "\x1b[2mTroubleshooting\x1b[22m"
    );
}

#[test]
fn token_counts_use_the_compact_form() {
    assert_eq!(format_tokens(42), "42");
    assert_eq!(format_tokens(999), "999");
    assert_eq!(format_tokens(9600), "9.6k");
    assert_eq!(format_tokens(15000), "15k");
    assert_eq!(format_tokens(999000), "999k");
}

#[test]
fn durations_use_the_compact_form() {
    assert_eq!(format_duration(4_000), "4s");
    assert_eq!(format_duration(130_000), "2m 10s");
    assert_eq!(format_duration(362_000), "6m 2s");
    assert_eq!(format_duration(3_660_000), "1h 01m");
}

#[test]
fn model_labels_shorten_the_reference_way() {
    assert_eq!(compact_model_label("anthropic/claude-opus-4.7"), "opus 4.7");
    assert_eq!(compact_model_label("openai/gpt-4o"), "gpt-4o");
    assert_eq!(compact_model_label("zai/glm-5.2"), "glm-5.2");
}

fn read_theme(name: &str) -> (Theme, serde_json::Value) {
    let json = e::tui::theme::bundled_json(name == "light");
    (
        Theme::from_json(json).expect("parse"),
        serde_json::from_str(json).unwrap(),
    )
}

#[test]
fn the_two_themes_are_structural_mirrors() {
    let (_, light) = read_theme("light");
    let (_, dark) = read_theme("dark");
    let (lc, dc) = (&light["colors"], &dark["colors"]);
    let l_keys: Vec<_> = lc.as_object().unwrap().keys().collect();
    let d_keys: Vec<_> = dc.as_object().unwrap().keys().collect();
    assert_eq!(l_keys, d_keys);
    for key in l_keys {
        assert_eq!(lc[key], dc[key], "token {key} is not mirrored");
    }
    // A var nothing references is dead weight that hides drift.
    for theme in [&light, &dark] {
        let used: Vec<String> = theme["colors"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        for var in theme["vars"].as_object().unwrap().keys() {
            assert!(used.contains(var), "unused var {var}");
        }
    }
}

#[test]
fn the_palette_carries_the_reference_values() {
    let (light, _) = read_theme("light");
    let (dark, _) = read_theme("dark");
    let expected: &[(&str, i64, i64)] = &[
        ("ink", 235, 255),
        ("statusline", 241, 245),
        ("dim", 247, 245),
        ("divider", 250, 240),
        ("accent", 238, 252),
        ("comment", 243, 245),
        ("code", 241, 250),
        ("selected", 251, 239),
        ("selectedInk", 237, 255),
    ];
    for (name, l, d) in expected {
        assert_eq!(light.vars.get(*name), Some(l), "light var {name}");
        assert_eq!(dark.vars.get(*name), Some(d), "dark var {name}");
    }
}

use e::tui::markdown::{code_panel, render_markdown};

fn dark() -> Theme {
    read_theme("dark").0
}

#[test]
fn code_panel_geometry_matches_the_reference() {
    let t = dark();
    // The reference's own literals: a dim `─ label ─…` rule, flush-left
    // code, a dim solid rule below — no side rails, no padding.
    assert_eq!(
        code_panel(&t, "x", "zig", 80),
        vec!["\x1b[2m─ zig ─\x1b[22m", "x", "\x1b[2m───────\x1b[22m"]
    );
    assert_eq!(
        code_panel(&t, "x", "", 80),
        vec!["\x1b[2m──────\x1b[22m", "x", "\x1b[2m──────\x1b[22m"]
    );
    // Label truncated by display width to panel_width - 4 when narrow.
    assert_eq!(
        code_panel(&t, "x", "typescript", 8),
        vec!["\x1b[2m─ type ─\x1b[22m", "x", "\x1b[2m────────\x1b[22m"]
    );
    // A wide rune measures two cells, so the frame stays square.
    assert_eq!(
        code_panel(&t, "x", "漢", 6),
        vec!["\x1b[2m─ 漢 ─\x1b[22m", "x", "\x1b[2m──────\x1b[22m"]
    );
    // A retired reference behavior, deliberately not ported back: nothing
    // infers a language an unlabeled fence didn't declare. The fence
    // renders bare — no label in the rule, the content unhighlighted.
    let bare = code_panel(&t, "{\"a\": 1}", "", 80);
    assert_eq!(
        bare,
        vec![
            "\x1b[2m────────\x1b[22m",
            "{\"a\": 1}",
            "\x1b[2m────────\x1b[22m"
        ]
    );
}

#[test]
fn code_panel_survives_a_degenerate_terminal_width() {
    let t = dark();
    // 0–3 columns used to underflow panel_width arithmetic and panic; any
    // width must render without unwinding (the screen clips if needed).
    for cols in 0..=6 {
        let _ = code_panel(&t, "hello", "rust", cols);
        let _ = code_panel(&t, "hello", "", cols);
    }
}

#[test]
fn lists_match_the_reference_glyphs_and_indent() {
    let t = dark();
    let out = render_markdown(&t, "- one\n  - nested\n\n1. numbered\n", 40).join("\n");
    // The reference keeps the bullet's trailing space inside the dim run,
    // and dims ordered markers the same way.
    assert!(out.contains("\x1b[2m• \x1b[22mone"), "bullet: {out:?}");
    assert!(out.contains("  \x1b[2m• \x1b[22mnested"), "nested: {out:?}");
    assert!(
        out.contains("\x1b[2m1.\x1b[22m numbered"),
        "ordered: {out:?}"
    );

    // Ordered markers echo the author's numbering instead of renumbering.
    let echoed = render_markdown(&t, "1. a\n1. b\n", 40).join("\n");
    assert!(echoed.contains("\x1b[2m1.\x1b[22m a"), "{echoed:?}");
    assert!(echoed.contains("\x1b[2m1.\x1b[22m b"), "{echoed:?}");

    // Task lists: a dim ☐ replaces the bullet; a done ✓ wears the accent.
    let tasks = render_markdown(&t, "- [ ] open\n- [x] done\n", 40).join("\n");
    assert!(tasks.contains("\x1b[2m☐ \x1b[22mopen"), "{tasks:?}");
    assert!(
        tasks.contains(&format!("{} done", t.fg("accent", "✓"))),
        "{tasks:?}"
    );
}

#[test]
fn tables_render_the_reference_boxed_ladder() {
    let t = dark();
    // Fits the frame → the boxed grid: ┌┬┐ top, padded cells, ├┼┤ after the
    // header and between every body row, └┴┘ bottom.
    let out = render_markdown(&t, "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n", 80);
    assert_eq!(out[0], "┌───┬───┐");
    assert!(
        out[1].contains("│ ") && out[1].contains("\x1b[1ma\x1b[22m"),
        "{:?}",
        out[1]
    );
    assert_eq!(out[2], "├───┼───┤");
    assert_eq!(out[3], "│ 1 │ 2 │");
    assert_eq!(out[4], "├───┼───┤", "a separator rides between body rows");
    assert_eq!(out[5], "│ 3 │ 4 │");
    assert_eq!(out[6], "└───┴───┘");

    // A header cell's inline bold re-asserts the row bold after its close.
    let strong = render_markdown(&t, "| prefix **strong** suffix |\n|---|\n| value |\n", 80);
    assert!(
        strong
            .iter()
            .any(|l| l.contains("\x1b[1mprefix \x1b[1mstrong\x1b[22m\x1b[1m suffix\x1b[22m")),
        "{strong:?}"
    );

    // Too wide for the grid → the vertical key/value box at frame width.
    let vertical = render_markdown(
        &t,
        "| name | detail |\n|---|---|\n| one | aaaaaaaaaa |\n| two | bbbbbbbbbb |\n",
        16,
    );
    assert_eq!(vertical[0], "┌──────────────┐");
    assert!(
        vertical
            .iter()
            .any(|l| l.starts_with("│") && l.contains("name: one")),
        "{vertical:?}"
    );
    assert!(
        vertical.iter().any(|l| l == "├──────────────┤"),
        "records separate: {vertical:?}"
    );
    assert_eq!(vertical.last().unwrap(), "└──────────────┘");
}

#[test]
fn wrapping_reopens_styles_and_avoids_orphans() {
    use e::tui::markdown::wrap_styled;
    // A bold span crossing a seam closes at the row end and reopens on the
    // next row — a repainted row never leans on the row above it.
    let rows = wrap_styled("\x1b[1mbold words that wrap\x1b[22m tail", 12);
    assert!(rows.len() >= 2, "{rows:?}");
    assert!(rows[0].ends_with("\x1b[0m"), "{:?}", rows[0]);
    assert!(rows[1].starts_with("\x1b[1m"), "{:?}", rows[1]);

    // The reference's orphan rule: a lone last word pulls the previous word
    // down with it.
    let rows = wrap_styled("alpha beta gamma delta", 16);
    assert_eq!(rows, vec!["alpha beta", "gamma delta"]);

    // A link crossing a seam closes and reopens so each row hyperlinks.
    let link = "\x1b]8;;https://x.dev\x1b\\\x1b[4mspanning link text\x1b[24m\x1b]8;;\x1b\\";
    let rows = wrap_styled(link, 9);
    assert!(rows.len() >= 2);
    assert!(rows[0].ends_with("\x1b]8;;\x1b\\\x1b[0m"), "{:?}", rows[0]);
    assert!(
        rows[1].starts_with("\x1b]8;;https://x.dev\x1b\\"),
        "{:?}",
        rows[1]
    );

    // A hard-wrapped token may have several active SGR attributes. Every one
    // closes at the seam and reopens on the continuation row.
    let styled = "\x1b[1m\x1b[3m\x1b[4m\x1b[9m\x1b[38;5;245mabcdefgh\x1b[0m";
    let rows = wrap_styled(styled, 4);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].ends_with("\x1b[0m"), "{:?}", rows[0]);
    assert!(
        rows[1].starts_with("\x1b[1m\x1b[3m\x1b[4m\x1b[9m\x1b[38;5;245m"),
        "{:?}",
        rows[1]
    );
}

#[test]
fn soft_breaks_keep_the_authors_rows() {
    let t = dark();
    // The reference preserves the author's line breaks inside a paragraph
    // instead of reflowing them into one.
    let out = render_markdown(&t, "first line\nsecond line\n", 80);
    assert_eq!(out, vec!["first line", "second line"]);
}

#[test]
fn rules_and_blockquotes_match_byte_for_byte() {
    let t = dark();
    let hr = render_markdown(&t, "---\n", 92).join("\n");
    assert!(hr.contains(&format!("\x1b[2m{}\x1b[22m", "─".repeat(60))));
    let quote = render_markdown(&t, "> quoted\n", 92).join("\n");
    assert!(quote.contains("\x1b[2m│ \x1b[22mquoted"), "{quote:?}");
}

#[test]
fn inline_spans_match_the_reference() {
    let t = dark();
    let out = render_markdown(
        &t,
        "One **bold** with `code` and a [link](https://x.dev).\n",
        80,
    )
    .join("\n");
    assert!(out.contains("\x1b[1mbold\x1b[22m"));
    // Inline code: the palette's dedicated inline-code gray (dim var = 245 dark).
    assert!(out.contains("\x1b[38;5;245mcode\x1b[39m"), "{out:?}");
    // Links: underline only, OSC 8 wrapped with a document-scoped id (so a
    // wrapped link stays one link), no printed URL.
    assert!(
        out.contains("\x1b]8;id=e-1;https://x.dev\x1b\\\x1b[4mlink\x1b[24m\x1b]8;;\x1b\\"),
        "{out:?}"
    );
    assert!(!out.contains("(https://x.dev)"));
}

#[test]
fn image_inside_link_restores_the_outer_hyperlink() {
    let t = dark();
    let out = render_markdown(
        &t,
        "[before ![alt](https://image.test/i.png) https://label.test](https://outer.test)",
        120,
    )
    .join("\n");
    let outer = "\x1b]8;id=e-1;https://outer.test\x1b\\";
    let image_close = format!("\x1b[24m\x1b]8;;\x1b\\{outer}\x1b[4m https://label.test");

    assert_eq!(out.matches(outer).count(), 2, "{out:?}");
    assert!(out.contains(&image_close), "{out:?}");
    assert!(
        !out.contains("id=e-3"),
        "outer label was autolinked: {out:?}"
    );
}

#[test]
fn tool_rows_carry_no_done_suffix() {
    use e::tui::transcript::{Block, Kind};
    let theme = e::tui::theme::resolve("dark", false);

    // Success: the row is the row — the reference shape, no "(done)".
    let mut block = Block::new(Kind::Tool, "Ran");
    block.detail = Some("cargo test".into());
    block.done = true;
    block.result = Some("done".into());
    let rows = block.lines_for_test(&theme, 80);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].contains("(done)"));
    assert!(rows[0].contains("Ran"));

    // Failure: error-token marker plus the │ continuation with the outcome.
    let mut failed = Block::new(Kind::Tool, "Ran");
    failed.detail = Some("false".into());
    failed.done = true;
    failed.is_error = true;
    failed.result = Some("exit 7".into());
    let rows = failed.lines_for_test(&theme, 80);
    assert_eq!(rows.len(), 2);
    // The reference wording, exactly: "exit code 7", not "exit 7".
    assert!(rows[1].contains("│ exit code 7"));
}

#[test]
fn finished_tool_runs_collapse_to_the_reference_group() {
    use e::tui::transcript::{Block, Kind, Transcript};
    let theme = e::tui::theme::resolve("dark", false);

    let mut transcript = Transcript::default();
    transcript.push(Block::new(Kind::User, "go"));
    let mut read = Block::new(Kind::Tool, "Read");
    read.detail = Some("runtime.rs".into());
    read.done = true;
    let mut edit = Block::new(Kind::Tool, "Edited");
    edit.detail = Some("main.rs".into());
    edit.done = true;
    let mut ran = Block::new(Kind::Tool, "Ran");
    ran.detail = Some("cargo build".into());
    ran.done = true;
    ran.is_error = true;
    ran.result = Some("exit 1".into());
    transcript.push(read);
    transcript.push(edit);
    transcript.push(ran);
    transcript.collapse_tools();

    // The reference's own literal shape, e's verbs: header with tallies
    // ("1 read · 1 edit · 1 command · 1 failed"), ├ children, └ last.
    assert_eq!(transcript.blocks.len(), 2);
    let group = &transcript.blocks[1];
    assert_eq!(group.kind, Kind::ToolGroup);
    let rows = group.lines_for_test(&theme, 80);
    let plain: Vec<String> = rows
        .iter()
        .map(|r| {
            let mut out = String::new();
            let mut chars = r.chars();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    for e in chars.by_ref() {
                        if e.is_ascii_alphabetic() {
                            break;
                        }
                    }
                    continue;
                }
                out.push(c);
            }
            out
        })
        .collect();
    assert_eq!(
        plain[0],
        "● 3 tool calls · 1 read · 1 edit · 1 command · 1 failed"
    );
    assert_eq!(plain[1], "├ Read runtime.rs");
    assert_eq!(plain[2], "├ Edited main.rs");
    assert_eq!(plain[3], "└ Ran cargo build");

    // The reference pluralization: "3 commands", but "2 read".
    let mut t2 = Transcript::default();
    for cmd in ["a", "b", "c"] {
        let mut b = Block::new(Kind::Tool, "Ran");
        b.detail = Some(cmd.into());
        b.done = true;
        t2.push(b);
    }
    t2.collapse_tools();
    assert!(t2.blocks[0].text.starts_with("3 tool calls · 3 commands"));

    // Minimal mode groups a single completed call too.
    let mut t3 = Transcript::default();
    let mut single = Block::new(Kind::Tool, "Read");
    single.done = true;
    t3.push(single);
    t3.collapse_tools();
    assert_eq!(t3.blocks[0].kind, Kind::ToolGroup);
    assert_eq!(t3.blocks[0].text, "1 tool call · 1 read");
}

#[test]
fn command_rows_preview_their_output() {
    use e::tui::transcript::{Block, Kind};
    let theme = e::tui::theme::resolve("dark", false);
    let mut block = Block::new(Kind::Tool, "Ran");
    block.detail = Some("printf lines".into());
    block.done = true;
    block.is_error = true;
    block.result = Some("exit 7".into());
    block.preview = vec![
        "line-1".into(),
        "line-2".into(),
        "line-3".into(),
        "line-4".into(),
    ];
    block.more = 2;
    let rows = block.lines_for_test(&theme, 80);
    // The reference shape: output, process outcome, then expansion hint.
    assert_eq!(rows.len(), 7);
    assert!(rows[1].contains("│ line-1"));
    assert!(rows[4].contains("│ line-4"));
    assert!(rows[5].contains("│ exit code 7"));
    assert!(rows[6].contains("│ … 2 lines more (ctrl o to view)"));
}

#[test]
fn live_tool_group_replaces_running_state_and_streams_output() {
    use e::tui::transcript::{Block, ToolChild};
    let theme = e::tui::theme::resolve("dark", false);
    let mut group = Block::tool_group(vec![
        ToolChild::pending(
            1,
            "read".into(),
            "Reading".into(),
            "Read".into(),
            "src/core/mod.rs".into(),
        ),
        ToolChild::pending(
            2,
            "command".into(),
            "Running".into(),
            "Ran".into(),
            "cargo test".into(),
        ),
    ]);
    assert_eq!(group.text, "2 tool calls · 1 read · 1 command");

    group.start_tool(1);
    // The focused running call paints as the transient overlay row, not a
    // tree row; the still-pending sibling shows nowhere yet.
    let running = group.lines_for_test(&theme, 80);
    assert_eq!(running.len(), 1, "only the header while the call runs");
    let overlay = group.overlay_rows(&theme, 80);
    assert!(
        overlay[0].contains("Reading src/core/mod.rs"),
        "{overlay:?}"
    );
    assert!(!running.iter().any(|line| line.contains("cargo test")));
    let narrow = group.overlay_rows(&theme, 20);
    assert!(
        narrow[0].contains('…'),
        "long targets need an explicit ellipsis"
    );

    group.finish_tool(
        1,
        e::core::tools::ToolOutcome::Completed,
        "12 lines".into(),
        "content",
    );
    group.start_tool(2);
    group.append_tool_output(2, "one\ntwo\nthree\nfour\nfive\nsix\n");
    // The finished call keeps `├` — the tree stays open while the focused
    // call is out — and the live output streams under the overlay row.
    let streaming = group.lines_for_test(&theme, 80);
    assert!(streaming[1].contains("Read src/core/mod.rs"));
    assert!(streaming[1].contains('├'), "{:?}", streaming[1]);
    assert!(!streaming.iter().any(|line| line.contains("cargo test")));
    let overlay = group.overlay_rows(&theme, 80);
    assert!(overlay[0].contains("Running cargo test"));
    assert!(overlay[0].contains('└'), "a tree's focused call wears └");
    assert!(overlay.iter().any(|line| line.contains("one")));
    // The reference pluralizes the elision row: one hidden line is a "line".
    assert!(overlay
        .last()
        .unwrap()
        .contains("1 line more (ctrl o to view)"));

    group.finish_tool(
        2,
        e::core::tools::ToolOutcome::Failed,
        "exit 7".into(),
        "one\ntwo\nthree\nfour\nfive\nsix\n",
    );
    let done = group.lines_for_test(&theme, 80);
    // The reference withdraws pipe rows on completion — full output lives
    // behind ctrl+o, never inline. A non-zero exit keeps its `Ran` row; the
    // failure lives in the header tally.
    assert!(group.text.ends_with("· 1 failed"));
    assert!(done[2].contains("Ran cargo test"));
    assert!(!done.iter().any(|line| line.contains('│')));
}

#[test]
fn failed_turns_end_in_error_color() {
    use e::tui::transcript::{Block, Kind};
    let theme = e::tui::theme::resolve("dark", false);
    let block = Block::new(Kind::Error, "error: boom");
    let rows = block.lines_for_test(&theme, 80);
    // The reference notice grammar: `● Error:` in the error tone, the body
    // in the system-notice text gray.
    assert_eq!(
        rows[0],
        format!(
            "{} {}",
            theme.fg("error", "● Error:"),
            theme.fg("customMessageText", "error: boom")
        )
    );
}

#[test]
fn running_write_and_edit_rows_stay_lean() {
    use e::tui::transcript::{Block, ToolChild};
    let theme = e::tui::theme::resolve("dark", false);
    let mut group = Block::tool_group(vec![ToolChild::pending(
        1,
        "write".into(),
        "Writing".into(),
        "Wrote".into(),
        "src/lib.rs".into(),
    )]);
    group.start_tool(1);
    // A write streams no inline content: the overlay says "Writing
    // src/lib.rs" and nothing more — no file dump. (Full content still
    // lands behind ctrl+o.)
    group.append_tool_output(1, "hello\nworld\n");
    let overlay = group.overlay_rows(&theme, 80);
    assert!(overlay[0].contains("Writing src/lib.rs"), "{overlay:?}");
    assert!(overlay[0].contains('●'), "a lone call wears the ● marker");
    assert!(!overlay.iter().any(|line| line.contains('│')));
    let rows = group.lines_for_test(&theme, 80);
    assert!(!rows.iter().any(|line| line.contains('│')));

    // Edits are the same; the completion summary rides the row itself.
    group.finish_tool(
        1,
        e::core::tools::ToolOutcome::Completed,
        "+2 -0".into(),
        "hello\nworld\n",
    );
    let rows = group.lines_for_test(&theme, 80);
    // The reference stat suffix drops a zero side: `+2`, no ` -0`, with the
    // diff-marker hue on the count.
    assert!(rows[1].contains("Wrote src/lib.rs") && rows[1].contains("+2"));
    assert!(!rows[1].contains("-0"));
    assert!(!rows.iter().any(|line| line.contains('│')));
}

#[test]
fn silent_batches_continue_one_tree_and_long_trees_cap_rows() {
    use e::tui::transcript::{Block, Kind, ToolChild, Transcript};
    let theme = e::tui::theme::resolve("dark", false);
    let read = |id: u64, target: &str| {
        ToolChild::pending(
            id,
            "read".into(),
            "Reading".into(),
            "Read".into(),
            target.into(),
        )
    };

    // Batches with no assistant voice between them continue the same tree;
    // the collapsed thinking between batches is absorbed, not left to
    // fragment it.
    let mut t = Transcript::default();
    t.extend_tool_group(vec![read(1, "a.rs")]);
    t.push(Block::new(Kind::Thinking, "Thought for 2s"));
    t.extend_tool_group(vec![read(2, "b.rs")]);
    assert_eq!(t.blocks.len(), 1, "one tree across the silent batch");
    assert_eq!(t.blocks[0].text, "2 tool calls \u{b7} 2 read");

    // Assistant text separates trees; the next batch starts a new one.
    t.push(Block::new(Kind::Assistant, "Now the second stretch."));
    t.extend_tool_group(vec![read(3, "c.rs")]);
    assert_eq!(t.blocks.len(), 3);
    assert_eq!(t.blocks[2].text, "1 tool call \u{b7} 1 read");

    // The reference never caps the tree: every started call keeps its row
    // and the header carries the full tallies. The latest running call is
    // the focused one — out of the tree, on the overlay. (Rows render once
    // a call leaves its pending state, so start them.)
    let many = (0..12).map(|i| read(10 + i, &format!("{i}.rs"))).collect();
    t.extend_tool_group(many);
    assert_eq!(t.blocks[2].text, "13 tool calls \u{b7} 13 read");
    for id in (3..=3).chain(10..22) {
        t.blocks[2].start_tool(id);
    }
    let rows = t.blocks[2].lines_for_test(&theme, 80);
    assert_eq!(
        rows.len(),
        1 + 12,
        "header plus every started call but the focused one"
    );
    assert!(rows[1].contains("Reading c.rs"));
    assert!(rows.last().unwrap().contains("Reading 10.rs"));
    assert!(
        !rows.iter().any(|line| line.contains('└')),
        "the tree stays open while the focused call is out"
    );
    let overlay = t.blocks[2].overlay_rows(&theme, 80);
    assert!(overlay[0].contains("Reading 11.rs"), "{overlay:?}");
    assert!(!rows.iter().any(|line| line.contains("earlier tool calls")));
}

#[test]
fn picker_band_shrinks_with_its_rows() {
    use e::tui::menu::{Menu, MenuItem, MenuKind, HINT_USE};
    let theme = e::tui::theme::resolve("dark", false);
    let items: Vec<MenuItem> = (0..17)
        .map(|i| MenuItem::new(&format!("/cmd{i}"), "description", &format!("/cmd{i}")))
        .collect();
    let mut menu = Menu::new(MenuKind::Commands, "Commands", HINT_USE, items);
    let full = menu.render(&theme, 80);
    assert_eq!(full.len(), 6 + 4, "divider, header, blank, 6 rows, divider");
    // The header is uniformly dim, count and filter hint included.
    assert!(
        full[1].contains("Commands 17 · Type to filter"),
        "{:?}",
        full[1]
    );
    assert!(full[1].starts_with(theme.fg_prefix("dim")), "{:?}", full[1]);
    // The selected row signals by brightness alone: bold bright ink, no
    // fill background, no caret.
    assert!(full[3].contains("\x1b[1m"), "{:?}", full[3]);
    assert!(
        full[3].contains(theme.fg_prefix("userMessageText")),
        "{:?}",
        full[3]
    );
    assert!(!full[3].contains("\x1b[48;5;239m"), "{:?}", full[3]);

    // Filtering down to one match shrinks the band with it — the reference
    // never blank-pads a short list — and drops the filter clause from the
    // header.
    menu.set_query("cmd15");
    let filtered = menu.render(&theme, 80);
    assert_eq!(filtered.len(), 1 + 4);
    // The matched chars are brightened, so compare the escape-stripped row.
    let strip = |row: &str| -> String {
        let mut out = String::new();
        let mut chars = row.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for e in chars.by_ref() {
                    if e.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    };
    assert!(strip(&filtered[3]).contains("/cmd15"), "{:?}", filtered[3]);
    // The match highlight itself: a bold mark rides inside the row.
    assert!(filtered[3].contains("\x1b[1mc"), "{:?}", filtered[3]);
    assert!(filtered[1].contains("Commands 1"), "{:?}", filtered[1]);
    assert!(!filtered[1].contains("Type to filter"), "{:?}", filtered[1]);

    // No match at all: the notice alone, in the reference's own words.
    menu.set_query("zzz");
    let empty = menu.render(&theme, 80);
    assert_eq!(empty.len(), 1 + 4);
    assert!(empty[3].contains("no matching slash commands"));
}

#[test]
fn command_rows_carry_a_right_aligned_category() {
    use e::tui::menu::{Menu, MenuItem, MenuKind, HINT_USE};
    let theme = e::tui::theme::resolve("dark", false);
    // The `/` picker's category column: a built-in's functional group, a
    // prompt template's `Prompt` — each right-aligned in the row, and the
    // selected row's category brightens with it (copying the reference).
    let tag = |label: &str, meta: &str| {
        let mut item = MenuItem::new(label, "description", label);
        item.meta = meta.into();
        item
    };
    let items = vec![tag("/login", "Account"), tag("/deploy", "Prompt")];
    let menu = Menu::new(MenuKind::Commands, "Commands", HINT_USE, items);
    let rows = menu.render(&theme, 80);
    let strip = |row: &str| -> String {
        let mut out = String::new();
        let mut chars = row.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for e in chars.by_ref() {
                    if e.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    };
    // Rows: divider, header, blank, 2 rows, divider. Each tag lands
    // right-aligned past a run of padding.
    assert!(strip(&rows[3]).ends_with("   Account"), "{:?}", rows[3]);
    assert!(strip(&rows[4]).ends_with("   Prompt"), "{:?}", rows[4]);
    // The selected row (row 3) is bold-bright end to end — the category
    // `Account` rides inside that same bright dress, not a dim tail.
    let bright = theme.fg_prefix("userMessageText");
    let account_at = rows[3].find("Account").expect("category present");
    let dress_at = rows[3].find(bright).expect("bright dress present");
    assert!(dress_at < account_at, "category is inside the bright run");
    assert!(
        !rows[3][dress_at..account_at].contains("\x1b[39m"),
        "no color reset between the dress and the category: {:?}",
        rows[3]
    );
}

#[test]
fn trust_panel_offers_the_broader_ancestor_between_its_rows() {
    use e::tui::trustpanel::{render, TrustStage};
    let theme = e::tui::theme::resolve("dark", false);
    let mut stage = TrustStage {
        selected: 0,
        parent: Some(std::path::PathBuf::from("/home/u/code")),
    };
    let rows = render(&stage, &theme, 100, "/home/u/code/clones/e-1");
    // Three choices: this directory, the ancestor, decline — descriptions
    // three spaces past the longest label, not flung across the frame.
    assert!(
        rows[4].contains("Trust this directory   remembered"),
        "{:?}",
        rows[4]
    );
    assert!(rows[5].contains("Trust /home/u/code"), "{:?}", rows[5]);
    assert!(rows[5].contains("everything inside it"), "{:?}", rows[5]);
    assert!(rows[6].contains("Not now"), "{:?}", rows[6]);
    // Selection cycles through all three and wraps.
    stage.step(-1);
    assert_eq!(stage.selected, 2);
    assert_eq!(stage.choice(), (None, false), "the last row declines");
    stage.step(-1);
    assert_eq!(
        stage.choice(),
        (Some(std::path::PathBuf::from("/home/u/code")), true),
        "the middle row trusts the ancestor"
    );
    // Without a parent the panel keeps its two rows.
    let bare = TrustStage {
        selected: 0,
        parent: None,
    };
    assert_eq!(bare.row_count(), 2);
    assert_eq!(render(&bare, &theme, 100, "/w").len(), rows.len() - 1);
}

#[test]
fn footnote_syntax_stays_inert() {
    let theme = e::tui::theme::resolve("dark", false);
    // A retired reference behavior, deliberately not ported back: no
    // footnote grammar. The reference mark renders as the literal text the
    // author wrote, and a definition is an ordinary paragraph — nothing is
    // collected, numbered, or flushed at the end.
    let md = "First[^b] then[^a].\n\n[^a]: alpha note\n[^b]: beta note";
    let joined = render_markdown(&theme, md, 80).join("\n");
    assert!(
        joined.contains("[^b]") && joined.contains("[^a]"),
        "{joined:?}"
    );
    assert!(joined.contains("alpha note"), "{joined:?}");
    assert!(joined.contains("beta note"), "{joined:?}");
    assert!(!joined.contains(&theme.fg("dim", "[1]")), "{joined:?}");
}

#[test]
fn file_rows_segment_paths_the_reference_way() {
    use e::tui::menu::project_path;
    let plain = |width: usize, label: &str| -> String {
        project_path(label, width).iter().map(|(c, _)| c).collect()
    };
    // A path that fits shows whole.
    assert_eq!(
        plain(40, "src/tui/surfaces/menu.rs"),
        "src/tui/surfaces/menu.rs"
    );
    // Too long: the dirname middle-ellipsizes into its narrow budget
    // (w/3 clamped to 3–12), the basename keeps its prefix-biased tail.
    assert_eq!(plain(20, "src/tui/surfaces/menu.rs"), "src…es/menu.rs");
    // Directories carry their trailing slash through every projection.
    assert_eq!(plain(10, "src/core/providers/"), "s…e/pro…s/");
    assert_eq!(plain(40, "src/"), "src/");
    // Below the segmentation floor: basename alone, prefix-biased.
    assert_eq!(plain(7, "deep/dir/some-name.rs"), "some-…s");
}

#[test]
fn picker_tabs_follow_the_reference_grammar() {
    use e::tui::menu::{degrade_hint, Menu, MenuItem, MenuKind, HINT_MODELS, HINT_SKILLS};
    let theme = e::tui::theme::resolve("dark", false);
    let mut items = vec![
        MenuItem::new("alpha", "Global", "alpha"),
        MenuItem::new("beta", "Workspace", "beta"),
    ];
    items[0].tab = Some(1);
    items[1].tab = Some(2);
    let mut menu = Menu::new(MenuKind::Skills, "Skills", HINT_SKILLS, items).with_tabs(
        vec!["All".into(), "Global".into(), "Workspace".into()],
        Some(0),
        0,
        "Source",
    );
    // The header brightens its `{title} {count}`, lays the tabs two spaces
    // apart, and brackets only the active one; inactive tabs stay dim.
    let rows = menu.render(&theme, 80);
    assert!(rows[1].contains("Skills 2"), "{:?}", rows[1]);
    assert!(rows[1].contains("[All]"), "{:?}", rows[1]);
    assert!(rows[1].contains("\x1b[1m"), "{:?}", rows[1]);
    assert!(
        rows[1].contains(&theme.fg("dim", "Global")),
        "{:?}",
        rows[1]
    );
    assert_eq!(rows.len(), 2 + 4, "both skills under the All tab");

    // Tab narrows to one source at a time; counts and rows follow.
    menu.cycle_tab();
    let global = menu.render(&theme, 80);
    assert!(global[1].contains("[Global]"), "{:?}", global[1]);
    assert!(global[1].contains("Skills 1"), "{:?}", global[1]);
    assert_eq!(global.len(), 1 + 4);
    menu.cycle_tab();
    let workspace = menu.render(&theme, 80);
    assert!(workspace[3].contains("beta"), "{:?}", workspace[3]);

    // An empty narrower tab names the source it searched.
    menu.set_query("alpha");
    let empty = menu.render(&theme, 80);
    assert!(
        empty[3].contains("No Workspace skills found."),
        "{:?}",
        empty[3]
    );

    // The model picker degrades by windowing its tabs around the active
    // one, marking a clipped end with a dim ellipsis.
    let mut models = vec![
        MenuItem::new("a-model", "", "a"),
        MenuItem::new("b-model", "", "b"),
    ];
    models[0].tab = Some(1);
    models[1].tab = Some(2);
    let models = Menu::new(MenuKind::Models, "Models", HINT_MODELS, models).with_tabs(
        vec!["All".into(), "Anthropic".into(), "OpenAI".into()],
        Some(0),
        0,
        "",
    );
    let narrow = models.render(&theme, 20);
    assert!(narrow[1].contains("[All]"), "{:?}", narrow[1]);
    assert!(narrow[1].contains('…'), "{:?}", narrow[1]);
    assert!(!narrow[1].contains("Anthropic"), "{:?}", narrow[1]);

    // The hints degrade stepwise through the reference's ladders.
    assert_eq!(
        degrade_hint(HINT_SKILLS, 45),
        "↑↓ Navigate  Tab Source  Enter Use  Esc Close"
    );
    assert_eq!(degrade_hint(HINT_MODELS, 12), "Enter Esc");
}

#[test]
fn review_projection_shows_every_child_with_its_detail_link() {
    use e::tui::transcript::{Block, ToolChild};
    let theme = e::tui::theme::resolve("dark", false);
    let mut group = Block::tool_group(vec![
        ToolChild::pending(
            1,
            "read".into(),
            "Reading".into(),
            "Read".into(),
            "a.rs".into(),
        ),
        ToolChild::pending(
            2,
            "read".into(),
            "Reading".into(),
            "Read".into(),
            "b.rs".into(),
        ),
    ]);
    group.start_tool(1);
    group.finish_tool(1, e::core::tools::ToolOutcome::Completed, "done".into(), "");
    group.tool_children[0].detail = Some(41);
    group.start_tool(2);

    // The review screen has no overlay: the focused running call keeps a
    // row here, the last child wears └, and finished rows carry their
    // stored-detail id for the splice.
    let rows = group.review_lines(&theme, 80);
    assert_eq!(rows.len(), 3);
    assert!(rows[1].0.contains("Read a.rs"));
    assert_eq!(rows[1].1, Some(41));
    assert!(rows[2].0.contains("Reading b.rs"), "{:?}", rows[2].0);
    assert!(rows[2].0.contains('└'));
    assert_eq!(rows[2].1, None);
}

#[test]
fn sealed_groups_report_missing_results_instead_of_hiding_them() {
    use e::tui::transcript::{Block, ToolChild};
    let theme = e::tui::theme::resolve("dark", false);
    let mut group = Block::tool_group(vec![
        ToolChild::pending(
            1,
            "read".into(),
            "Reading".into(),
            "Read".into(),
            "a.rs".into(),
        ),
        ToolChild::pending(
            2,
            "read".into(),
            "Reading".into(),
            "Read".into(),
            "b.rs".into(),
        ),
    ]);
    group.start_tool(1);
    group.finish_tool(1, e::core::tools::ToolOutcome::Completed, "done".into(), "");
    // Live: the second call is pending — no row, no unreported tally.
    let live = group.lines_for_test(&theme, 80);
    assert_eq!(live.len(), 2);
    assert!(!group.text.contains("unreported"));

    // Sealed (a restored session): the recorded call whose result never
    // came gets an explicit row and an `unreported` tally — a header that
    // says "2 tool calls" sits above two rows, not one.
    group.seal();
    assert!(group.text.contains("· 1 unreported"), "{}", group.text);
    let sealed = group.lines_for_test(&theme, 80);
    assert_eq!(sealed.len(), 3);
    assert!(sealed[2].contains("Tool completion was not reported"));
}

#[test]
fn interrupted_tools_wear_the_cancelled_glyph() {
    use e::tui::transcript::{Block, Kind, Transcript};
    let theme = e::tui::theme::resolve("dark", false);
    let mut block = Block::new(Kind::Tool, "Ran");
    block.detail = Some("sleep 100".into());
    block.cancelled = true;
    let rows = block.lines_for_test(&theme, 80);
    assert!(rows[0].contains("■"), "cancelled marker missing");
    assert!(!rows[0].contains("●"));

    // Groups tally cancellations, the reference wording.
    let mut t = Transcript::default();
    let mut done = Block::new(Kind::Tool, "Read");
    done.done = true;
    t.push(done);
    t.push(block);
    t.collapse_tools();
    assert!(
        t.blocks[0].text.ends_with("· 1 cancelled"),
        "{}",
        t.blocks[0].text
    );
}

#[test]
fn a_malformed_user_theme_falls_back_instead_of_panicking() {
    use e::tui::theme::Theme;
    // Every shape that used to slice out of bounds, plus a non-ASCII value
    // whose byte length lies about its char count.
    for broken in [
        r##"{"vars":{},"colors":{"text":"#f"}}"##,
        r##"{"vars":{},"colors":{"text":"#12345"}}"##,
        r##"{"vars":{},"colors":{"text":"#1234567"}}"##,
        r##"{"vars":{},"colors":{"text":"#zzzzzz"}}"##,
        r##"{"vars":{},"colors":{"text":"#é2é4é6"}}"##,
        r#"{"vars":{},"colors":{"text":"nothex"}}"#,
    ] {
        // Must return Err (or Ok with the token dropped) — never unwind.
        let _ = Theme::from_json(broken);
    }
    // A valid theme still resolves its hex colors.
    let ok = Theme::from_json(r##"{"vars":{"ink":235},"colors":{"text":"#a1b2c3"}}"##).unwrap();
    assert_eq!(ok.fg_prefix("text"), "\x1b[38;2;161;178;195m");
}

#[test]
fn sleep_events_speak_in_the_system_grammar() {
    use e::core::output::format_elapsed;
    use e::tui::transcript::{Block, Kind};
    let theme = e::tui::theme::resolve("dark", false);

    // Woke inside the window: the record of the gap, then the turn goes on.
    let resumed = Block::new(
        Kind::System,
        format!(
            "the device was asleep for {} — continuing",
            format_elapsed(190)
        ),
    );
    let rows = resumed.lines_for_test(&theme, 80);
    // The reference notice grammar: bold accent label, notice-gray body.
    assert_eq!(
        rows[0],
        format!(
            "\x1b[1m{}\x1b[22m {}",
            theme.fg("customMessageLabel", "● System:"),
            theme.fg(
                "customMessageText",
                &format!(
                    "the device was asleep for {} — continuing",
                    format_elapsed(190)
                )
            )
        )
    );

    // Past the window: the stop line sits where "cancelled" would, and the
    // TurnEnd row is suppressed for it.
    let stopped = Block::new(
        Kind::System,
        format!(
            "run stopped — the device was asleep for {}",
            format_elapsed(22 * 60)
        ),
    );
    let rows = stopped.lines_for_test(&theme, 80);
    assert_eq!(
        rows[0],
        format!(
            "\x1b[1m{}\x1b[22m {}",
            theme.fg("customMessageLabel", "● System:"),
            theme.fg(
                "customMessageText",
                &format!(
                    "run stopped — the device was asleep for {}",
                    format_elapsed(22 * 60)
                )
            )
        )
    );
}
