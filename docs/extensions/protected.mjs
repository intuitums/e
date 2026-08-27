#!/usr/bin/env node
/** protected — the tool_call hook as a guard over sensitive paths: denies
 *  any tool call (read, write, edit, grep, bash) whose `path`, `glob`
 *  (grep's file filter), or bash `command` names a credential-shaped
 *  path — `~/.ssh`, `~/.aws`, `~/.gnupg`, `.env*`, `*.pem`, `*.key`.
 *  Complements gate.mjs's destructive-command denylist: this one is
 *  about what gets *read into context* or *written to disk*, not just
 *  what bash runs. Fail-open like gate.mjs: only an explicit block
 *  stops a call.
 *
 * Not a real boundary: this only sees the call's arguments, not which
 * files it actually touches. `grep({path: ".", pattern: "..."})` with no
 * `glob` searches every file under `.` — nothing here (or anything a
 * tool_call hook could see) stops it from returning a denied file's
 * contents if the pattern happens to match one. The `glob` check below
 * closes the case where the denied file is named explicitly; it doesn't
 * close that one. See docs/sandboxing.md for why a hook is a speed bump,
 * not an isolation boundary.
 *
 * Copy scaffold.mjs + protected.mjs into ~/.e/extensions/ (chmod +x) and
 * restart e, then ask the model to read ~/.ssh/id_rsa and watch it be
 * refused.
 */

import { connect } from "./scaffold.mjs";

// Only the trailing boundary is checked, deliberately: `path` args are
// clean paths (`$` ends the string), `command` is free-form bash text
// where the segment is instead followed by a space/quote/operator, and
// `glob` is a glob pattern where it can be preceded by `*` or `**/`
// (`*.key`, `**/*.env`). A leading-boundary check would miss those glob
// forms, so all three are matched the same way: the segment followed by
// `(\/|$|[^\w.-])`. This trades a few false positives (a filename that
// merely contains one of these as a substring) for not missing a glob
// wildcard — the safer direction for a denylist.
const DENIED = [
  /\.ssh(\/|$|[^\w.-])/,
  /\.aws(\/|$|[^\w.-])/,
  /\.gnupg(\/|$|[^\w.-])/,
  /\.env(\.[\w-]*)?(\/|$|[^\w.-])/,
  /\.pem(\/|$|[^\w.-])/,
  /\.key(\/|$|[^\w.-])/,
];

function matches(text) {
  return typeof text === "string" && DENIED.some((pattern) => pattern.test(text));
}

connect({
  manifest: {
    name: "protected",
    version: "1.0",
    description: "blocks tool calls that touch credential-shaped paths (example)",
    hooks: ["tool_call"],
  },
  hookToolCall({ name, arguments: args }) {
    const path = args && typeof args === "object" ? args.path : undefined;
    const glob = args && typeof args === "object" ? args.glob : undefined;
    if (matches(path)) {
      return { block: true, reason: `denied by protected: sensitive path ${path}` };
    }
    if (matches(glob)) {
      return { block: true, reason: `denied by protected: sensitive glob ${glob}` };
    }
    if (name === "bash" && matches(args?.command)) {
      return { block: true, reason: "denied by protected: sensitive path in command" };
    }
    return { block: false };
  },
}).run();
