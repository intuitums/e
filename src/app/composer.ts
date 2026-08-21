/**
 * the reference design's composer: a `┃` rail at column zero, no rules, no tint.
 *
 * pi-tui's `Editor` supplies text editing, history, kill-ring, paste handling,
 * and autocomplete; it draws a full-width `─` rule above and below its content
 * and has no side borders. The rules are dropped on the way out and each
 * content row gets the rail — the exact treatment proven in the extension era,
 * now against the base `Editor` since app keybindings are ours to define.
 */

import { Editor, visibleWidth, type EditorTheme, type TUI } from "@earendil-works/pi-tui";
import { theme } from "../render/style.ts";

const RAIL_WIDTH = 2;
const ANSI_RUN = /\x1b\[[0-9;]*m/g;

function isBoundary(line: string): boolean {
	return line.replace(ANSI_RUN, "").startsWith("─");
}

function isPlainRule(line: string): boolean {
	const bare = line.replace(ANSI_RUN, "");
	return bare.length > 0 && /^─+$/.test(bare);
}

export class Composer extends Editor {
	constructor(tui: TUI, editorTheme: EditorTheme) {
		super(tui, editorTheme, { paddingX: 0 });
	}

	render(width: number): string[] {
		const inner = Math.max(1, width - RAIL_WIDTH);
		const raw = super.render(inner);

		const first = raw.findIndex(isBoundary);
		const last = raw.findIndex((line, i) => i > first && isBoundary(line));
		if (first === -1 || last === -1) return raw;

		const rail = `${theme().fg("userMessageText", "┃")} `;
		// the reference design separates the transcript from the composer with one blank row;
		// the composer band owns it (`footer_layout.zig`'s banner gap).
		const out: string[] = [""];
		const boundary = (line: string) =>
			isPlainRule(line) ? undefined : theme().fg("dim", line.replace(ANSI_RUN, ""));

		const top = boundary(raw[first]!);
		if (top !== undefined) out.push(top);
		for (const line of raw.slice(first + 1, last)) out.push(rail + line);
		const bottom = boundary(raw[last]!);
		if (bottom !== undefined) out.push(bottom);
		// Autocomplete rows keep their styling, indented to the text column.
		for (const line of raw.slice(last + 1)) out.push("  " + line);
		return out;
	}
}
