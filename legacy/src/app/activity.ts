/**
 * the reference design's activity row: `• Thinking (12s)` idle, or the verb-and-tally form
 * `• running | 4 files read, 1 command started (↑2.2k ↓14)` once tools move.
 *
 * Shapes follow `buildThinkingLabel` (capital Thinking, elapsed clock) and
 * `buildThinkingLabelFull` (lowercase verb, tally, tokens) in the reference design's
 * `core/output/activity_status.zig`, pinned by the ported tests.
 */

import type { Component } from "@earendil-works/pi-tui";
import { theme } from "../render/style.ts";

const TOOLS: Record<string, { verb: string; singular: string; plural: string; past: string }> = {
	read: { verb: "reading", singular: "file", plural: "files", past: "read" },
	ls: { verb: "listing", singular: "directory", plural: "directories", past: "listed" },
	write: { verb: "writing", singular: "file", plural: "files", past: "wrote" },
	edit: { verb: "editing", singular: "file", plural: "files", past: "edited" },
	bash: { verb: "running", singular: "command", plural: "commands", past: "started" },
	find: { verb: "reading", singular: "file", plural: "files", past: "read" },
	grep: { verb: "reading", singular: "file", plural: "files", past: "read" },
};
const ORDER = ["read", "ls", "write", "edit", "bash"];

export function formatTokens(tokens: number): string {
	if (tokens < 1000) return `${tokens}`;
	const whole = Math.floor(tokens / 1000);
	const tenths = Math.floor((tokens % 1000) / 100);
	return whole < 10 && tenths > 0 ? `${whole}.${tenths}k` : `${whole}k`;
}

export function formatDuration(ms: number): string {
	const seconds = Math.floor(ms / 1000);
	if (seconds < 60) return `${seconds}s`;
	if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
	const hours = Math.floor(seconds / 3600);
	return `${hours}h ${String(Math.floor((seconds % 3600) / 60)).padStart(2, "0")}m`;
}

export interface Turn {
	counts: Map<string, number>;
	lastVerb?: string;
	input: number;
	output: number;
	startedAt: number;
}

export function freshTurn(now = Date.now()): Turn {
	return { counts: new Map(), input: 0, output: 0, startedAt: now };
}

export function noteTool(turn: Turn, toolName: string): void {
	const spec = TOOLS[toolName];
	if (!spec) return;
	const key = toolName === "find" || toolName === "grep" ? "read" : toolName;
	turn.counts.set(key, (turn.counts.get(key) ?? 0) + 1);
	turn.lastVerb = spec.verb;
}

export function activityLabel(turn: Turn, now = Date.now()): string {
	const tokens =
		turn.input === 0 && turn.output === 0
			? ""
			: ` (↑${formatTokens(turn.input)} ↓${formatTokens(turn.output)})`;

	if (turn.lastVerb === undefined) {
		// No tool activity yet: the reference design's `• Thinking (12s)` shape.
		const elapsed = Math.floor((now - turn.startedAt) / 1000);
		const clock = elapsed >= 3 ? ` (${elapsed}s)` : "";
		return `Thinking${clock}${tokens}`;
	}

	const parts: string[] = [];
	for (const name of ORDER) {
		const count = turn.counts.get(name) ?? 0;
		if (count === 0) continue;
		const spec = TOOLS[name]!;
		parts.push(`${count} ${count === 1 ? spec.singular : spec.plural} ${spec.past}`);
	}
	return parts.length === 0
		? `${turn.lastVerb}${tokens}`
		: `${turn.lastVerb} | ${parts.join(", ")}${tokens}`;
}

/** The one-row component above the composer. Empty (zero rows) when idle. */
export class ActivityRow implements Component {
	private turn?: Turn;

	setTurn(turn: Turn | undefined): void {
		this.turn = turn;
	}

	invalidate(): void {}

	render(): string[] {
		if (!this.turn) return [];
		return ["", ` • ${activityLabel(this.turn)}`];
	}
}
