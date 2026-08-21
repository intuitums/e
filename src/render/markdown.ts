/**
 * The the reference design Markdown renderer: markdown text → styled terminal lines.
 *
 * This is the successor to the transformer from the extension era. Owning the
 * frontend removes every constraint that made that code contorted: no escape
 * games to survive re-parsing, no gutter guessing, no hard-break smuggling —
 * blocks render straight to their final lines at a known width.
 *
 * Visual contract, pinned by tests against the reference design's Zig test literals:
 *   headings   level-specific SGR (bold+underline / bold / underline / …)
 *   bullets    dim `•`, two columns of indent per level, numbers kept
 *   code       shrink-wrapped `┌ lang ─┐` panel, the reference design highlight colors
 *   quotes     dim `│ ` rail, body upright
 *   rules      fixed 60 columns, SGR dim
 *   inline     bold/italic/strike as SGR; code in the reference design's inline-code gray;
 *              links underline-only with OSC 8 hyperlink
 *
 * Parsing uses marked's lexer (re-exported by pi-tui), the same library the engine's
 * own renderer is built on.
 */

import { Marked, visibleWidth, type Tokens } from "@earendil-works/pi-tui";
import {
	BOLD_ON,
	DIM_ON,
	UNDERLINE_OFF,
	UNDERLINE_ON,
	WEIGHT_OFF,
	bold,
	dim,
	headingStyle,
	rule,
} from "./ansi.ts";
import { highlight } from "./style.ts";
import { theme } from "./style.ts";

const marked = new Marked();

const ITALIC_ON = "\x1b[3m";
const ITALIC_OFF = "\x1b[23m";
const STRIKE_ON = "\x1b[9m";
const STRIKE_OFF = "\x1b[29m";
const OSC8 = (url: string) => `\x1b]8;;${url}\x1b\\`;
const OSC8_CLOSE = "\x1b]8;;\x1b\\";

/** the reference design's inline-code color: a dedicated constant, not the statusline gray. */
function inlineCode(text: string): string {
	return theme().fg("dim", text);
}

/** Render inline tokens to a single styled string. */
export function renderInline(tokens: ReadonlyArray<Tokens.Generic> | undefined): string {
	if (!tokens) return "";
	let out = "";
	for (const token of tokens) {
		switch (token.type) {
			case "text": {
				const t = token as Tokens.Text;
				out += t.tokens ? renderInline(t.tokens) : t.text;
				break;
			}
			case "escape":
				out += (token as Tokens.Escape).text;
				break;
			case "strong":
				out += `${BOLD_ON}${renderInline((token as Tokens.Strong).tokens)}${WEIGHT_OFF}`;
				break;
			case "em":
				out += `${ITALIC_ON}${renderInline((token as Tokens.Em).tokens)}${ITALIC_OFF}`;
				break;
			case "del":
				out += `${STRIKE_ON}${renderInline((token as Tokens.Del).tokens)}${STRIKE_OFF}`;
				break;
			case "codespan":
				out += inlineCode((token as Tokens.Codespan).text);
				break;
			case "link": {
				// the reference design: underline only, no color, OSC 8 hyperlink, no printed URL.
				const l = token as Tokens.Link;
				const label = renderInline(l.tokens) || l.text;
				out += `${OSC8(l.href)}${UNDERLINE_ON}${label}${UNDERLINE_OFF}${OSC8_CLOSE}`;
				break;
			}
			case "image":
				out += (token as Tokens.Image).text;
				break;
			case "br":
				out += "\n";
				break;
			case "html":
				out += (token as Tokens.HTML).raw;
				break;
			default:
				out += "raw" in token ? String(token.raw) : "";
		}
	}
	return out;
}

const ANSI_RUN = /\x1b\[[0-9;]*m|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g;

function plainWidth(styled: string): number {
	return visibleWidth(styled.replace(ANSI_RUN, ""));
}

/**
 * Word-wrap a styled string. ANSI runs travel with the word they precede;
 * a row never splits a word unless the word alone exceeds the width.
 */
export function wrapStyled(styled: string, width: number): string[] {
	const rows: string[] = [];
	for (const hardLine of styled.split("\n")) {
		const words = hardLine.split(" ");
		let row = "";
		let rowWidth = 0;
		for (const word of words) {
			const w = plainWidth(word);
			if (row !== "" && rowWidth + 1 + w > width) {
				rows.push(row);
				row = word;
				rowWidth = w;
			} else {
				row = row === "" ? word : `${row} ${word}`;
				rowWidth = row === word ? w : rowWidth + 1 + w;
			}
		}
		rows.push(row);
	}
	return rows.length > 0 ? rows : [""];
}

/**
 * the reference design's code panel — geometry from `codePanelWidth`/`appendCodePanelHeader` in
 * the reference design's `transcript_blocks.zig`, pinned byte-exact by the ported tests.
 */
export function codePanel(code: string, language: string, cols: number): string[] {
	const source = code.replace(/\n+$/, "");
	const lines = language ? safeHighlight(source, language) : source.split("\n");

	const maxCodeWidth = lines.reduce((max, l) => Math.max(max, plainWidth(l)), 0);
	const labelWidth = language === "" ? 0 : Math.min(visibleWidth(language), Math.max(0, cols - 5));
	const panelWidth = Math.min(cols, Math.max(6, Math.max(maxCodeWidth + 4, labelWidth + 5)));
	const innerWidth = panelWidth - 4;

	const out: string[] = [];
	if (labelWidth > 0) {
		const label = language.slice(0, panelWidth - 5) || "?";
		const edge = "─".repeat(Math.max(0, panelWidth - 4 - visibleWidth(label)));
		out.push(`┌ ${DIM_ON}${label}${WEIGHT_OFF} ${edge}┐`);
	} else {
		out.push(`┌${"─".repeat(panelWidth - 2)}┐`);
	}
	for (const line of lines) {
		for (const row of hardWrap(line, innerWidth)) {
			const pad = " ".repeat(Math.max(0, innerWidth - plainWidth(row)));
			out.push(`│ ${row}${pad} │`);
		}
	}
	out.push(`└${"─".repeat(panelWidth - 2)}┘`);
	return out;
}

function safeHighlight(source: string, language: string): string[] {
	try {
		const lines = highlight(source, language);
		return lines.length > 0 ? lines.map(stripNoopFg) : source.split("\n");
	} catch {
		return source.split("\n");
	}
}

/**
 * the engine's highlighter styles every token; ours maps several token kinds to the
 * default color, which leaves no-op `\x1b[39m…\x1b[39m` pairs behind. the reference design emits
 * nothing for unstyled tokens, and the panel tests are byte-exact, so drop any
 * default-fg reset that has no color open to reset.
 */
function stripNoopFg(line: string): string {
	let open = false;
	let out = "";
	for (const token of line.split(/(\x1b\[[0-9;]*m)/).filter(Boolean)) {
		if (token === "\x1b[39m") {
			if (open) {
				out += token;
				open = false;
			}
			continue;
		}
		if (/^\x1b\[38[;0-9]*m$/.test(token)) open = true;
		out += token;
	}
	return out;
}

const RESETS = new Set(["\x1b[0m", "\x1b[39m", "\x1b[22m", "\x1b[24m"]);

/** Hard-wrap one code line, closing and reopening any open color at the seam. */
function hardWrap(line: string, width: number): string[] {
	if (plainWidth(line) <= width) return [line];
	const rows: string[] = [];
	let open: string | null = null;
	let row = "";
	let rowWidth = 0;
	for (const token of line.split(/(\x1b\[[0-9;]*m)/).filter(Boolean)) {
		if (token.startsWith("\x1b")) {
			open = RESETS.has(token) ? null : token;
			row += token;
			continue;
		}
		for (const ch of token) {
			const w = visibleWidth(ch);
			if (rowWidth + w > width) {
				rows.push(open ? `${row}\x1b[0m` : row);
				row = open ?? "";
				rowWidth = 0;
			}
			row += ch;
			rowWidth += w;
		}
	}
	if (rowWidth > 0 || rows.length === 0) rows.push(open ? `${row}\x1b[0m` : row);
	return rows;
}

function renderList(token: Tokens.List, depth: number, width: number): string[] {
	const out: string[] = [];
	const pad = "  ".repeat(depth);
	let n = typeof token.start === "number" && token.start > 0 ? token.start : 1;
	for (const item of token.items) {
		const glyph = token.ordered ? `${n}. ` : `${dim("•")}${" "}`;
		const glyphWidth = token.ordered ? `${n}. `.length : 2;
		const hanging = pad + " ".repeat(glyphWidth);
		const bodyWidth = Math.max(8, width - pad.length - glyphWidth);

		// An item's tokens are inline content followed by any nested blocks.
		let firstLine = true;
		for (const child of item.tokens ?? []) {
			if (child.type === "list") {
				out.push(...renderList(child as Tokens.List, depth + 1, width));
				continue;
			}
			const inline =
				child.type === "text"
					? renderInline((child as Tokens.Text).tokens ?? [child])
					: renderInline([child]);
			for (const row of wrapStyled(inline, bodyWidth)) {
				out.push(firstLine ? `${pad}${glyph}${row}` : `${hanging}${row}`);
				firstLine = false;
			}
		}
		n++;
	}
	return out;
}

function renderTable(token: Tokens.Table, width: number): string[] {
	// the reference design's table glyphs: ` │ ` separators, `─┼─` junctions.
	const header = token.header.map((c) => renderInline(c.tokens));
	const rows = token.rows.map((r) => r.map((c) => renderInline(c.tokens)));
	const cols = header.length;
	const widths = header.map((h, i) =>
		Math.max(plainWidth(h), ...rows.map((r) => plainWidth(r[i] ?? ""))),
	);
	const line = (cells: string[], boldRow: boolean) =>
		cells
			.map((c, i) => {
				const padded = c + " ".repeat(Math.max(0, widths[i]! - plainWidth(c)));
				return boldRow ? bold(padded) : padded;
			})
			.join(` ${dim("│")} `);
	const out = [line(header, true)];
	out.push(dim(widths.map((w) => "─".repeat(w)).join("─┼─")));
	for (const r of rows) out.push(line(r.length === cols ? r : [...r, ...Array(cols - r.length).fill("")], false));
	return out.map((l) => (plainWidth(l) > width ? l : l));
}

/** Render a full markdown document to lines at `width`. Blocks separated by one blank row. */
export function renderMarkdown(markdown: string, width: number): string[] {
	const out: string[] = [];
	const push = (lines: string[]) => {
		if (out.length > 0 && lines.length > 0) out.push("");
		out.push(...lines);
	};

	for (const token of marked.lexer(markdown)) {
		switch (token.type) {
			case "space":
				break;
			case "heading":
				push([headingStyle((token as Tokens.Heading).depth, renderInline((token as Tokens.Heading).tokens))]);
				break;
			case "paragraph":
				push(wrapStyled(renderInline((token as Tokens.Paragraph).tokens), width));
				break;
			case "code":
				push(codePanel((token as Tokens.Code).text, ((token as Tokens.Code).lang ?? "").trim(), width));
				break;
			case "list":
				push(renderList(token as Tokens.List, 0, width));
				break;
			case "blockquote": {
				const inner = renderMarkdown((token as Tokens.Blockquote).text, Math.max(8, width - 2));
				push(inner.map((l) => `${DIM_ON}│ ${WEIGHT_OFF}${l}`));
				break;
			}
			case "hr":
				push([rule()]);
				break;
			case "table":
				push(renderTable(token as Tokens.Table, width));
				break;
			case "html":
			case "text":
			default:
				push(wrapStyled("raw" in token ? String(token.raw).trimEnd() : "", width));
		}
	}
	return out;
}
