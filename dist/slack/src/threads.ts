/** Live RPC processes belong to threads; only saved log paths survive a restart. */
import { readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { randomUUID } from "node:crypto";
import type { Json, Rpc } from "./rpc.ts";

export type Connection = Pick<Rpc, "call" | "close" | "onAsk" | "listeners">;
type Thread = { rpc: Connection; session: string };

/** Own a connection per thread so extension questions always have one recipient. */
export class Threads {
  private paths = new Map<string, string>();
  private live = new Map<string, Promise<Thread>>();
  private connections = new Set<Connection>();
  private state: string;
  private connect: () => Connection;
  private defaults: Json;
  private onAsk: (key: string, rpc: Connection, ask: Json) => void;

  constructor(state: string, connect: () => Connection, defaults: Json,
              onAsk: (key: string, rpc: Connection, ask: Json) => void) {
    this.state = state;
    this.connect = connect;
    this.defaults = defaults;
    this.onAsk = onAsk;
    try {
      const saved = JSON.parse(readFileSync(state, "utf8")) as Record<string, { path?: string }>;
      if (!saved || typeof saved !== "object" || Array.isArray(saved)) {
        throw new Error("Slack state must be a map of thread paths");
      }
      for (const [key, thread] of Object.entries(saved)) {
        if (!thread || typeof thread !== "object" || Array.isArray(thread) ||
            (thread.path != null && typeof thread.path !== "string")) {
          throw new Error(`Invalid saved Slack thread: ${key}`);
        }
        // Older maps also stored session IDs. They belong to a dead process.
        if (typeof thread.path === "string") this.paths.set(key, thread.path);
      }
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
  }

  has(key: string): boolean {
    return this.live.has(key) || this.paths.has(key);
  }

  /** Concurrent first messages share the same connection and resume operation. */
  get(key: string, name: string): Promise<Thread> {
    const known = this.live.get(key);
    if (known) return known;
    const opening = this.open(key, name).catch((error) => {
      this.live.delete(key);
      throw error;
    });
    this.live.set(key, opening);
    return opening;
  }

  private async open(key: string, name: string): Promise<Thread> {
    const rpc = this.connect();
    this.connections.add(rpc);
    rpc.onAsk = (ask) => this.onAsk(key, rpc, ask);
    try {
      await rpc.call("hello", { ask: true });
      const path = this.paths.get(key);
      const created = await rpc.call("session.create", {
        ...this.defaults, save: true, name, ...(path ? { resume: path } : {}),
      });
      if (typeof created.path === "string") this.save(key, created.path);
      return { rpc, session: String(created.session) };
    } catch (error) {
      await rpc.close();
      this.connections.delete(rpc);
      throw error;
    }
  }

  /** Persist paths only; the next process must reopen each conversation. */
  save(key: string, path: string) {
    const paths = new Map(this.paths).set(key, path);
    const saved = Object.fromEntries([...paths].map(([key, path]) => [key, { path }]));
    const temporary = `${this.state}.${randomUUID()}.tmp`;
    try {
      writeFileSync(temporary, JSON.stringify(saved, null, 2), { flag: "wx", mode: 0o600, flush: true });
      renameSync(temporary, this.state);
      this.paths = paths;
    } finally {
      try { unlinkSync(temporary); } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      }
    }
  }

  async close() {
    // Include connections still waiting for hello or session.create.
    await Promise.all([...this.connections].map((rpc) => rpc.close()));
    this.connections.clear();
    this.live.clear();
  }
}
