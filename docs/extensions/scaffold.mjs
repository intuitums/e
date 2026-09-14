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
 *   const ext = connect({ manifest, ...handlers });
 *   ext.run();
 *
 * Handlers (each optional; returning undefined means "nothing to say").
 * Handlers receive e's `params` object as sent — command/tool get
 * `{name, args}` / `{name, arguments}`, hooks get their own params.
 *
 *   initialize(params)     — stash config ({extensions_config}), before the
 *                            manifest is answered; params.ui says whether a
 *                            person can answer ui.* requests
 *   startup({cwd, argv, flags}) — {"argv": […], "env": {"K": "v"|null},
 *                            "relaunch": {"cwd": …}}. `flags` are the
 *                            parsed values of your typed flag declarations
 *                            (see flag() below).
 *   command({name, args})  — {"notice": …} | {"show": {…}} | {"prompt": …}
 *                            | {"session_name": …}
 *   shortcut({key})        — same result shape as a command
 *   complete({name, prefix}) — {"items": [{value, label?, description?}]} for
 *                            a command declared with "completions": true
 *   tool({name, arguments}, {update}) — {"content", "is_error"?,
 *                            "summary"?, "display"?, "format"?}; `update(chunk,
 *                            stream?)` streams stdout/stderr progress first
 *   hookToolCall({name, arguments}) — {"block": true, "reason": …} | {"block": false}
 *   hookInput({text})      — {"consume": true} | {"replace": …} | {"notice": …} | {}
 *   beforeTurn({prompt})   — {"system_suffix": …, "message": {content, internal}}
 *   toolResult({name, content, is_error}) — {"content": …} | {}
 *   compactSummary({summary}) — {"summary": …} | {}
 *   render({kind, name, content}) — {"body": …, "format": …} | {}  (manifest `renders`)
 *   event({name, extra})   — a subscribed lifecycle event (manifest `events`)
 *   key({key})             — a key while your interactive panel is open
 *   panelClosed()          — the user (or another panel) closed yours
 *   paneSelect({pane, section, id})   — the side pane's cursor moved to an item
 *   paneActivate({pane, section, id}) — Enter on a pane item
 *   paneKey({pane, key})   — a pane chord e did not use
 *   paneClosed({pane})     — the user (or another pane) closed yours
 *
 * Asking e — every call returns a promise of the result, rejected with
 * e's error text (for instance "no ui" under `e rpc`):
 *
 *   ext.ui.notify(message, tone?)          ext.ui.show({title, body, format})
 *   ext.ui.select(title, options)          ext.ui.confirm(title, message?)
 *   ext.ui.input(title, {placeholder, prefill, secret}?)
 *   ext.ui.editor(title, text?)            a multi-line answer
 *   ext.ui.status(text | null, key?)       ext.ui.compose(text)
 *   ext.ui.activity(text | null, key?)     the row below the transcript
 *   ext.ui.panel({title, lines, interactive}) / ext.ui.panel(null)
 *   ext.ui.widget(lines | null, key?)      ext.ui.pane({id, title, side, sections}) / ext.ui.pane(null)
 *   ext.session.send(content, {internal, run}?)   ext.session.info()
 *   ext.session.name(name)   .model(m)   .effort(l)   .tools(names | null)
 *   ext.session.interrupt()  .compact(focus?)
 *   ext.hasUI                              true once initialize said so
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
 * "description"?, "tools"?[], "commands"?[], "flags"?[], "hooks"?[],
 * "events"?[], "shortcuts"?[]}.
 */
export function connect({ manifest = {}, ...handlers } = {}) {
  const rl = createInterface({ input: process.stdin });

  function send(obj) {
    process.stdout.write(JSON.stringify(obj) + "\n");
  }
  function reply(id, result) {
    send({ id, result });
  }
  function fail(id, error) {
    send({ id, error: error instanceof Error ? error.message : String(error) });
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

  // ---- our own requests to e ------------------------------------------
  let nextId = 0;
  const pending = new Map();
  /** Ask e; resolves with the result, rejects with e's error text. */
  function ask(method, params = {}) {
    const id = `s${++nextId}`;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      send({ id, method, params });
    });
  }
  function settle(id, result, error) {
    const waiter = pending.get(id);
    if (!waiter) return false;
    pending.delete(id);
    if (error !== undefined) waiter.reject(new Error(String(error)));
    else waiter.resolve(result);
    return true;
  }

  const api = {
    hasUI: false,
    ui: {
      notify: (message, tone) => ask("ui.notify", tone ? { message, tone } : { message }),
      show: (block) => ask("ui.show", block),
      select: (title, options) => ask("ui.select", { title, options }),
      confirm: (title, message) => ask("ui.confirm", message ? { title, message } : { title }),
      input: (title, options = {}) => ask("ui.input", { title, ...options }),
      editor: (title, text) => ask("ui.editor", text === undefined ? { title } : { title, text }),
      status: (text, key) => ask("ui.status", key === undefined ? { text } : { text, key }),
      activity: (text, key) => ask("ui.activity", key === undefined ? { text } : { text, key }),
      widget: (lines, key) => ask("ui.widget", key === undefined ? { lines } : { lines, key }),
      pane: (pane) => ask("ui.pane", pane === null || pane === undefined ? null : pane),
      compose: (text) => ask("ui.compose", { text }),
      panel: (panel) => ask("ui.panel", panel === null || panel === undefined ? null : panel),
    },
    session: {
      send: (content, options = {}) => ask("session.send", { content, ...options }),
      info: () => ask("session.info"),
      name: (name) => ask("session.name", { name }),
      model: (model) => ask("session.model", { model }),
      effort: (effort) => ask("session.effort", { effort }),
      tools: (names) => ask("session.tools", { names }),
      interrupt: () => ask("session.interrupt"),
      compact: (focus) => ask("session.compact", focus === undefined ? {} : { focus }),
    },
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
        let message;
        try {
          message = JSON.parse(line);
        } catch {
          return;
        }
        route(message);
      });
    },
  };

  /** Run a notification handler; a notification has no reply, so a
   *  throwing or rejecting observer is its own problem, never a crash. */
  function observe(handler, params) {
    if (typeof handler !== "function") return;
    try {
      Promise.resolve(handler(params)).catch(() => {});
    } catch {
      // as above
    }
  }

  function route(message) {
    const { id, method, params } = message;
    // A reply to one of our own requests: an id we issued, no method.
    if (method === undefined && id !== undefined && settle(id, message.result, message.error)) {
      return;
    }
    switch (method) {
      case "initialize":
        api.hasUI = Boolean(params && params.ui);
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
        observe(handlers.event, params || {});
        return;
      case "ui.key":
        observe(handlers.key, params || {});
        return;
      case "ui.panel_closed":
        observe(handlers.panelClosed);
        return;
      case "pane.select":
        observe(handlers.paneSelect, params || {});
        return;
      case "pane.activate":
        observe(handlers.paneActivate, params || {});
        return;
      case "pane.key":
        observe(handlers.paneKey, params || {});
        return;
      case "pane.closed":
        observe(handlers.paneClosed, params || {});
        return;
      default:
        break;
    }
    const handler = {
      "hook.startup": (params) => {
        if (params && typeof params.flags === "object") lastFlags = params.flags;
        return handlers.startup ? handlers.startup(params) : undefined;
      },
      command: handlers.command,
      shortcut: handlers.shortcut,
      "command.complete": handlers.complete,
      tool_call: handlers.tool,
      "hook.tool_call": handlers.hookToolCall,
      "hook.input": handlers.hookInput,
      "hook.before_turn": handlers.beforeTurn,
      "hook.tool_result": handlers.toolResult,
      "hook.compact_summary": handlers.compactSummary,
      "hook.render": handlers.render,
    }[method];
    if (typeof handler !== "function") return; // not ours; stay quiet
    try {
      const context = {
        update(chunk, stream = "stdout") {
          if (method !== "tool_call" || chunk === undefined || chunk === null) return;
          send({ method: "tool.update", params: { id, stream, chunk: String(chunk) } });
        },
      };
      answer(id, handler(params, context));
    } catch (error) {
      fail(id, error);
    }
  }

  return api;
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
