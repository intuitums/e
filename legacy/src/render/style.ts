/**
 * Theme bootstrap and the reference design style extras.
 *
 * Colors live in `themes/{light,dark}.json` — theme files whose
 * eight grayscale vars were audited byte-for-byte against the reference design's source
 * (`ui/render.zig`, `code_highlight.zig`, `presentation/ansi.zig`). Loading
 * them through the engine's own theme machinery buys three things at once: a `Theme`
 * instance for anything that wants one (extensions, later), `highlightCode`
 * emitting the reference design's exact syntax colors, and light/dark switching.
 *
 * The theme module is internal to the engine (not in its exports map), reached via
 * `import.meta.resolve` — the same route the parity harness proved.
 */

import type { Theme } from "@earendil-works/pi-coding-agent";

const entry = import.meta.resolve("@earendil-works/pi-coding-agent");
const themeModule = await import(entry.replace(/index\.js$/, "modes/interactive/theme/theme.js"));

const themesDir = new URL("../../../themes/", import.meta.url);

let current: Theme;

/** Load the light or dark palette and make it the active theme. */
export function setLight(light: boolean): void {
	const file = new URL(light ? "light.json" : "dark.json", themesDir).pathname;
	current = themeModule.loadThemeFromPath(file) as Theme;
	themeModule.setThemeInstance(current);
}

export function theme(): Theme {
	return current;
}

/** the reference design syntax colors, via the engine's highlighter running against the the reference design theme. */
export function highlight(code: string, lang?: string): string[] {
	return themeModule.highlightCode(code, lang) as string[];
}

export function languageFromPath(path: string): string | undefined {
	return themeModule.getLanguageFromPath(path) as string | undefined;
}

export function editorTheme() {
	return themeModule.getEditorTheme();
}

export function selectListTheme() {
	return themeModule.getSelectListTheme();
}

// Default to dark until the terminal is probed — both the engine's and the reference design's default.
setLight(false);
