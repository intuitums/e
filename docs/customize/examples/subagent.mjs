#!/usr/bin/env node
/** Delegate one isolated turn to another e process over `e rpc`. A
 *  self-contained extension: it speaks e's JSONL protocol directly (see the
 *  loop at the bottom), so it is a single file with nothing to install beside
 *  it. */
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

const E_BIN = process.env.E_BIN || "e";

// The agents this extension offers. Each is a tool/model envelope, not a
// character: a delegated turn runs e's ordinary prompt and is shaped only by
// which tools it may use and which model runs it. `tools` is the built-in
// allowlist (omit for the full set); `model` selects the model.
const AGENTS = [
  {
    name: "Explore",
    description: "Read-only recon: searches and analyzes the codebase without editing.",
    tools: ["read", "grep"],
    model: "{provider/model}", // ask the user what models to use
  },
  {
    name: "Plan",
    description: "Read-only planning: gathers context and lays out an implementation approach.",
    tools: ["read", "grep"],
    model: "{provider/model}", // ask the user what models to use
  },
  {
    name: "Build",
    description: "Full access: makes edits and runs commands to carry a task to completion.",
    model: "{provider/model}", // ask the user what models to use
  },
];

const agentsByName = new Map(AGENTS.map((a) => [a.name, a]));
const agentNames = AGENTS.map((a) => a.name);
const agentList = `Available agents: ${AGENTS.map((a) => `${a.name} (${a.description})`).join("; ")}.`;

// A model counts only once the {provider/model} filler is replaced with a real
// slug. Otherwise the child uses e rpc's normal model resolution.
const realModel = (model) => (model && !model.includes("{") ? model : undefined);

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

const MANIFEST = {
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
            description: "Optional agent: its tool access and model shape the turn",
          },
          model: { type: "string", description: "Optional provider/model override" },
          effort: { type: "string", description: "Optional reasoning effort" },
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
};

async function runDelegate(input, update) {
    let agent;
    if (input.agent) {
      agent = agentsByName.get(input.agent);
      if (!agent) return { content: `unknown agent: ${input.agent}`, is_error: true };
    }
    update(`delegating${agent ? ` to ${agent.name}` : ""}\n`);

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
    // An agent selects tools and a model. Omitting tools grants the full set,
    // and a model argument overrides the agent's model. The turn uses e's
    // ordinary system prompt. save:true keeps the full transcript available
    // through `session`.
    const request = { id: 1, prompt: input.prompt, save: true };
    if (agent?.tools) request.tools = agent.tools;
    const model = realModel(input.model) ?? realModel(agent?.model);
    if (model) request.model = model;
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
      ? `\n\n---\nFull transcript with every tool call: ${result.session}\nRead it for detail beyond this summary.`
      : "";
    return { content: answer + trailer };
}

// e's extension protocol: one JSON request per line on stdin, one response per
// line on stdout. We answer `initialize` with the manifest, run `delegate` for
// `tool_call` (streaming progress with `tool.update`), and exit on `shutdown`.
// Ignore methods this extension does not implement.
function send(object) {
  process.stdout.write(`${JSON.stringify(object)}\n`);
}
createInterface({ input: process.stdin }).on("line", (line) => {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    return;
  }
  const { id, method, params } = request;
  if (method === "initialize") {
    send({ id, result: MANIFEST });
  } else if (method === "shutdown") {
    process.exit(0);
  } else if (method === "tool_call") {
    const update = (chunk, stream = "stdout") => {
      if (chunk === undefined || chunk === null) return;
      send({ method: "tool.update", params: { id, stream, chunk: String(chunk) } });
    };
    Promise.resolve()
      .then(() => runDelegate(params?.arguments ?? {}, update))
      .then(
        (result) => send({ id, result: result ?? {} }),
        (error) => send({ id, error: error instanceof Error ? error.message : String(error) })
      );
  }
});
