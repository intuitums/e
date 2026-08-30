#!/usr/bin/env node
/** Delegate one isolated turn to another e process over `e rpc`. Copy with
 *  scaffold.mjs. */
import { spawn } from "node:child_process";
import { connect } from "./scaffold.mjs";

const E_BIN = process.env.E_BIN || "e";

// The agents this extension offers, named for what they do. They belong to the
// extension, not to e — e core knows nothing about them. Add one by adding an
// entry: `systemPrompt` is appended to the delegated turn's system prompt,
// `tools` scopes it to those built-ins (omit for the full set), `model` is an
// optional lighter/heavier override.
const PERSONAS = [
  {
    name: "Explore",
    description:
      "Fast, read-only scout that searches and analyzes the codebase without editing.",
    tools: ["read", "grep"],
    // No model pinned: the delegation inherits the caller's model, or the
    // caller passes a lighter one per call. Never hardcode one here.
    systemPrompt:
      "You are Explore: fast, read-only reconnaissance. Find the code that matters for the task — the files, the key symbols, and how they connect — and report back concisely, quoting only the lines that carry the answer. You never edit; another turn acts on what you find. End with a dense summary the dispatching agent can use without re-reading the files.",
  },
  {
    name: "Plan",
    description:
      "Read-only strategist that gathers context and designs an implementation approach; does not edit.",
    tools: ["read", "grep"],
    systemPrompt:
      "You are Plan: a read-only strategist. Gather the context you need, then design the change as an ordered list of concrete steps — which files change, what each change is, and the order that keeps the tree building between steps. Call out the risks and the one or two decisions a human should confirm. You do not edit; you produce the plan another turn will follow. Keep it tight — a map, not an essay.",
  },
  {
    name: "Build",
    description:
      "General-purpose worker with full tool access; handles complex, multi-step tasks and file modifications.",
    systemPrompt:
      "You are Build: a general-purpose worker with the full toolset. Handle the task end to end — read what you need, make the edits, run the commands, and verify your work. When the task is done, stop.",
  },
];

const agentsByName = new Map(PERSONAS.map((p) => [p.name, p]));
const agentNames = PERSONAS.map((p) => p.name);
const agentList = `Available agents: ${PERSONAS.map((p) => `${p.name} (${p.description})`).join("; ")}.`;

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
    let persona;
    if (input.agent) {
      persona = agentsByName.get(input.agent);
      if (!persona) return { content: `unknown agent: ${input.agent}`, is_error: true };
    }
    update(`delegating${persona ? ` to ${persona.name}` : ` (${mode})`}\n`);

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
    // The extension composes the request from the persona it resolved: its
    // body appends to the system prompt (`system`), its allowlist scopes the
    // tools (`tools`), its model is a fallback the caller can override. Without
    // a persona, tool_mode picks all-tools or none. save:true persists the
    // child's session so its full transcript stays inspectable via `session`.
    const request = { id: 1, prompt: input.prompt, save: true };
    if (persona) {
      if (persona.systemPrompt) request.system = persona.systemPrompt;
      if (persona.tools) request.tools = persona.tools;
    } else {
      request.tool_mode = mode;
    }
    const chosenModel = input.model || persona?.model;
    if (chosenModel) request.model = chosenModel;
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
    const answer = result.final_output || result.output || "delegate returned no output";
    // Hand back the final answer plus a pointer to the full turn: the parent
    // reads the JSONL session only if it needs more than this summary.
    const trailer = result.session
      ? `\n\n---\nFull transcript (every tool call) at ${result.session} — read it for detail beyond this summary.`
      : "";
    return { content: answer + trailer };
  },
}).run();
