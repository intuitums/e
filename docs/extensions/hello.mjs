#!/usr/bin/env node
/** hello — every extension surface at once, built on the scaffold.
 *
 * Copy scaffold.mjs + hello.mjs into ~/.e/extensions/ (chmod +x) and
 * restart e:
 *
 *   /hello            — a notice from the command
 *   /hello world      — names the session "world" and echoes a greeting
 *   type: magical     — the input hook rewrites it to "the magic words"
 *   ask the model to call say_hello
 *
 * Config lives in ~/.e/settings.json under the extension's own name:
 *   {"extensions":{"hello":{"name":"friend"}}}
 */

import { connect } from "./scaffold.mjs";

let greeting = "hello, there";

connect({
  manifest: {
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
  },
  initialize(params) {
    const config = (params && params.extensions_config) || {};
    greeting = `hello, ${(config.hello && config.hello.name) || "there"}`;
  },
  command({ name, args }) {
    const rest = (args || "").trim();
    if (!rest) return { notice: greeting };
    return { notice: `${greeting}, ${rest}`, session_name: rest };
  },
  tool({ name, arguments: args }) {
    if (name !== "say_hello") return { content: `unknown tool ${name}`, is_error: true };
    return { content: greeting };
  },
  hookInput({ text }) {
    if (text.includes("magical")) return { replace: "the magic words" };
    if (text === "shhh") return { consume: true, notice: "swallowed by hello" };
    return {};
  },
}).run();