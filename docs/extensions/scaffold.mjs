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
 *   startup({cwd, argv, flags}) — {"argv": […], "env": {"K": "v"|null},
 *                            "relaunch": {"cwd": …}}. `flags` are the
 *                            parsed values of your typed flag declarations
 *                            (see flag() below).
 *   command({name, args})  — {"notice": …} | {"prompt": …} | {"session_name": …}
 *   tool({name, arguments})— {"content": …, "is_error"?: bool, "session_name"?: …}
 *   hookToolCall({name, arguments}) — {"block": true, "reason": …} | {"block": false}
 *   hookInput({text})      — {"consume": true} | {"replace": …} | {"notice": …} | {}
 *
 * `flag(name)` (pi's getFlag) reads a parsed flag from any handler, any
 * time: a passed value, else the flag's `default` in the manifest, else
 * undefined. `flagPassed(name)` is true only when it was on the command
 * line. Flags arrive as a `flags` notification at startup — no startup
 * hook needed to read them.
 *
 * A thrown error is answered as a protocol error (startup errors are fatal
 * to launch, as e documents; runtime errors just fail that call).
 *
 * Run directly (dropped into ~/.e/extensions/, which users do by accident
 * since examples import it from there), this file answers initialize with
 * a minimal manifest and idles — a silent, harmless no-op extension.
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

  // Flags e parsed from the command line ("flags" notification; also rides
  // hook.startup params). flag()/flagPassed() read them — the pi getFlag
  // analogs, available in any handler, not just at startup.
  let lastFlags = {};

  // The manifest's declared defaults, per flag name.
  const defaults = {};
  for (const flag of manifest.flags || []) {
    if (Object.hasOwn(flag, "default")) defaults[flag.name] = flag.default;
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
      case "flags":
        // Notification: parsed flags, no reply expected.
        if (params && typeof params.flags === "object") lastFlags = params.flags;
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
      "hook.startup": (params) => {
        if (params && typeof params.flags === "object") lastFlags = params.flags;
        return handlers.startup ? handlers.startup(params) : undefined;
      },
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
    /** pi's getFlag: the parsed value of a typed flag in any handler —
     *  the passed value, else the manifest default, else undefined. Works
     *  from any handler, no startup hook needed. */
    flag(name) {
      return Object.hasOwn(lastFlags, name) ? lastFlags[name] : defaults[name];
    },
    /** True only when the flag was actually on the command line (passed),
     *  regardless of its default. */
    flagPassed(name) {
      return Object.hasOwn(lastFlags, name);
    },
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
// ---- direct-run no-op -----------------------------------------------------
// When executed (rather than imported), serve a minimal manifest so e sees
// a quiet, well-behaved extension instead of a startup failure.

import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

function invokedDirectly() {
  try {
    if (!process.argv[1]) return false;
    return (
      realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url))
    );
  } catch {
    return false;
  }
}

if (invokedDirectly()) {
  const write = (id, result) =>
    process.stdout.write(JSON.stringify({ id, result }) + "\n");
  createInterface({ input: process.stdin }).on("line", (line) => {
    let request;
    try {
      request = JSON.parse(line);
    } catch {
      return;
    }
    switch (request.method) {
      case "initialize":
        write(request.id, {
          name: "scaffold",
          version: "1.0",
          description: "library — import connect(), don't run me",
        });
        break;
      case "shutdown":
        process.exit(0);
    }
  });
}
