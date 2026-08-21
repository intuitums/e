/**
 * Parity tests against the reference design's own expected output.
 *
 * The literals here are lifted from the reference design's test suite — `renderCodeBlockForTranscript`
 * in `ui/render_engine/transcript_blocks.zig` and the heading-style table in
 * `core/agent/assistant_presentation.zig`. If the panel maths or the SGR
 * sequences drift, these fail loudly rather than degrading quietly.
 */

import { strict as assert } from "node:assert";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { activityLabel, formatDuration, formatTokens, freshTurn, noteTool } from "../src/app/activity.ts";
import { headingStyle } from "../src/render/ansi.ts";
import { codePanel, renderMarkdown } from "../src/render/markdown.ts";
import { compactModelLabel } from "../src/app/statusline.ts";


test("code panel geometry matches the reference design", () => {
	assert.deepEqual(codePanel("x", "zig", 80), [
		"┌ \x1b[2mzig\x1b[22m ─┐",
		"│ x    │",
		"└──────┘",
	]);

	assert.deepEqual(codePanel("x", "", 80), ["┌────┐", "│ x  │", "└────┘"]);

	// Label truncated to panel_width - 5 when the terminal is narrow.
	assert.deepEqual(codePanel("x", "typescript", 8), [
		"┌ \x1b[2mtyp\x1b[22m ─┐",
		"│ x    │",
		"└──────┘",
	]);
});

test("heading styles match the reference design's level table", () => {
	assert.equal(headingStyle(1, "Workspace overview"), "\x1b[1m\x1b[4mWorkspace overview\x1b[24m\x1b[22m");
	assert.equal(headingStyle(2, "Installation"), "\x1b[1mInstallation\x1b[22m");
	assert.equal(headingStyle(3, "macOS"), "\x1b[4mmacOS\x1b[24m");
	assert.equal(headingStyle(4, "Shell setup"), "\x1b[1m\x1b[2mShell setup\x1b[22m");
	assert.equal(headingStyle(5, "Optional tools"), "\x1b[2m\x1b[4mOptional tools\x1b[24m\x1b[22m");
	assert.equal(headingStyle(6, "Troubleshooting"), "\x1b[2mTroubleshooting\x1b[22m");
});

test("token counts use the reference design's compact form", () => {
	assert.equal(formatTokens(42), "42");
	assert.equal(formatTokens(999), "999");
	assert.equal(formatTokens(9600), "9.6k");
	assert.equal(formatTokens(15000), "15k");
	assert.equal(formatTokens(999000), "999k");
});

test("durations use the reference design's compact form", () => {
	assert.equal(formatDuration(4_000), "4s");
	assert.equal(formatDuration(130_000), "2m 10s");
	assert.equal(formatDuration(362_000), "6m 2s");
	assert.equal(formatDuration(3_660_000), "1h 01m");
});

test("model labels shorten the reference way", () => {
	assert.equal(compactModelLabel("anthropic/claude-opus-4.7"), "opus 4.7");
	assert.equal(compactModelLabel("openai/gpt-4o"), "gpt-4o");
	assert.equal(compactModelLabel("zai/glm-5.2"), "glm-5.2");
});

test("lists match the reference design's glyphs, indent, and ordered markers", () => {
	const out = renderMarkdown("- one\n  - nested\n\n1. numbered\n", 40).join("\n");
	assert.equal(out.includes(`\x1b[2m•\x1b[22m one`), true);
	assert.equal(out.includes(`  \x1b[2m•\x1b[22m nested`), true);
	assert.equal(out.includes("1. numbered"), true);
});

test("the two themes are structural mirrors of each other", () => {
	const read = (name: string) =>
		JSON.parse(readFileSync(new URL(`../themes/${name}.json`, import.meta.url), "utf8"));
	const light = read("light");
	const dark = read("dark");

	// Every token must resolve through the same var name in both files, so the
	// palettes can only ever differ by the eight values in `vars`.
	assert.deepEqual(Object.keys(light.colors), Object.keys(dark.colors));
	for (const [token, value] of Object.entries(light.colors)) {
		assert.equal(dark.colors[token], value, `token "${token}" is not mirrored`);
	}
	assert.deepEqual(Object.keys(light.vars), Object.keys(dark.vars));

	// A var nothing references is dead weight that hides drift.
	for (const theme of [light, dark]) {
		const used = new Set(Object.values(theme.colors).map(String));
		for (const name of Object.keys(theme.vars)) {
			assert.equal(used.has(name), true, `unused var "${name}" in ${theme.name}`);
		}
	}
});

test("the palette carries the reference design's own values", () => {
	const read = (name: string) =>
		JSON.parse(readFileSync(new URL(`../themes/${name}.json`, import.meta.url), "utf8"));
	const light = read("light").vars;
	const dark = read("dark").vars;

	// [light, dark] exactly as the reference design defines them. Sources, in the reference design's tree:
	//   ui/render.zig                       — the *_style block and its light branch
	//   ui/render_engine/code_highlight.zig — the two syntax palettes
	//   core/agent/presentation/ansi.zig    — inline_code_{light,dark}_open
	//   ui/assistant/user_message_card.zig  — marker and accent styles
	const expected: Record<string, [number, number]> = {
		ink: [235, 255], // hint_style, and the card's marker_style
		statusline: [241, 245], // statusline_style
		dim: [247, 245], // dim_style, and inline code
		divider: [250, 240], // divider_style
		accent: [238, 252], // permission_auto, green, red, warning, task_completed
		comment: [243, 245], // syntax comments
		code: [241, 250], // syntax strings and numbers, and system_notice_text
		selected: [251, 239], // approval_button_inactive background
	};

	for (const [name, [l, d]] of Object.entries(expected)) {
		assert.equal(light[name], l, `light var "${name}"`);
		assert.equal(dark[name], d, `dark var "${name}"`);
	}
});

test("rules and blockquotes match the reference design byte for byte", () => {
	const hr = renderMarkdown("---\n", 92).join("\n");
	assert.equal(hr.includes(`\x1b[2m${"─".repeat(60)}\x1b[22m`), true);
	const quote = renderMarkdown("> quoted\n", 92).join("\n");
	assert.equal(quote.includes("\x1b[2m│ \x1b[22mquoted"), true);
});

test("the activity line reads the way the reference design's does", () => {
	const t0 = 1_000_000;
	const turn = freshTurn(t0);
	// the reference design's buildThinkingLabel: capital Thinking, elapsed clock after a beat.
	assert.equal(activityLabel(turn, t0), "Thinking");
	assert.equal(activityLabel(turn, t0 + 12_000), "Thinking (12s)");

	turn.input = 2200;
	turn.output = 14;
	assert.equal(activityLabel(turn, t0 + 12_000), "Thinking (12s) (↑2.2k ↓14)");

	// buildThinkingLabelFull: lowercase verb | tally, tally ordered by tool.
	noteTool(turn, "read"); noteTool(turn, "read"); noteTool(turn, "read"); noteTool(turn, "read");
	noteTool(turn, "bash");
	assert.equal(activityLabel(turn, t0), "running | 4 files read, 1 command started (↑2.2k ↓14)");
});
