/**
 * Deciding whether the terminal is light or dark.
 *
 * the engine resolves `the reference design:light/the reference design:dark` by probing the terminal at startup, but the
 * mode also has to switch the palette the moment it is turned on, before that
 * setting has been read. the engine exposes no lightness flag on `Theme`, so this reads
 * it back out of the theme that is already active: a theme built for a light
 * terminal paints its body text dark, and vice versa.
 *
 * Falls back to dark, which is both the engine's and the reference design's own default.
 */

import type { Theme } from "@earendil-works/pi-coding-agent";

/** xterm 256 greyscale ramp: 232 is near-black, 255 near-white. */
function greyscaleLuminance(index: number): number | undefined {
	if (index >= 232 && index <= 255) return ((index - 232) * 10 + 8) / 255;
	if (index >= 16 && index <= 231) {
		const n = index - 16;
		const levels = [0, 95, 135, 175, 215, 255];
		const r = levels[Math.floor(n / 36)] ?? 0;
		const g = levels[Math.floor((n % 36) / 6)] ?? 0;
		const b = levels[n % 6] ?? 0;
		return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
	}
	return undefined;
}

function luminanceOf(ansi: string): number | undefined {
	const truecolor = /\x1b\[38;2;(\d+);(\d+);(\d+)m/.exec(ansi);
	if (truecolor) {
		const [r, g, b] = truecolor.slice(1, 4).map(Number) as [number, number, number];
		return (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255;
	}
	const indexed = /\x1b\[38;5;(\d+)m/.exec(ansi);
	if (indexed) return greyscaleLuminance(Number(indexed[1]));
	return undefined;
}

export function isLightTerminal(theme: Theme): boolean {
	const luminance = luminanceOf(theme.getFgAnsi("text"));
	// Dark body text means the ground behind it is light.
	return luminance !== undefined && luminance < 0.5;
}
