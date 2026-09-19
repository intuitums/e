/** JSONL requests, responses, and extension questions for one e process. */
import { spawn, type ChildProcess } from "node:child_process";
import { createInterface } from "node:readline";

export type Json = Record<string, unknown>;

/** A spawned `e rpc` and the pipes to it. */
export class Rpc {
  private child: ChildProcess;
  private next = 1;
  private stopped = false;
  private exited: Promise<void>;
  private failure?: Error;
  private pending = new Map<string, { resolve: (v: Json) => void; reject: (e: Error) => void }>();
  /** Event lines by session id; a turn's owner registers here. */
  readonly listeners = new Map<string, (event: Json) => void>();
  /** `ask` lines: an extension's question for a person. */
  onAsk: (ask: Json) => void = () => {};

  constructor(bin: string, args: string[], cwd?: string) {
    this.child = spawn(bin, [...args, "rpc"], { cwd, stdio: ["pipe", "pipe", "inherit"] });
    createInterface({ input: this.child.stdout! }).on("line", (line) => this.receive(line));
    this.exited = new Promise<void>((resolve) => {
      this.child.once("error", (error) => { this.stopped = true; this.fail(error); resolve(); });
      this.child.once("exit", (code) => {
        this.stopped = true;
        this.fail(new Error(`e rpc exited (${code})`));
        resolve();
      });
    });
    this.child.stdin!.on("error", (error) => this.fail(error));
  }

  /** Retire the connection so shutdown cannot wait on an exited process. */
  private fail(error: Error) {
    this.failure = error;
    for (const p of this.pending.values()) p.reject(error);
    this.pending.clear();
  }

  private receive(line: string) {
    let msg: Json;
    try {
      msg = JSON.parse(line);
    } catch {
      return;
    }
    if (typeof msg.type === "string") {
      if (msg.type === "ask") this.onAsk(msg);
      else if (typeof msg.session === "string") this.listeners.get(msg.session)?.(msg);
      return;
    }
    const p = this.pending.get(String(msg.id));
    if (!p) return;
    this.pending.delete(String(msg.id));
    if (msg.error) p.reject(new Error(String(msg.error)));
    else p.resolve((msg.result ?? {}) as Json);
  }

  /** One request, one response. A prompt resolves when its turn ends. */
  call(method: string, params: Json = {}): Promise<Json> {
    if (this.failure) return Promise.reject(this.failure);
    const id = `r${this.next++}`;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.child.stdin!.write(JSON.stringify({ id, method, params }) + "\n");
    });
  }

  /** Give shutdown a deadline, then terminate a process that stopped answering. */
  async close(timeoutMs = 1000) {
    if (this.stopped) return;
    const deadline = async (work: Promise<unknown>) => {
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        await Promise.race([work, new Promise<void>((resolve) => { timer = setTimeout(resolve, timeoutMs); })]);
      } finally { clearTimeout(timer); }
    };
    await deadline(this.call("shutdown").catch(() => {}));
    this.child.stdin?.end();
    await deadline(this.exited);
    if (!this.stopped) {
      this.child.kill("SIGTERM");
      await deadline(this.exited);
    }
    if (!this.stopped) {
      this.child.kill("SIGKILL");
      await this.exited;
    }
  }
}
