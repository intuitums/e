#!/usr/bin/env node
/**
 * The published entry point: node cannot carry the type-stripping flag in a
 * shebang portably, so start the bot with the same command `npm start` uses.
 */
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const entry = fileURLToPath(new URL("../src/index.ts", import.meta.url));
const { status } = spawnSync(
  process.execPath,
  ["--experimental-strip-types", entry, ...process.argv.slice(2)],
  { stdio: "inherit" },
);
process.exit(status ?? 1);
