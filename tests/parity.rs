//! The conformance suite: byte-pinned against the reference design's own
//! test literals. Ported from `test/parity.test.ts`, which remains the
//! executable TypeScript twin until the swap milestone.

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
    assert_eq!(
        code_panel(&t, "x", "zig", 80),
        vec!["┌ \x1b[2mzig\x1b[22m ─┐", "│ x    │", "└──────┘"]
    );
    assert_eq!(
        code_panel(&t, "x", "", 80),
        vec!["┌────┐", "│ x  │", "└────┘"]
    );
    // Label truncated to panel_width - 5 when the terminal is narrow.
    assert_eq!(
        code_panel(&t, "x", "typescript", 8),
        vec!["┌ \x1b[2mtyp\x1b[22m ─┐", "│ x    │", "└──────┘"]
    );
}

#[test]
fn lists_match_the_reference_glyphs_and_indent() {
    let t = dark();
    let out = render_markdown(&t, "- one\n  - nested\n\n1. numbered\n", 40).join("\n");
    assert!(out.contains("\x1b[2m•\x1b[22m one"), "bullet: {out:?}");
    assert!(out.contains("  \x1b[2m•\x1b[22m nested"), "nested: {out:?}");
    assert!(out.contains("1. numbered"), "ordered: {out:?}");
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
    // Links: underline only, OSC 8 wrapped, no printed URL.
    assert!(
        out.contains("\x1b]8;;https://x.dev\x1b\\\x1b[4mlink\x1b[24m\x1b]8;;\x1b\\"),
        "{out:?}"
    );
    assert!(!out.contains("(https://x.dev)"));
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
    assert!(rows[1].contains("│ exit 7"));
}

#[test]
fn reasoning_renders_inline_markdown() {
    let theme = e::tui::theme::resolve("dark", false);
    let styled = e::tui::markdown::inline_spans(&theme, "**Assessing clarity** of `e docs`");
    assert!(!styled.contains("**"), "literal asterisks leaked");
    assert!(styled.contains("\x1b[1mAssessing clarity\x1b[22m"));
    assert!(!styled.contains('`'), "literal backticks leaked");
    // Unpaired markers pass through untouched.
    assert_eq!(
        e::tui::markdown::inline_spans(&theme, "2 ** 3 and a ` alone"),
        "2 ** 3 and a ` alone"
    );
}
