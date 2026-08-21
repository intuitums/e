/**
 * the reference design's footer-resident menu band.
 *
 * Menus render between the composer and the hint row, the reference design's inline-picker
 * shape (`picker_presentation.zig` / `catalog_screen_layout.zig` row format):
 *
 *   ── divider ──────────────
 *   Header 7 · Type to filter          3–9
 *   (blank)
 *     item        description
 *     item        description          ← selected row bold-white, no caret
 *   ── divider ──────────────
 *
 * The hint row itself is the statusline row; the app swaps its content while a
 * menu is open. Selection marker convention is the reference design's: none — bold vs dim.
 */

import { truncateToWidth, visibleWidth, type Component } from "@earendil-works/pi-tui";
import { theme } from "../render/style.ts";

export interface MenuItem {
	label: string;
	description?: string;
	/** Right-aligned dim metadata (the reference design's `resume-catalog · 8m · 12 turns`). */
	meta?: string;
	value: string;
}

export interface MenuSpec {
	title: string;
	items: MenuItem[];
	hint: string;
	/** Filter items as the user types after the trigger. */
	filter?: (query: string, items: MenuItem[]) => MenuItem[];
	onSelect: (item: MenuItem) => void;
	onClose: () => void;
}

const MAX_VISIBLE = 8;

export class FooterMenu implements Component {
	readonly spec: MenuSpec;
	query = "";
	selected = 0;
	private windowStart = 0;

	constructor(spec: MenuSpec) {
		this.spec = spec;
	}

	visibleItems(): MenuItem[] {
		const all = this.spec.items;
		if (this.query === "") return all;
		if (this.spec.filter) return this.spec.filter(this.query, all);
		const q = this.query.toLowerCase();
		return all.filter(
			(i) => i.label.toLowerCase().includes(q) || i.description?.toLowerCase().includes(q),
		);
	}

	move(delta: number): void {
		const n = this.visibleItems().length;
		if (n === 0) return;
		this.selected = (this.selected + delta + n) % n;
		if (this.selected < this.windowStart) this.windowStart = this.selected;
		if (this.selected >= this.windowStart + MAX_VISIBLE)
			this.windowStart = this.selected - MAX_VISIBLE + 1;
	}

	current(): MenuItem | undefined {
		return this.visibleItems()[this.selected];
	}

	setQuery(query: string): void {
		this.query = query;
		this.selected = 0;
		this.windowStart = 0;
	}

	invalidate(): void {}

	render(width: number): string[] {
		const t = theme();
		const items = this.visibleItems();
		const divider = t.fg("dim", "─".repeat(width));

		const count = items.length;
		const windowEnd = Math.min(this.windowStart + MAX_VISIBLE, count);
		const range =
			count > MAX_VISIBLE ? `${this.windowStart + 1}–${windowEnd}` : "";
		let header =
			t.bold(t.fg("userMessageText", this.spec.title)) +
			t.fg("muted", ` ${count}` + (this.query ? ` · ${this.query}` : " · Type to filter"));
		if (range) {
			const bare = header.replace(/\x1b\[[0-9;]*m/g, "");
			const pad = width - visibleWidth(bare) - range.length;
			if (pad > 1) header += " ".repeat(pad) + t.fg("muted", range);
		}

		const rows: string[] = [divider, header, ""];
		if (count === 0) {
			rows.push(t.fg("muted", "  Nothing found."));
		}
		// Column where descriptions start: after the longest visible label.
		const labelWidth = Math.min(
			36,
			Math.max(...items.slice(this.windowStart, windowEnd).map((i) => visibleWidth(i.label)), 0),
		);
		for (let i = this.windowStart; i < windowEnd; i++) {
			const item = items[i]!;
			const selected = i === this.selected;
			const label = item.label + " ".repeat(Math.max(0, labelWidth - visibleWidth(item.label)));
			let row = selected ? t.bold(t.fg("userMessageText", label)) : label;
			if (item.description) row += t.fg("muted", `  ${item.description}`);
			if (item.meta) {
				const bare = row.replace(/\x1b\[[0-9;]*m/g, "");
				const pad = width - 2 - visibleWidth(bare) - visibleWidth(item.meta);
				if (pad > 1) row += " ".repeat(pad) + t.fg("muted", item.meta);
			}
			rows.push(truncateToWidth(`  ${row}`, width, "…"));
		}
		rows.push(divider);
		return rows;
	}
}
