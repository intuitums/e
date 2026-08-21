/**
 * e's slash commands — the reference design's command set mapped onto the agent engine.
 *
 * Names, descriptions, and categories follow the reference design's `command_specs.zig` where a
 * the engine analog exists; the engine-only capabilities keep the reference design's tone. Anything not in this
 * table falls through to `sendUserMessage(…, { expandPromptTemplates: true })`,
 * which is the engine's dispatcher for extension commands and skill invocations — so
 * the user's installed extensions keep their commands without e knowing
 * them.
 */

import type { App } from "../app/app.ts";

export interface Command {
	name: string;
	category: string;
	description: string;
	run: (app: App, args: string) => void | Promise<void>;
}

export const COMMANDS: Command[] = [
	{
		name: "/help",
		category: "General",
		description: "show available slash commands",
		run: (app) => app.openHelpMenu(),
	},
	{
		name: "/clear",
		category: "General",
		description: "start a fresh session",
		run: (app) => app.newSession(),
	},
	{
		name: "/new",
		category: "Session",
		description: "start a fresh session",
		run: (app) => app.newSession(),
	},
	{
		name: "/resume",
		category: "Session",
		description: "resume a saved session",
		run: (app) => app.openResumeMenu(),
	},
	{
		name: "/rename",
		category: "Session",
		description: "rename the current session",
		run: (app, args) => app.renameSession(args),
	},
	{
		name: "/copy",
		category: "Session",
		description: "copy the last assistant response",
		run: (app) => app.copyLastResponse(),
	},
	{
		name: "/compact",
		category: "Session",
		description: "compact older conversation turns",
		run: (app) => app.compact(),
	},
	{
		name: "/model",
		category: "Model",
		description: "choose model and reasoning effort",
		run: (app) => app.openModelMenu(),
	},
	{
		name: "/models",
		category: "Model",
		description: "browse available models",
		run: (app) => app.openModelMenu(),
	},
	{
		name: "/status",
		category: "General",
		description: "show runtime configuration",
		run: (app) => app.showStatus(),
	},
	{
		name: "/version",
		category: "General",
		description: "show the e version",
		run: (app) => app.showVersion(),
	},
	{
		name: "/quit",
		category: "General",
		description: "exit the interactive shell",
		run: (app) => app.quit(),
	},
	{
		name: "/exit",
		category: "General",
		description: "exit the interactive shell",
		run: (app) => app.quit(),
	},
];

export function findCommand(name: string): Command | undefined {
	return COMMANDS.find((c) => c.name === name);
}
