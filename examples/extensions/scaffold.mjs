#!/usr/bin/env node
/**
 * scaffold — the wire-protocol helper for e extensions.
 *
 * An e extension is a bare process speaking JSONL over stdin/stdout; the
 * framing (id routing, the initialize manifest, dispatch) is the same for
 * every extension. This file is that shared plumbing: `connect()` turns
 * your handlers into a running extension, so an extension reads like pi's
 * SDK (handlers in, protocol out) without importing anything but node.
 *
 * Copy this file next to your own extension and:
 *
 *   import { connect } from "./scaffold.mjs";
 *
 * Handlers (each optional; returning undefined means "nothing to say").
 * Handlers receive e's `params` object as sent — command/tool get
 * `{name, args}` / `{name, arguments}`, hooks get their own params.
 *
 *   initialize(params)     — stash config ({extensions_config}), before the
 *                            manifest is answered
 *   startup({cwd, argv})   — {"argv": […], "env": {"K": "v"|null}, "relaunch": {"cwd": …}}
 *   command({name, args})  — {"notice": …} | {"prompt": …} | {"session_name": …}
 *   tool({name, arguments})— {"content": …, "is_error"?: bool, "session_name"?: …}
 *   hookToolCall({name, arguments}) — {"block": true, "reason": …} | {"block": false}
 *   hookInput({text})      — {"consume": true} | {"replace": …} | {"notice": …} | {}
 *
 * A thrown error is answered as a protocol error (startup errors are fatal
 * to launch, as e documents; runtime errors just fail that call). If this
 * file is dropped into ~/.e/extensions/ by accident it answers initialize
 * with a minimal manifest and idles — a harmless no-op extension.
 */

import { createInterface } from "node:readline";

/**
 * Build an extension from a manifest plus handlers; call `.run()` to start.
 * `manifest` is the initialize result minus the id: {"name", "version",
 * "description"?, "tools"?[], "commands"?[], "flags"?[], "hooks"?[]}.
 */
export function connect({ manifest = {}, ...handlers } = {}) {
  const rl = createInterface({ input: process.stdin });

  function reply(id, result) {
    process.stdout.write(JSON.stringify({ id, result }) + "\n");
  }
  function fail(id, error) {
    process.stdout.write(
      JSON.stringify({ id, error: error instanceof Error ? error.message : String(error) }) + "\n"
    );
  }
  /** Await a handler result (sync or promise) and answer with it. */
  function answer(id, result) {
    if (result && typeof result.then === "function") {
      result.then(
        (value) => reply(id, value === undefined ? {} : value),
        (error) => fail(id, error)
      );
    } else {
      reply(id, result === undefined ? {} : result);
    }
  }

  function route(request) {
    const { id, method, params } = request;
    switch (method) {
      case "initialize":
        try {
          if (typeof handlers.initialize === "function") handlers.initialize(params);
        } catch (error) {
          fail(id, error);
          return;
        }
        reply(id, { name: "scaffold", version: "1.0", ...manifest });
        return;
      case "shutdown":
        process.exit(0);
        return;
      case "event":
        return; // a notification; never answered
      default:
        break;
    }
    const handler = {
      "hook.startup": handlers.startup,
      "command": handlers.command,
      "tool_call": handlers.tool,
      "hook.tool_call": handlers.hookToolCall,
      "hook.input": handlers.hookInput,
    }[method];
    if (typeof handler !== "function") return; // not ours; stay quiet
    try {
      answer(id, handler(params));
    } catch (error) {
      fail(id, error);
    }
  }

  return {
    run() {
      rl.on("line", (line) => {
        let request;
        try {
          request = JSON.parse(line);
        } catch {
          return;
        }
        route(request);
      });
    },
  };
}