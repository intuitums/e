#!/usr/bin/env node
/** gate — the tool_call hook as a guard, in the fail-open shape e expects:
 *  only an explicit block stops a call; anything else — including a slow or
 *  crashed extension — lets the tool through. Denies a few clearly
 *  destructive bash patterns while allowing everything else.
 *
 * Copy scaffold.mjs + gate.mjs into ~/.e/extensions/ (chmod +x) and restart
 * e, then ask the model to `rm -rf` something and watch it be refused.
 */

import { connect } from "./scaffold.mjs";

const DENIED = [
  /rm\s+(-[a-z]*f|\/)/,      // rm -f… / rm …/…
  /mkfs/,                     // disk formats
  /:\s*,?\s*shutdown\b/i,     // remote shutdown
];

connect({
  manifest: {
    name: "gate",
    version: "1.0",
    description: "blocks clearly destructive bash patterns (example)",
    hooks: ["tool_call"],
  },
  hookToolCall({ name, arguments: args }) {
    const haystack =
      typeof args === "string" ? args : JSON.stringify(args ?? {});
    for (const pattern of DENIED) {
      if (pattern.test(haystack)) {
        return { block: true, reason: `denied by gate: ${pattern}` };
      }
    }
    return { block: false };
  },
}).run();