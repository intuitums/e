/**
 * e for Slack: each thread owns an e rpc process and session. Questions
 * and replies stay with that thread even while other threads run turns.
 * Copy this and change what your team wants posted.
 */

import { randomUUID } from "node:crypto";
import { resolve } from "node:path";
import bolt from "@slack/bolt";
import { Rpc, type Json } from "./rpc.ts";
import { Threads, type Connection } from "./threads.ts";

const { App } = bolt;

// ---------------------------------------------------------------- app

const env = (name: string) => {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is required (see .env.example)`);
  return v;
};

const cwd = resolve(env("E_CWD"));
const app = new App({
  token: env("SLACK_BOT_TOKEN"),
  signingSecret: env("SLACK_SIGNING_SECRET"),
  appToken: env("SLACK_APP_TOKEN"),
  socketMode: true,
});

const threads = new Threads(
  process.env.E_SLACK_STATE ?? "./e-slack-state.json",
  () => new Rpc(process.env.E_BIN ?? "e", [], cwd),
  { cwd, ...(process.env.E_MODEL ? { model: process.env.E_MODEL } : {}) },
  (key, rpc, ask) => relayAsk(key, rpc, ask),
);

/** Text a person wants to read about a finished tool call. */
function toolLine(batch: Map<number, Json>, end: Json): string | null {
  const call = batch.get(Number(end.id));
  if (!call) return null;
  const target = call.target ? ` \`${call.target}\`` : "";
  const failed = end.outcome !== "completed" ? " — failed" : "";
  return `▸ ${call.name}${target}${failed}`;
}

/** Slack messages cap near 4000 characters; split long replies on lines. */
function chunks(text: string, size = 3900): string[] {
  const out: string[] = [];
  let rest = text;
  while (rest.length > size) {
    let cut = rest.lastIndexOf("\n", size);
    if (cut < size / 2) cut = size;
    out.push(rest.slice(0, cut));
    rest = rest.slice(cut).replace(/^\n/, "");
  }
  out.push(rest);
  return out;
}

interface Post {
  (text: string, blocks?: unknown[]): Promise<unknown>;
}

/** Run one prompt on a thread's session, posting as it goes. */
async function runTurn(key: string, name: string, prompt: string, post: Post) {
  const { rpc, session } = await threads.get(key, name);
  const batch = new Map<number, Json>();
  rpc.listeners.set(session, (event) => {
    switch (event.type) {
      case "tool_batch":
        for (const call of event.calls as Json[]) batch.set(Number(call.id), call);
        break;
      case "tool_end": {
        const line = toolLine(batch, event);
        if (line) void post(line);
        break;
      }
      case "error":
        void post(`:warning: ${event.message}`);
        break;
    }
  });
  try {
    const result = await rpc.call("session.prompt", { session, prompt });
    if (typeof result.path === "string") threads.save(key, result.path);
    if (result.error) {
      await post(`:x: ${result.error}`);
      return;
    }
    if (result.aborted) {
      await post("_stopped_");
      return;
    }
    const text = String(result.final_output || "_(no reply)_");
    for (const part of chunks(text)) await post(part);
    const cost = result.cost_usd;
    if (typeof cost === "number") await post(`_$${cost.toFixed(4)}_`);
    const created = await rpc.call("session.info", { session });
    if (created.path) threads.save(key, String(created.path));
  } catch (error) {
    await post(`:x: ${(error as Error).message}`);
  } finally {
    rpc.listeners.delete(session);
  }
}

// A message in a thread while an extension waits for text answers it.
type Answer = (result: Json) => Promise<unknown>;
const textAsks = new Map<string, Answer>();
const buttonAsks = new Map<string, Answer>();

/** Route a question through the connection owned by this thread. */
function relayAsk(key: string, rpc: Connection, ask: Json) {
  const n = Number(ask.ask);
  const params = (ask.params ?? {}) as Json;
  const title = String(params.title ?? ask.method);
  const answer: Answer = (result) => rpc.call("ask.reply", { ask: n, result });
  const token = randomUUID();
  const [channel, thread_ts] = key.split(":");
  const post = (text: string, blocks?: unknown[]) =>
    app.client.chat.postMessage({ channel, thread_ts, text, blocks: blocks as never });
  switch (ask.method) {
    case "ui.confirm":
      buttonAsks.set(token, answer);
      void post(title, [
        { type: "section", text: { type: "mrkdwn", text: `*${title}*\n${params.message ?? ""}` } },
        {
          type: "actions",
          elements: [
            { type: "button", text: { type: "plain_text", text: "Yes" }, style: "primary", action_id: "ask_yes", value: token },
            { type: "button", text: { type: "plain_text", text: "No" }, action_id: "ask_no", value: token },
          ],
        },
      ]);
      break;
    case "ui.select": {
      buttonAsks.set(token, answer);
      const options = ((params.options ?? []) as (string | Json)[]).slice(0, 5).map((o) => {
        const label = typeof o === "string" ? o : String(o.label);
        const value = typeof o === "string" ? o : String(o.value ?? o.label);
        return {
          type: "button",
          text: { type: "plain_text", text: label.slice(0, 75) },
          action_id: `ask_pick_${value}`,
          value: JSON.stringify({ token, value, label }),
        };
      });
      void post(title, [
        { type: "section", text: { type: "mrkdwn", text: `*${title}*` } },
        { type: "actions", elements: options },
      ]);
      break;
    }
    default:
      // ui.input / ui.editor: the next message in the thread is the answer.
      textAsks.set(key, answer);
      void post(`*${title}* — reply in this thread${params.placeholder ? ` (${params.placeholder})` : ""}`);
  }
}

/** A button can answer only the process and question that created it. */
async function answerButton(token: string, result: Json) {
  const answer = buttonAsks.get(token);
  if (!answer) return;
  buttonAsks.delete(token);
  await answer(result);
}

app.action("ask_yes", async ({ ack, action }) => {
  await ack();
  await answerButton(String((action as unknown as Json).value), { confirmed: true });
});
app.action("ask_no", async ({ ack, action }) => {
  await ack();
  await answerButton(String((action as unknown as Json).value), { confirmed: false });
});
app.action(/^ask_pick_/, async ({ ack, action }) => {
  await ack();
  const { token, value, label } = JSON.parse(String((action as unknown as Json).value));
  await answerButton(token, { value, label });
});

function stripMention(text: string): string {
  return text.replace(/<@[A-Z0-9]+>/g, "").trim();
}

app.event("app_mention", async ({ event, client }) => {
  const channel = event.channel;
  const thread_ts = event.thread_ts ?? event.ts;
  const key = `${channel}:${thread_ts}`;
  const post: Post = (text, blocks) =>
    client.chat.postMessage({ channel, thread_ts, text, blocks: blocks as never });
  const prompt = stripMention(event.text ?? "");
  if (!prompt) {
    await post("Ask me something in this thread.");
    return;
  }
  await runTurn(key, `slack ${channel}/${thread_ts}`, prompt, post);
});

app.message(async ({ message, client }) => {
  // Thread replies only, from people, in threads we know.
  const m = message as unknown as Json;
  if (m.subtype || !m.thread_ts || m.thread_ts === m.ts) return;
  const channel = String(m.channel);
  const thread_ts = String(m.thread_ts);
  const key = `${channel}:${thread_ts}`;
  if (!threads.has(key)) return;
  const text = stripMention(String(m.text ?? ""));
  if (!text) return;
  const pendingAsk = textAsks.get(key);
  if (pendingAsk !== undefined) {
    textAsks.delete(key);
    await pendingAsk({ text });
    return;
  }
  const post: Post = (t, blocks) => client.chat.postMessage({ channel, thread_ts, text: t, blocks: blocks as never });
  if (text.toLowerCase() === "stop") {
    const { rpc, session } = await threads.get(key, `slack ${channel}/${thread_ts}`);
    await rpc.call("session.interrupt", { session });
    return;
  }
  await runTurn(key, `slack ${channel}/${thread_ts}`, text, post);
});

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, async () => {
    await threads.close();
    process.exit(0);
  });
}

await app.start();
console.log("e for Slack is listening");
