#!/usr/bin/env node
/** Delegate one isolated turn to another e process. Copy with scaffold.mjs. */
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
          "Delegate a bounded task to an isolated e turn and return its final answer. Read-only by default.",
        parameters: {
          type: "object",
          properties: {
            prompt: { type: "string", description: "Complete task and expected result" },
            model: { type: "string", description: "Optional provider/model override" },
            effort: { type: "string", description: "Optional reasoning effort" },
            tool_mode: {
              type: "string",
              enum: ["read_only", "all", "none"],
              default: "read_only",
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
    const mode = input.tool_mode || "read_only";
    const modeFlag = { read_only: "--read-only", all: null, none: "--no-tools" }[mode];
    if (modeFlag === undefined) return { content: `invalid tool_mode: ${mode}`, is_error: true };
    const args = ["--no-extensions", "--no-save", "--json"];
    if (modeFlag) args.push(modeFlag);
    if (input.model) args.push("--model", input.model);
    if (input.effort) args.push("--effort", input.effort);
    args.push("ask", "--", input.prompt);
    update(`delegating (${mode})\n`);

    const child = spawn(process.env.E_BIN || "e", args, {
      cwd: process.cwd(),
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
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
      status = await new Promise((resolve, reject) => {
        child.once("error", reject);
        child.once("exit", (code, signal) => resolve({ code, signal }));
      });
    } finally {
      clearTimeout(timeout);
      clearTimeout(forceKill);
      children.delete(child);
    }
    if (timedOut) {
      return { content: `delegate exceeded its ${deadline}s deadline`, is_error: true };
    }
    let result;
    try {
      result = JSON.parse(stdout.trim());
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
