#!/usr/bin/env node
/** Delegate one isolated turn to another e process over `e rpc`. Copy with
 *  scaffold.mjs. */
import { spawn } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { connect } from "./scaffold.mjs";

const E_BIN = process.env.E_BIN || "e";

// Personas belong to this extension, not to e. Each is a markdown file in
// ~/.e/agents/ with frontmatter (name/description/tools/model) and a body that
// becomes the delegated turn's appended system prompt. e core knows nothing
// about them — the extension reads the files and composes the `e rpc` request.
// Discovered once at startup; restart to pick up new files, the same as skills.
const AGENTS_DIR = join(process.env.E_HOME || join(homedir(), ".e"), "agents");

function parseFrontmatter(text) {
  const match = text.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/);
  if (!match) return { fm: {}, body: text };
  const fm = {};
  for (const line of match[1].split("\n")) {
    const i = line.indexOf(":");
    if (i !== -1) fm[line.slice(0, i).trim()] = line.slice(i + 1).trim();
  }
  return { fm, body: match[2] };
}

// `tools: read, grep` or `tools: [read, grep]` → ["read","grep"]; empty → undefined.
function parseToolList(value) {
  if (!value) return undefined;
  const tools = value
    .replace(/^\[|\]$/g, "")
    .split(",")
    .map((t) => t.trim().replace(/^["']|["']$/g, ""))
    .filter(Boolean);
  return tools.length ? tools : undefined;
}

function discoverAgents() {
  let files;
  try {
    files = readdirSync(AGENTS_DIR).filter((f) => f.endsWith(".md"));
  } catch {
    return [];
  }
  const agents = [];
  for (const file of files) {
    let text;
    try {
      text = readFileSync(join(AGENTS_DIR, file), "utf8");
    } catch {
      continue;
    }
    const { fm, body } = parseFrontmatter(text);
    agents.push({
      name: fm.name || file.replace(/\.md$/, ""),
      description: fm.description || "",
      tools: parseToolList(fm.tools),
      model: fm.model || undefined,
      systemPrompt: body.trim(),
    });
  }
  return agents;
}

const agents = discoverAgents();
const agentsByName = new Map(agents.map((a) => [a.name, a]));
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
