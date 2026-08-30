#!/usr/bin/env node
/** Delegate one isolated turn to another e process over `e rpc`. Copy with
 *  scaffold.mjs. */
import { execFileSync, spawn } from "node:child_process";
import { connect } from "./scaffold.mjs";

const E_BIN = process.env.E_BIN || "e";

// Discover the personas e offers here (trust-scoped in core, so a project's
// own `.e/agents/` only appears in a trusted directory). Done once at startup;
// restart e to pick up new agent files, the same as skills.
function discoverAgents() {
  try {
    const raw = execFileSync(E_BIN, ["agents", "--json"], { encoding: "utf8" });
    const agents = JSON.parse(raw);
    return Array.isArray(agents) ? agents : [];
  } catch {
    return [];
  }
}

const agents = discoverAgents();
const agentNames = agents.map((a) => a.name);
const agentList = agents.length
  ? `Available agents: ${agents.map((a) => `${a.name} (${a.description})`).join("; ")}.`
  : "No agents are defined; omit `agent` to run a plain delegated turn.";

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
        description: `Delegate a bounded task to an isolated e turn and return its final answer. ${agentList}`,
        parameters: {
          type: "object",
          properties: {
            prompt: { type: "string", description: "Complete task and expected result" },
            agent: {
              type: "string",
              ...(agentNames.length ? { enum: agentNames } : {}),
              description:
                "Optional persona: its system prompt, tool allowlist, and model shape the turn",
            },
            model: { type: "string", description: "Optional provider/model override" },
            effort: { type: "string", description: "Optional reasoning effort" },
            tool_mode: {
              type: "string",
              enum: ["all", "none"],
              default: "all",
              description:
                "Ignored when `agent` sets a tool allowlist; otherwise 'all' runs the built-in tools, 'none' answers from the prompt alone",
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
    update(`delegating${input.agent ? ` to ${input.agent}` : ` (${mode})`}\n`);

    // A single-shot `e rpc` child: one request line in, one response object
    // out, then EOF shuts it down. --no-extensions keeps the turn hermetic
    // and, crucially, bounds recursion — the child has no delegate tool of
    // its own, so a delegation is never a chain.
    const child = spawn(E_BIN, ["rpc", "--no-extensions"], {
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
    // `agent` names a persona; e resolves its system prompt, tool allowlist,
    // and model. tool_mode only applies to a persona-less delegation.
    const request = { id: 1, prompt: input.prompt, save: false };
    if (input.agent) request.agent = input.agent;
    else request.tool_mode = mode;
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
