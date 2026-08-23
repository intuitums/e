#!/usr/bin/env node
/**
 * hello — a small but complete e extension in JavaScript.
 *
 * Shows every surface at once: a slash command, a tool, the input hook,
 * session naming, and per-extension config. Copy it to ~/.e/extensions/
 * (chmod +x) and restart e:
 *
 *   /hello            — a notice from the command
 *   /hello world      — names the session "world" and replies "hello world"
 *   type: magical     — the input hook rewrites it to "the magic words"
 *   say "x" to the model and let it call the bye tool
 *
 * Config lives in ~/.e/settings.json under the extension's own name:
 *   {"extensions":{"hello":{"name":"friend"}}}
 */

import { createInterface } from "node:readline";

const rl = createInterface({ input: process.stdin });

function reply(id, result) {
  process.stdout.write(JSON.stringify({ id, result }) + "\n");
}
function fail(id, message) {
  process.stdout.write(JSON.stringify({ id, error: message }) + "\n");
}

rl.on("line", (line) => {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    return;
  }
  switch (request.method) {
    case "initialize": {
      const config = request.params.extensions_config || {};
      const name = (config.hello && config.hello.name) || "there";
      // Remember it for later calls — the tool uses it too.
      greeting = `hello, ${name}`;
      reply(request.id, {
        name: "hello",
        version: "1.0",
        description: "a small example extension",
        commands: [{ name: "hello", description: "greet (optionally: /hello <name>)" }],
        tools: [
          {
            name: "say_hello",
            description: "return a greeting",
            parameters: { type: "object", properties: {} },
          },
        ],
        hooks: ["input"],
      });
      break;
    }
    case "command":
      if (request.params.name === "hello") {
        const rest = (request.params.args || "").trim();
        if (rest) {
          // /hello world → name the session and echo a greeting.
          reply(request.id, {
            notice: `${greeting}, ${rest}`,
            session_name: rest,
          });
        } else {
          reply(request.id, { notice: greeting });
        }
      } else {
        fail(request.id, `unknown command ${request.params.name}`);
      }
      break;
    case "hook.input":
      // A tiny rewrite: the input hook can consume or replace a line.
      if (request.params.text.includes("magical")) {
        reply(request.id, { replace: "the magic words" });
      } else if (request.params.text === "shhh") {
        reply(request.id, { consume: true, notice: "swallowed by hello" });
      } else {
        reply(request.id, {});
      }
      break;
    case "tool_call":
      if (request.params.name === "say_hello") {
        // Tools can name the session too.
        reply(request.id, { content: greeting, session_name: "hello-session" });
      } else {
        fail(request.id, `unknown tool ${request.params.name}`);
      }
      break;
    case "shutdown":
      process.exit(0);
  }
});

let greeting = "hello, there";