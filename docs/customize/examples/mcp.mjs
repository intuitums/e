#!/usr/bin/env node
/**
 * MCP stdio tool bridge for e. Configure `extensions.mcp` in settings.json:
 *   {"command":"npx","args":["-y","@modelcontextprotocol/server-filesystem","/safe/root"]}
 *
 * This targets the stable 2025-11-25 stdio lifecycle used by current SDKs'
 * legacy/default mode: newline-delimited JSON-RPC, initialize/initialized,
 * tools/list, and tools/call. It intentionally does not implement sampling,
 * elicitation, prompts, or resources; this bridge is only a tool adapter.
 */
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";

let child;
let childLines;
let nextMcpId = 1;
const pending = new Map();
const progress = new Map();

function stopChild(signal = "SIGTERM") {
  if (child && child.exitCode === null && child.signalCode === null) child.kill(signal);
}
for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.once(signal, () => {
    stopChild();
    process.exit(128);
  });
}
process.once("exit", () => stopChild());

const writeE = (value) => process.stdout.write(JSON.stringify(value) + "\n");
const writeMcp = (value) => child.stdin.write(JSON.stringify(value) + "\n");

function callMcp(method, params = {}) {
  const id = nextMcpId++;
  writeMcp({ jsonrpc: "2.0", id, method, params });
  return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
}

function routeMcp(line) {
  let message;
  try { message = JSON.parse(line); } catch { return; }
  if (Object.hasOwn(message, "id") && (message.result !== undefined || message.error)) {
    const waiter = pending.get(message.id);
    if (!waiter) return;
    pending.delete(message.id);
    if (message.error) waiter.reject(new Error(message.error.message || JSON.stringify(message.error)));
    else waiter.resolve(message.result || {});
    return;
  }
  if (message.method === "notifications/progress") {
    const token = message.params?.progressToken;
    const eId = progress.get(String(token));
    if (eId !== undefined) {
      const detail = message.params?.message ?? message.params?.progress;
      if (detail !== undefined) {
        writeE({ method: "tool.update", params: { id: eId, stream: "stdout", chunk: `${detail}\n` } });
      }
    }
    return;
  }
  // This tool-only bridge cannot answer server-initiated sampling/roots/etc.
  if (Object.hasOwn(message, "id") && message.method) {
    writeMcp({ jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "unsupported by e MCP tool bridge" } });
  }
}

async function startMcp(config) {
  if (!config || typeof config.command !== "string" || !config.command) {
    throw new Error("settings extensions.mcp.command is required");
  }
  const env = { ...process.env, ...(config.env || {}) };
  child = spawn(config.command, Array.isArray(config.args) ? config.args : [], {
    cwd: config.cwd || process.cwd(),
    env,
    stdio: ["pipe", "pipe", "pipe"],
  });
  child.stderr.pipe(process.stderr);
  childLines = createInterface({ input: child.stdout });
  childLines.on("line", routeMcp);
  child.once("exit", () => {
    for (const waiter of pending.values()) waiter.reject(new Error("MCP server exited"));
    pending.clear();
  });
  child.once("error", (error) => {
    for (const waiter of pending.values()) waiter.reject(error);
    pending.clear();
  });
  await callMcp("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "e-mcp-bridge", version: "1.0.0" },
  });
  writeMcp({ jsonrpc: "2.0", method: "notifications/initialized" });
  const tools = [];
  let cursor;
  do {
    const listed = await callMcp("tools/list", cursor ? { cursor } : {});
    tools.push(...(listed.tools || []));
    cursor = listed.nextCursor;
  } while (cursor);
  return tools;
}

function toolText(result) {
  const chunks = [];
  for (const item of result.content || []) {
    if (item.type === "text") chunks.push(item.text || "");
    else chunks.push(JSON.stringify(item));
  }
  if (result.structuredContent !== undefined) chunks.push(JSON.stringify(result.structuredContent));
  return chunks.filter(Boolean).join("\n");
}

createInterface({ input: process.stdin }).on("line", async (line) => {
  let request;
  try { request = JSON.parse(line); } catch { return; }
  const { id, method, params } = request;
  try {
    if (method === "initialize") {
      const tools = await startMcp(params?.extensions_config?.mcp);
      writeE({
        id,
        result: {
          name: "mcp",
          version: "1.0.0",
          tools: tools.map((tool) => ({
            name: tool.name,
            description: tool.description || `MCP tool ${tool.name}`,
            parameters: tool.inputSchema || { type: "object", properties: {} },
          })),
        },
      });
      return;
    }
    if (method === "tool_call") {
      const token = `e-${id}`;
      progress.set(token, id);
      try {
        const result = await callMcp("tools/call", {
          name: params.name,
          arguments: params.arguments || {},
          _meta: { progressToken: token },
        });
        writeE({ id, result: { content: toolText(result), is_error: Boolean(result.isError) } });
      } finally {
        progress.delete(token);
      }
      return;
    }
    if (method === "shutdown") {
      if (child) child.stdin.end();
      setTimeout(() => child?.kill("SIGTERM"), 500).unref();
      return;
    }
  } catch (error) {
    writeE({ id, error: error instanceof Error ? error.message : String(error) });
  }
});
