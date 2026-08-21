/**
 * the reference design's status line: dot-separated segments in the statusline gray, on the last
 * row. Segment order from `buildHintLine` in the reference design's `ui/render.zig`: queued N,
 * permission mode (later), compact model label, reasoning effort, session
 * title, context %, workspace identity. Right side carries transient overlays
 * (`press ctrl+c again to exit`).
 */

import { truncateToWidth, visibleWidth, type Component } from "@earendil-works/pi-tui";
import type { AgentSession } from "@earendil-works/pi-coding-agent";
import { theme } from "../render/style.ts";
import { homedir } from "node:os";

const SEP = " · ";

export function compactModelLabel(model: string): string {
	const bare = model.slice(model.lastIndexOf("/") + 1);
	if (!bare.startsWith("claude-")) return bare;
	const name = bare.slice("claude-".length);
	for (const family of ["opus", "sonnet", "haiku"]) {
		if (name.startsWith(`${family}-`)) return `${family} ${name.slice(family.length + 1)}`;
	}
	return name;
}

function shortCwd(cwd: string): string {
	const home = homedir();
	const p = cwd.startsWith(home) ? `~${cwd.slice(home.length)}` : cwd;
	const parts = p.split("/");
	return parts.length > 3 ? `…/${parts.slice(-2).join("/")}` : p;
}

export class Statusline implements Component {
	overlay?: string;
	/** While a menu is open the row shows its keys instead — the reference design's hint row. */
	hint?: string;

	private session: () => AgentSession | undefined;

	constructor(session: () => AgentSession | undefined) {
		this.session = session;
	}

	invalidate(): void {}

	render(width: number): string[] {
		if (this.hint) return ["", theme().fg("muted", truncateToWidth(this.hint, width, ""))];
		const s = this.session();
		const segments: string[] = [];
		if (s) {
			const queued = s.pendingMessageCount;
			if (queued > 0) segments.push(`queued ${queued}`);
			if (s.model?.id) segments.push(compactModelLabel(s.model.id));
			const level = s.thinkingLevel;
			if (level && level !== "off") segments.push(level);
			const name = s.sessionName;
			if (name) segments.push(truncateToWidth(name, 32, "…"));
			const usage = s.getContextUsage?.();
			const pct = usage?.percent == null ? 0 : Math.round(usage.percent);
			if (pct >= 1) segments.push(`${pct}%`);
			segments.push(shortCwd(s.sessionManager.getCwd()));
		}
		if (segments.length === 0) return [""];

		const [head, ...rest] = segments;
		let line =
			theme().fg("accent", head!) +
			(rest.length > 0 ? theme().fg("muted", SEP + rest.join(SEP)) : "");

		if (this.overlay) {
			const overlay = theme().fg("muted", this.overlay);
			const pad = width - visibleWidth(line.replace(/\x1b\[[0-9;]*m/g, "")) - this.overlay.length;
			line = pad > 1 ? line + " ".repeat(pad) + overlay : overlay;
		}
		return ["", truncateToWidth(line, width, "")];
	}
}
