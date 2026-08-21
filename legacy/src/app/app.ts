/**
 * The frame: transcript above, then activity row, composer, status line.
 *
 * Mounted as four children of a `TuiMainScreen`, which renders them in order
 * on the normal screen — the reference design's model: the transcript grows downward into
 * scrollback from wherever the session started, and only the last few rows
 * (activity/composer/status) repaint in place.
 */

import { spawn } from "node:child_process";
import type { AgentSession, AgentSessionRuntime } from "@earendil-works/pi-coding-agent";
import { matchesKey, type TUI } from "@earendil-works/pi-tui";
import { ActivityRow, formatDuration, formatTokens, freshTurn, noteTool, type Turn } from "./activity.ts";
import { Composer } from "./composer.ts";
import { Statusline } from "./statusline.ts";
import {
	Transcript,
	assistantBlock,
	bannerBlock,
	summaryBlock,
	toolBlock,
	userBlock,
	type Block,
	type ToolRowState,
} from "./transcript.ts";
import { editorTheme } from "../render/style.ts";
import { FooterMenu, type MenuSpec } from "../footer/menu.ts";
import { findCommand } from "../commands/index.ts";
import { HINT_PICKER, HINT_RESUME, commandItems, modelItems, sessionItems } from "./menus.ts";
import { noticeBlock } from "./transcript.ts";

/** the reference design's tool-row labels: `● Read runtime.zig`, `● Ran zig build`. */
function toolPresentation(name: string, args: any): { verb: string; target: string } {
	const base = (p: unknown) => (typeof p === "string" ? p.split("/").pop() ?? p : "");
	switch (name) {
		case "read": return { verb: "Read", target: base(args?.path) };
		case "write": return { verb: "Wrote", target: base(args?.path) };
		case "edit": return { verb: "Edited", target: base(args?.path) };
		case "bash": return { verb: "Ran", target: String(args?.command ?? "").split("\n")[0] ?? "" };
		case "grep": return { verb: "Searched", target: String(args?.pattern ?? "") };
		case "find": return { verb: "Searched", target: String(args?.pattern ?? "") };
		case "ls": return { verb: "Listed", target: base(args?.path) || "." };
		default: return { verb: name, target: "" };
	}
}

export class App {
	readonly transcript = new Transcript();
	readonly activity = new ActivityRow();
	readonly composer: Composer;
	readonly statusline: Statusline;

	private session?: AgentSession;
	private turn?: Turn;
	private ticker?: ReturnType<typeof setInterval>;
	private streamingBlock?: Block;
	private streamingText = "";
	private toolStates = new Map<string, ToolRowState>();
	private ctrlCArmedAt = 0;
	private onQuit: () => void;
	private tui: TUI;
	private runtime?: AgentSessionRuntime;
	private menu?: FooterMenu;
	private menuIsSlash = false;
	private unsubscribe?: () => void;
	version = "0.0.0";

	constructor(tui: TUI, onQuit: () => void) {
		this.tui = tui;
		this.onQuit = onQuit;
		this.composer = new Composer(tui, editorTheme());
		this.statusline = new Statusline(() => this.session);

		tui.addChild(this.transcript);
		tui.addChild(this.activity);
		tui.addChild(this.composer);
		tui.addChild({
			render: (width: number) => this.menu?.render(width) ?? [],
			invalidate: () => {},
		});
		tui.addChild(this.statusline);
		tui.setFocus(this.composer);

		this.composer.onSubmit = (text) => this.submit(text);
		this.composer.onChange = (text) => this.onComposerChange(text);
		tui.addInputListener((data) => this.handleKey(data));
	}

	banner(version: string): void {
		this.transcript.push(bannerBlock(version));
	}

	attach(session: AgentSession): void {
		this.unsubscribe?.();
		this.session = session;
		this.unsubscribe = session.subscribe((event) => this.onEvent(event));
	}

	setRuntime(runtime: AgentSessionRuntime): void {
		this.runtime = runtime;
		runtime.setRebindSession(async (session) => {
			this.attach(session);
			this.replay(session);
		});
	}

	/** Rebuild the transcript from a session's stored branch (resume/new). */
	private replay(session: AgentSession): void {
		this.transcript.clear();
		this.transcript.push(bannerBlock(this.version));
		try {
			for (const entry of session.sessionManager.getBranch() as any[]) {
				if (entry.type !== "message") continue;
				const message = entry.message;
				const text = contentText(message);
				if (!text.trim()) continue;
				if (message.role === "user") this.transcript.push(userBlock(text));
				else if (message.role === "assistant") this.transcript.push(assistantBlock(() => text));
			}
		} catch {
			// a session we cannot replay still gets a working composer
		}
		this.tui.requestRender(true);
	}

	/* ---------- menus ---------- */

	openMenu(spec: MenuSpec, isSlash = false): void {
		this.menu = new FooterMenu(spec);
		this.menuIsSlash = isSlash;
		this.statusline.hint = spec.hint;
		if (isSlash) this.menu.setQuery(this.composer.getText().slice(1));
		this.tui.requestRender();
	}

	closeMenu(): void {
		if (!this.menu) return;
		const spec = this.menu.spec;
		this.menu = undefined;
		this.menuIsSlash = false;
		this.statusline.hint = undefined;
		spec.onClose();
		this.tui.requestRender();
	}

	private onComposerChange(text: string): void {
		if (this.menuIsSlash) {
			if (!text.startsWith("/")) this.closeMenu();
			else {
				this.menu!.setQuery(text.slice(1));
				this.tui.requestRender();
			}
			return;
		}
		if (!this.menu && text.startsWith("/") && !text.includes(" ")) {
			this.openSlashMenu();
		}
	}

	private openSlashMenu(): void {
		this.openMenu(
			{
				title: "Commands",
				items: commandItems(this.session),
				hint: HINT_PICKER,
				onSelect: (item) => {
					this.composer.setText("");
					this.closeMenu();
					void this.dispatch(item.value);
				},
				onClose: () => {},
			},
			true,
		);
	}

	openHelpMenu(): void {
		this.openMenu({
			title: "Commands",
			items: commandItems(this.session),
			hint: HINT_PICKER,
			onSelect: (item) => {
				this.closeMenu();
				void this.dispatch(item.value);
			},
			onClose: () => {},
		});
	}

	openResumeMenu(): void {
		const cwd = this.session?.sessionManager.getCwd() ?? process.cwd();
		void sessionItems(cwd).then((items) => {
			this.openMenu({
				title: "Sessions",
				items,
				hint: HINT_RESUME,
				onSelect: (item) => {
					this.closeMenu();
					void this.runtime
						?.switchSession(item.value)
						.catch((e) => this.notice(`Could not resume: ${e?.message ?? e}`));
				},
				onClose: () => {},
			});
		});
	}

	openModelMenu(): void {
		const s = this.session;
		if (!s) return;
		this.openMenu({
			title: "Models",
			items: modelItems(s),
			hint: HINT_PICKER,
			onSelect: (item) => {
				this.closeMenu();
				const [provider, ...rest] = item.value.split("/");
				const id = rest.join("/");
				const model = ((s.modelRuntime as any).getModel?.(provider, id) ??
					(s.modelRuntime as any).getModels?.().find((m: any) => m.id === id)) as any;
				if (model) {
					void Promise.resolve(s.setModel(model))
						.then(() => this.notice(`Model set to ${id}.`))
						.catch((e) => this.notice(`Could not set model: ${e?.message ?? e}`));
				}
			},
			onClose: () => {},
		});
	}

	/* ---------- commands ---------- */

	notice(text: string): void {
		this.transcript.push(noticeBlock(text));
		this.tui.requestRender();
	}

	async dispatch(input: string): Promise<void> {
		const trimmed = input.trim();
		const space = trimmed.indexOf(" ");
		const name = space === -1 ? trimmed : trimmed.slice(0, space);
		const args = space === -1 ? "" : trimmed.slice(space + 1);
		const command = findCommand(name);
		if (command) {
			await command.run(this, args);
			return;
		}
		// Unknown to e: hand to the engine's dispatcher — extension commands, skills.
		this.session?.sendUserMessage(trimmed, { expandPromptTemplates: true });
	}

	newSession(): void {
		void this.runtime?.newSession().catch((e) => this.notice(`Could not start a session: ${e?.message ?? e}`));
	}

	renameSession(args: string): void {
		const name = args.trim();
		if (!name) {
			this.notice("Usage: /rename <title>");
			return;
		}
		this.session?.setSessionName(name);
		this.notice(`Renamed to ${name}.`);
	}

	copyLastResponse(): void {
		const text = this.session?.getLastAssistantText?.();
		if (!text) {
			this.notice("Nothing to copy yet.");
			return;
		}
		const pb = spawn("pbcopy");
		pb.stdin.end(text);
		this.notice("Copied the last response.");
	}

	compact(): void {
		this.notice("Compacting…");
		void Promise.resolve(this.session?.compact()).catch((e) =>
			this.notice(`Compaction failed: ${e?.message ?? e}`),
		);
	}

	showStatus(): void {
		const s = this.session;
		if (!s) return;
		const model = s.model?.id ?? "no model";
		this.notice(`e ${this.version} · ${model} · ${s.thinkingLevel ?? "off"} · ${s.sessionManager.getCwd()}`);
	}

	showVersion(): void {
		this.notice(`e ${this.version}`);
	}

	quit(): void {
		this.onQuit();
	}

	/* ---------- input ---------- */

	private handleKey(data: string): { consume?: boolean } | undefined {
		const s = this.session;
		if (this.menu) {
			if (matchesKey(data, "up") || matchesKey(data, "ctrl+k")) {
				this.menu.move(-1);
				this.tui.requestRender();
				return { consume: true };
			}
			if (matchesKey(data, "down") || matchesKey(data, "ctrl+j")) {
				this.menu.move(1);
				this.tui.requestRender();
				return { consume: true };
			}
			if (matchesKey(data, "return")) {
				const item = this.menu.current();
				if (item) this.menu.spec.onSelect(item);
				else this.closeMenu();
				return { consume: true };
			}
			if (matchesKey(data, "escape")) {
				if (this.menuIsSlash) this.composer.setText("");
				this.closeMenu();
				return { consume: true };
			}
		}
		if (matchesKey(data, "escape")) {
			// the reference design: esc interrupts the running turn (menus will cascade in front later).
			if (s?.isStreaming) {
				void s.abort();
				return { consume: true };
			}
			return undefined;
		}
		if (matchesKey(data, "ctrl+c")) {
			const now = Date.now();
			if (s?.isStreaming) {
				void s.abort();
				this.arm(now);
				return { consume: true };
			}
			if (this.composer.getText().length > 0) {
				this.composer.setText("");
				this.arm(now);
				this.tui.requestRender();
				return { consume: true };
			}
			if (now - this.ctrlCArmedAt < 1500) {
				this.onQuit();
				return { consume: true };
			}
			this.arm(now);
			return { consume: true };
		}
		if (matchesKey(data, "ctrl+d") && this.composer.getText().length === 0) {
			this.onQuit();
			return { consume: true };
		}
		return undefined;
	}

	private arm(now: number): void {
		this.ctrlCArmedAt = now;
		this.statusline.overlay = "press ctrl+c again to exit";
		this.tui.requestRender();
		setTimeout(() => {
			if (Date.now() - this.ctrlCArmedAt >= 1400) {
				this.statusline.overlay = undefined;
				this.tui.requestRender();
			}
		}, 1600);
	}

	private submit(text: string): void {
		const trimmed = text.trim();
		if (trimmed === "") return;
		this.composer.addToHistory(text);
		this.composer.setText("");

		if (trimmed.startsWith("/")) {
			this.closeMenu();
			void this.dispatch(trimmed);
			return;
		}

		const s = this.session;
		if (!s) return;
		// the reference design queues prompts typed mid-turn; the engine's followUp is the same contract.
		void s.prompt(trimmed, s.isStreaming ? { streamingBehavior: "followUp" } : undefined).catch(() => {});
		this.tui.requestRender();
	}

	/* ---------- session events ---------- */

	private onEvent(event: any): void {
		switch (event.type) {
			case "agent_start": {
				this.turn = freshTurn();
				this.activity.setTurn(this.turn);
				this.ticker ??= setInterval(() => this.tui.requestRender(), 1000);
				break;
			}
			case "message_start": {
				const role = event.message?.role;
				if (role === "user") {
					const text = contentText(event.message);
					if (text) this.transcript.push(userBlock(text));
				} else if (role === "assistant") {
					this.streamingText = "";
					const block = assistantBlock(() => this.streamingText);
					this.streamingBlock = block;
					this.transcript.push(block);
				}
				break;
			}
			case "message_update": {
				if (event.message?.role === "assistant" && this.streamingBlock) {
					this.streamingText = contentText(event.message);
					this.transcript.touch(this.streamingBlock);
				}
				break;
			}
			case "message_end": {
				const usage = event.message?.usage;
				if (usage && this.turn) {
					this.turn.input += (usage.input ?? 0) + (usage.cacheRead ?? 0) + (usage.cacheWrite ?? 0);
					this.turn.output += usage.output ?? 0;
				}
				if (event.message?.role === "assistant" && this.streamingBlock) {
					this.streamingText = contentText(event.message);
					this.transcript.touch(this.streamingBlock);
					this.streamingBlock = undefined;
				}
				break;
			}
			case "tool_execution_start": {
				const { verb, target } = toolPresentation(event.toolName, event.args);
				const state: ToolRowState = { verb, target };
				this.toolStates.set(event.toolCallId, state);
				this.transcript.push(toolBlock(state));
				if (this.turn) {
					noteTool(this.turn, event.toolName);
				}
				break;
			}
			case "tool_execution_end": {
				const state = this.toolStates.get(event.toolCallId);
				if (state) {
					state.done = true;
					state.isError = event.isError === true;
					if (state.isError) {
						const text = event.result?.content?.find?.((c: any) => c.type === "text")?.text;
						state.output = typeof text === "string" ? text : undefined;
					}
					this.transcript.invalidate();
				}
				break;
			}
			case "agent_settled": {
				if (this.turn) {
					const tokens =
						this.turn.input === 0 && this.turn.output === 0
							? ""
							: ` (↑${formatTokens(this.turn.input)} ↓${formatTokens(this.turn.output)})`;
					this.transcript.push(
						summaryBlock(`${formatDuration(Date.now() - this.turn.startedAt)}${tokens}`),
					);
				}
				this.turn = undefined;
				this.activity.setTurn(undefined);
				if (this.ticker) {
					clearInterval(this.ticker);
					this.ticker = undefined;
				}
				break;
			}
		}
		this.tui.requestRender();
	}
}

function contentText(message: any): string {
	if (typeof message?.content === "string") return message.content;
	if (!Array.isArray(message?.content)) return "";
	return message.content
		.filter((c: any) => c.type === "text")
		.map((c: any) => c.text)
		.join("");
}
