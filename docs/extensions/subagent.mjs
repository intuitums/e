#!/usr/bin/env node
/** Delegate one isolated turn to another e process over `e rpc`. Copy with
 *  scaffold.mjs. */
import { spawn } from "node:child_process";
import { connect } from "./scaffold.mjs";

const children = new Set();
function stopChildren(signal = "SIGTERM") {
  for (const child of children) {
    if (child.exitCode === null && child.signalCode === null) child.kill(signal);
  }
}
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.once(signal, () => {
    stopChildren();
    process.exit(128);
  });
}
process.once("exit", () => stopChildren());

connect({
  manifest: {
    name: "subagent",
    version: "1.0.0",
    tools: [
      {
        name: "delegate",
        description:
          "Delegate a bounded task to an isolated e turn and return its final answer.",
        parameters: {
          type: "object",
          properties: {
            prompt: { type: "string", description: "Complete task and expected result" },
            model: { type: "string", description: "Optional provider/model override" },
            effort: { type: "string", description: "Optional reasoning effort" },
            tool_mode: {
              type: "string",
              enum: ["all", "none"],
              default: "all",
              description: "'all' runs the built-in tools, 'none' answers from the prompt alone",
            },
            timeout_seconds: {
              type: "integer",
              minimum: 1,
              maximum: 290,
              default: 240,
              description: "Hard deadline for the delegated turn",
            },
          },
          required: ["prompt"],
          additionalProperties: false,
        },
      },
    ],
  },
  async tool({ arguments: input }, { update }) {
    const mode = input.tool_mode || "all";
    if (mode !== "all" && mode !== "none") {
      return { content: `invalid tool_mode: ${mode}`, is_error: true };
    }
    update(`delegating (${mode})\n`);

    // A single-shot `e rpc` child: one request line in, one response object
    // out, then EOF shuts it down. --no-extensions keeps the turn hermetic
    // and, crucially, bounds recursion — the child has no delegate tool of
    // its own, so a delegation is never a chain.
    const child = spawn(process.env.E_BIN || "e", ["rpc", "--no-extensions"], {
      cwd: process.cwd(),
      env: process.env,
      stdio: ["pipe", "pipe", "pipe"],
    });
    // Register before writing. A child that fails or exits immediately must
    // not beat the listeners and leave this call waiting forever.
    const exited = new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("exit", (code, signal) => resolve({ code, signal }));
    });
    children.add(child);
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
      update(chunk, "stderr");
    });
    // The child may exit before we finish writing; swallow the EPIPE rather
    // than crash the extension.
    child.stdin.on("error", () => {});
    const request = { id: 1, prompt: input.prompt, save: false, tool_mode: mode };
    if (input.model) request.model = input.model;
    if (input.effort) request.effort = input.effort;
    child.stdin.write(`${JSON.stringify(request)}\n`);
    child.stdin.end();

    const deadline = Math.min(290, Math.max(1, Number(input.timeout_seconds) || 240));
    let forceKill;
    const timeout = setTimeout(() => {
      timedOut = true;
      child.kill("SIGTERM");
      forceKill = setTimeout(() => child.kill("SIGKILL"), 2_000);
      forceKill.unref();
    }, deadline * 1_000);
    timeout.unref();
    let status;
    try {
      status = await exited;
    } finally {
      clearTimeout(timeout);
      clearTimeout(forceKill);
      children.delete(child);
    }
    if (timedOut) {
      return { content: `delegate exceeded its ${deadline}s deadline`, is_error: true };
    }
    // Exactly one response line for our one request; take the first non-empty.
    const line = stdout
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l.length > 0);
    let result;
    try {
      result = JSON.parse(line);
    } catch {
      return {
        content: `delegate returned invalid JSON (${status.code ?? status.signal}): ${stderr || stdout}`,
        is_error: true,
      };
    }
    if (result.error || status.code !== 0) {
      return { content: result.error || stderr || "delegate failed", is_error: true };
    }
    return { content: result.final_output || result.output || "delegate returned no output" };
  },
}).run();
