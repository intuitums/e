/**
 * the reference design's style primitives.
 *
 * the reference design applies every emphasis with SGR attributes (bold / dim / underline) and
 * never with hue — the whole palette is a grayscale ramp living in the theme
 * JSON. These helpers emit the exact byte sequences the reference design does, so transformed
 * Markdown matches `the reference design`'s transcript output character for character.
 *
 * Raw SGR is safe inside assistant Markdown: the engine builds assistant `Markdown`
 * components with `defaultTextStyle === undefined`, so `applyDefaultStyle()`
 * returns text untouched and never injects a competing reset.
 */

export const BOLD_ON = "\x1b[1m";
export const DIM_ON = "\x1b[2m";
export const UNDERLINE_ON = "\x1b[4m";
/** SGR 22 clears bold *and* dim; SGR 24 clears underline. */
export const WEIGHT_OFF = "\x1b[22m";
export const UNDERLINE_OFF = "\x1b[24m";

export const bold = (s: string) => `${BOLD_ON}${s}${WEIGHT_OFF}`;
export const dim = (s: string) => `${DIM_ON}${s}${WEIGHT_OFF}`;

/** Heading styles per level, matching the reference design's `MarkdownProcessor` exactly. */
export function headingStyle(level: number, text: string): string {
	switch (level) {
		case 1:
			return `${BOLD_ON}${UNDERLINE_ON}${text}${UNDERLINE_OFF}${WEIGHT_OFF}`;
		case 2:
			return `${BOLD_ON}${text}${WEIGHT_OFF}`;
		case 3:
			return `${UNDERLINE_ON}${text}${UNDERLINE_OFF}`;
		case 4:
			return `${BOLD_ON}${DIM_ON}${text}${WEIGHT_OFF}`;
		case 5:
			return `${DIM_ON}${UNDERLINE_ON}${text}${UNDERLINE_OFF}${WEIGHT_OFF}`;
		default:
			return `${DIM_ON}${text}${WEIGHT_OFF}`;
	}
}

/**
 * Escape the ASCII punctuation that would otherwise start an inline construct.
 *
 * Assistant Markdown is parsed with `preserveBackslashEscapes` off, so the engine
 * normalises `\x` back to `x` at render time and the backslashes never reach
 * the screen.
 */
export function escapeInline(text: string): string {
	return text.replace(/[\\`*_[\]<>~|]/g, (ch) => `\\${ch}`);
}

/** Markdown hard line break: two trailing spaces before the newline. */
export const HARD_BREAK = "  \n";

/**
 * the reference design draws its thematic rule at a fixed 60 columns with SGR dim, not at the
 * content width and not in a palette colour (`horizontal_rule_width` and
 * `writeHorizontalRule` in `presentation/ansi.zig`). the engine's own rule is
 * `min(width, 80)` in the `mdHr` colour, so this is emitted directly.
 */
export const RULE_WIDTH = 60;
export const rule = () => dim("\u2500".repeat(RULE_WIDTH));

/**
 * the reference design's blockquote is a dim `│ ` rail with the quoted text left at its default
 * weight. the engine italicises quote bodies, which no theme token can undo.
 */
export const QUOTE_RAIL = `${DIM_ON}\u2502 ${WEIGHT_OFF}`;
