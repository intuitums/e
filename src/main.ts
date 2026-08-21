/**
 * e — a TUI for coding agents.
 *
 * Boots the agent engine through its SDK (same settings, sessions, models, skills,
 * and extensions as the engine CLI) and mounts our own frame on a
 * `TuiMainScreen`. the reference design is the visual spec; the engine is the machinery.
 */

import { readFileSync } from "node:fs";
import {
	SessionManager,
	SettingsManager,
	ModelRuntime,
	createAgentSessionServices,
	createAgentSessionFromServices,
	createAgentSessionRuntime,
	getAgentDir,
	type CreateAgentSessionRuntimeFactory,
} from "@earendil-works/pi-coding-agent";
import { ProcessTerminal, TuiMainScreen } from "@earendil-works/pi-tui";
import { App } from "./app/app.ts";
import { setLight } from "./render/style.ts";

const VERSION: string = JSON.parse(
	readFileSync(new URL("../package.json", import.meta.url), "utf8"),
).version;

function parseArgs(argv: string[]) {
	const args = { continue: false, message: [] as string[], version: false };
	for (const a of argv) {
		if (a === "--version" || a === "-v") args.version = true;
		else if (a === "--continue" || a === "-c") args.continue = true;
		else args.message.push(a);
	}
	return args;
}

async function main(): Promise<void> {
	const args = parseArgs(process.argv.slice(2));
	if (args.version) {
		console.log(`e ${VERSION}`);
		return;
	}

	const cwd = process.cwd();
	const agentDir = getAgentDir();
	const settingsManager = SettingsManager.create(cwd, agentDir);
	const modelRuntime = await ModelRuntime.create();

	// The runtime factory InteractiveMode itself uses: gives /new, /resume,
	// and fork their machinery, and rebinds our app when the session changes.
	const createRuntime: CreateAgentSessionRuntimeFactory = async ({ cwd: target, sessionManager, sessionStartEvent }) => {
		const services = await createAgentSessionServices({ cwd: target, agentDir, settingsManager, modelRuntime });
		return {
			...(await createAgentSessionFromServices({ services, sessionManager, sessionStartEvent })),
			services,
			diagnostics: services.diagnostics,
		};
	};
	const runtime = await createAgentSessionRuntime(createRuntime, {
		cwd,
		agentDir,
		sessionManager: args.continue ? SessionManager.continueRecent(cwd) : SessionManager.create(cwd),
	});
	const session = runtime.session;

	// Quick pre-paint light/dark guess from COLORFGBG; refined after start.
	const colorfgbg = process.env.COLORFGBG?.split(";");
	if (colorfgbg && Number(colorfgbg[colorfgbg.length - 1]) >= 7) setLight(true);

	const terminal = new ProcessTerminal();
	const tui = new TuiMainScreen(terminal);

	let quitting = false;
	const quit = async () => {
		if (quitting) return;
		quitting = true;
		tui.stop();
		await terminal.drainInput?.();
		runtime.dispose();
		process.exit(0);
	};

	const app = new App(tui, () => void quit());
	app.version = VERSION;
	app.banner(VERSION);
	app.attach(session);
	app.setRuntime(runtime);
	tui.start();

	// Definitive theme from the terminal itself, once it can answer.
	void tui
		.queryTerminalColorScheme({ timeoutMs: 1200 })
		.then((scheme) => {
			if (scheme) {
				setLight(scheme === "light");
				tui.invalidate();
				tui.requestRender(true);
			}
		})
		.catch(() => {});

	process.on("SIGINT", () => {});
	process.on("SIGTERM", () => void quit());

	if (args.message.length > 0) {
		void runtime.session.prompt(args.message.join(" ")).catch(() => {});
	}
}

main().catch((error) => {
	console.error(error);
	process.exit(1);
});
