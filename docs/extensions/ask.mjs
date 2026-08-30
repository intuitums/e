#!/usr/bin/env node
/**
 * ask — the question tool, as an extension.
 *
 * Adds one `ask` tool: the model asks the person at the keyboard a
 * question and waits for the answer. It is built entirely on the
 * `input.request` notification — e shows the question panel, and the
 * person's answer (or dismissal) comes back as an `input.reply` request
 * with the same id. Nothing here is special-cased in e; this is the
 * reference implementation of the surface any extension can use.
 *
 * Copy next to scaffold.mjs is NOT required — this one speaks the wire
 * directly and imports nothing. Make it executable, restart e.
 */

import { createInterface } from "node:readline";

let nextId = 0;
// input.request id → the function that resolves that tool call.
const waiting = new Map();

function send(message) {
  process.stdout.write(JSON.stringify(message) + "\n");
}

const rl = createInterface({ input: process.stdin });
rl.on("line", (line) => {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    return;
  }
  const { id, method, params } = request;
  switch (method) {
    case "initialize":
      send({
        id,
        result: {
          name: "ask",
          version: "1.0",
          description: "the question tool",
          tools: [
            {
              name: "ask",
              description:
                "Ask the user one question and wait for their answer. Use it when you " +
                "genuinely need a decision you cannot make from context — a choice " +
                "between real alternatives, missing information only they have. Offer " +
                "2-4 short options when the answers are enumerable; the user can " +
                "always type a freeform answer unless allow_freeform is false.",
              parameters: {
                type: "object",
                properties: {
                  question: { type: "string", description: "The question to ask, one sentence" },
                  options: {
                    type: "array",
                    description: "Choices to offer, in order",
                    items: {
                      type: "object",
                      properties: {
                        label: { type: "string", description: "Short answer text, returned verbatim when chosen" },
                        description: { type: "string", description: "One-line explanation of the choice" },
                      },
                      required: ["label"],
                    },
                  },
                  allow_freeform: { type: "boolean", description: "Allow a typed answer besides the options (default true)" },
                },
                required: ["question"],
              },
            },
          ],
        },
      });
      return;
    case "tool_call":
      if (params.name !== "ask") return;
      ask(params.arguments).then(
        (answer) => send({ id, result: { content: answer } }),
        (dismissed) =>
          send({
            id,
            result: {
              content: dismissed,
              is_error: true,
            },
          })
      );
      return;
    case "input.reply":
      // The person answered (params.answer) or dismissed (null). Resolve
      // whichever tool call asked; a reply for a forgotten id is ignored.
      if (params && waiting.has(params.id)) {
        waiting.get(params.id)(params.answer ?? null);
        waiting.delete(params.id);
      }
      return;
    case "shutdown":
      process.exit(0);
      return;
    default:
      return; // flags, events, hooks we don't use
  }
});

/**
 * Send the input.request and wait. Resolves with the answer text;
 * rejects with a message when the person dismissed the panel.
 */
function ask(args = {}) {
  const question = String(args.question ?? "").trim();
  if (!question) {
    return Promise.reject("ask: missing question");
  }
  const options = (args.options ?? [])
    .map((o) => ({ label: String(o?.label ?? "").trim(), description: String(o?.description ?? "").trim() }))
    .filter((o) => o.label);
  const freeform = args.allow_freeform !== false || options.length === 0;
  if (options.length === 0 && !freeform) {
    return Promise.reject("ask: no way to answer — add options or allow freeform");
  }
  const requestId = `q${++nextId}`;
  return new Promise((resolve, reject) => {
    waiting.set(requestId, (answer) => {
      if (answer === null) reject("The user dismissed the question without answering.");
      else resolve(answer);
    });
    send({
      method: "input.request",
      params: { id: requestId, question, options, freeform },
    });
  });
}
