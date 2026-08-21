/**
 * Menu construction for the app: slash picker, help, resume, model.
 *
 * All are `FooterMenu`s in the reference design's inline-picker shape; the hint strings are the reference design's
 * own (`↑↓ Navigate     Enter Use     Esc Close` and the per-screen variants
 * from the catalog screens).
 */

import { SessionManager, type AgentSession } from "@earendil-works/pi-coding-agent";
import { COMMANDS } from "../commands/index.ts";
import type { MenuItem, MenuSpec } from "../footer/menu.ts";

export const HINT_PICKER = "↑↓ Navigate     Enter Use     Esc Close";
export const HINT_RESUME = "↑↓ Navigate     Enter Resume     Esc Close";

export function commandItems(session: AgentSession | undefined): MenuItem[] {
	const items: MenuItem[] = COMMANDS.filter((c) => c.name !== "/exit").map((c) => ({
		label: c.name,
		description: c.description,
		meta: c.category,
		value: c.name,
	}));
	// The user's extension commands, so /ship and friends stay reachable.
	try {
		const registered = (session?.extensionRunner as any)?.getRegisteredCommands?.() ?? [];
		for (const cmd of registered) {
			items.push({
				label: `/${cmd.name}`,
				description: cmd.description ?? "",
				meta: "Extensions",
				value: `/${cmd.name}`,
			});
		}
	} catch {
		// extension runner shape is the engine-internal; menus survive without it
	}
	return items;
}

function ago(date: Date): string {
	const s = Math.floor((Date.now() - date.getTime()) / 1000);
	if (s < 60) return `${s}s`;
	if (s < 3600) return `${Math.floor(s / 60)}m`;
	if (s < 86400) return `${Math.floor(s / 3600)}h`;
	return `${Math.floor(s / 86400)}d`;
}

export async function sessionItems(cwd: string): Promise<MenuItem[]> {
	const sessions = await SessionManager.list(cwd);
	return sessions
		.sort((a, b) => b.modified.getTime() - a.modified.getTime())
		.slice(0, 50)
		.map((info) => ({
			label: (info.name ?? info.firstMessage ?? info.id).slice(0, 48).replace(/\n/g, " "),
			meta: `${ago(info.modified)} · ${info.messageCount} turns`,
			value: info.path,
		}));
}

export function modelItems(session: AgentSession): MenuItem[] {
	const registry = (session.modelRuntime as any).getAvailableSnapshot?.() ??
		(session.modelRuntime as any).getModels?.() ?? [];
	const current = session.model?.id;
	return (registry as any[]).map((m) => ({
		label: m.id,
		description: m.provider ?? m.providerId ?? "",
		meta: m.id === current ? "current" : "",
		value: `${m.provider ?? m.providerId}/${m.id}`,
	}));
}
