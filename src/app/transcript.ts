/**
 * The transcript: an ordered list of blocks rendered with the reference design's gap policy.
 *
 * One component owns every block and the blank rows between them, so spacing
 * is decided in exactly one place — the port of the reference design's `BlockGapPolicy`
 * (`transcript_blocks.zig`): one blank row between blocks, except runs of tool
 * rows, which sit contiguous. This is the point where the engine's extra turn-boundary
 * whitespace ceases to exist.
 *
 * Rendered lines are cached per block; streaming updates invalidate only the
 * tail block.
 */

import type { Component } from "@earendil-works/pi-tui";
import { renderMarkdown, wrapStyled } from "../render/markdown.ts";
import { bold, dim } from "../render/ansi.ts";
import { theme } from "../render/style.ts";

export type BlockKind = "banner" | "user" | "assistant" | "tool" | "summary" | "notice";

export interface Block {
	kind: BlockKind;
	/** Produce final styled lines at the given content width. */
	lines(width: number): string[];
}

function gapRows(prev: BlockKind, next: BlockKind): number {
	if (prev === "tool" && next === "tool") return 0;
	return 1;
}

export class Transcript implements Component {
	private blocks: Block[] = [];
	private cache = new Map<Block, { width: number; lines: string[] }>();
	private dirty = new Set<Block>();

	push(block: Block): void {
		this.blocks.push(block);
	}

	/** Mark one block's content as changed (streaming updates). */
	touch(block: Block): void {
		this.dirty.add(block);
	}

	replaceLast(block: Block): void {
		const old = this.blocks.pop();
		if (old) this.cache.delete(old);
		this.blocks.push(block);
	}

	last(): Block | undefined {
		return this.blocks[this.blocks.length - 1];
	}

	isEmpty(): boolean {
		return this.blocks.length === 0;
	}

	clear(): void {
		this.blocks = [];
		this.cache.clear();
		this.dirty.clear();
	}

	invalidate(): void {
		this.cache.clear();
	}

	render(width: number): string[] {
		const out: string[] = [];
		let prev: BlockKind | undefined;
		for (const block of this.blocks) {
			let entry = this.cache.get(block);
			if (!entry || entry.width !== width || this.dirty.has(block)) {
				entry = { width, lines: block.lines(width) };
				this.cache.set(block, entry);
				this.dirty.delete(block);
			}
			if (entry.lines.length === 0) continue;
			if (prev !== undefined) for (let i = 0; i < gapRows(prev, block.kind); i++) out.push("");
			out.push(...entry.lines);
			prev = block.kind;
		}
		return out;
	}
}

/* ---------- Block constructors (the reference design shapes) ---------- */

/** `𝑒 v0.1.0 · Run /help for commands` — name bold, rest dim, the reference design's shape. */
export function bannerBlock(version: string): Block {
	return {
		kind: "banner",
		lines: () => [
			theme().bold(theme().fg("userMessageText", "𝑒")) +
				theme().fg("muted", ` v${version} · Run /help for commands`),
		],
	};
}

/** the reference design minimal-mode user card: `┃ ` rail, bold text, no tint, column 0. */
export function userBlock(text: string): Block {
	return {
		kind: "user",
		lines: (width) => {
			const rail = `${theme().fg("userMessageText", "┃")} `;
			const rows: string[] = [];
			for (const line of text.split("\n")) {
				if (line.trim() === "") {
					rows.push(theme().fg("userMessageText", "┃"));
					continue;
				}
				for (const row of wrapStyled(line, Math.max(8, width - 2))) rows.push(rail + bold(row));
			}
			return rows;
		},
	};
}

/** Assistant markdown with the reference design's two-column gutter. */
export function assistantBlock(getText: () => string): Block {
	return {
		kind: "assistant",
		lines: (width) => {
			const text = getText().trim();
			if (text === "") return [];
			return renderMarkdown(text, Math.max(8, width - 2)).map((l) => (l === "" ? "" : `  ${l}`));
		},
	};
}

export interface ToolRowState {
	verb: string;
	target: string;
	output?: string;
	isError?: boolean;
	done?: boolean;
}

/**
 * `  ● Read runtime.zig`, with a dimmed `└ ` continuation for output when the
 * row errs (the reference design shows output inline only for failures; expansion comes later).
 */
export function toolBlock(state: ToolRowState): Block {
	return {
		kind: "tool",
		lines: (width) => {
			const marker = state.done ? "●" : dim("●");
			const head = `  ${marker} ${state.verb}`;
			const target = state.target ? ` ${theme().fg("muted", state.target)}` : "";
			const rows = [head + target];
			if (state.isError && state.output) {
				const body = state.output.trim().split("\n").slice(0, 6);
				for (const line of body) rows.push(`  ${dim("└")} ${theme().fg("muted", line.slice(0, Math.max(8, width - 6)))}`);
			}
			return rows;
		},
	};
}

/** `  6m 2s (↑42 ↓9.6k)` — dim turn trailer. */
export function summaryBlock(text: string): Block {
	return { kind: "summary", lines: () => [theme().fg("dim", `  ${text}`)] };
}

/** `● System: …` style notice. */
export function noticeBlock(text: string): Block {
	return {
		kind: "notice",
		lines: (width) => wrapStyled(text, Math.max(8, width - 2)).map((l, i) => (i === 0 ? `  ${l}` : `  ${l}`)),
	};
}
