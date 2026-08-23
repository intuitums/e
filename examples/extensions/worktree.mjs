#!/usr/bin/env node
/**
 * worktree — a startup-hook example: `e -w [branch]` creates a managed Git
 * worktree and relaunches e there, consuming the flag so the relaunched
 * session is a normal one. This is the pattern workspaces extensions are
 * built around; a minimal but real version lives here as a reference.
 *
 * Leaves land in <root>/worktrees/<repo>/<branch>, branched from the repo's
 * default branch after a fetch. Set the root with E_WORKTREE_ROOT (default
 * ~/workspaces).
 */

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { homedir } from "node:os";
import { createInterface } from "node:readline";

const rl = createInterface({ input: process.stdin });

function git(cwd, args, allow = false) {
  const r = spawnSync("git", ["-C", cwd, ...args], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  if (r.status === 0) return r.stdout.trim();
  if (allow) return undefined;
  throw new Error(`git ${args.join(" ")} failed: ${r.stderr.trim() || r.status}`);
}

/** The repository that owns cwd — the base clone for linked worktrees. */
function owningRepo(repoRoot) {
  const common = git(repoRoot, ["rev-parse", "--git-common-dir"], true);
  return common ? resolve(repoRoot, common, "..") : repoRoot;
}

function root() {
  return process.env.E_WORKTREE_ROOT?.trim() || join(homedir(), "workspaces");
}

function prepareWorktree(cwd) {
  const repoRoot = git(cwd, ["rev-parse", "--show-toplevel"], true);
  if (!repoRoot) throw new Error("-w requires running e inside a Git repository");
  const repo = owningRepo(repoRoot);
  git(repo, ["fetch", "--prune", "origin"], true, 15000); // newest default first
  const base =
    git(repo, ["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"], true) ||
    "origin/main";
  const branch = `wt-${Date.now().toString(36)}`;
  const path = join(root(), "worktrees", basename(repo), branch);
  mkdirSync(dirname(path), { recursive: true });
  git(repo, ["worktree", "add", "-b", branch, path, base]);
  return path;
}

rl.on("line", (line) => {
  let request;
  try { request = JSON.parse(line); } catch { return; }
  switch (request.method) {
    case "initialize":
      process.stdout.write(JSON.stringify({
        id: request.id,
        result: {
          name: "worktree",
          version: "1.0",
          description: "e -w: managed Git worktree launch (example)",
          flags: [{ name: "-w, --worktree", description: "launch e in a fresh worktree" }],
          hooks: ["startup"],
        },
      }) + "\n");
      break;
    case "hook.startup": {
      const argv = request.params.argv || [];
      const idx = argv.findIndex((a) => a === "-w" || a === "--worktree" || a.startsWith("--w=") || a.startsWith("--worktree="));
      if (idx === -1) {
        process.stdout.write(JSON.stringify({ id: request.id, result: { argv } }) + "\n");
        break;
      }
      try {
        const next = [...argv];
        next.splice(idx, 1); // consume the flag; the branch is auto-named
        const path = prepareWorktree(request.params.cwd);
        process.stdout.write(JSON.stringify({
          id: request.id,
          result: { argv: next, relaunch: { cwd: path } },
        }) + "\n");
      } catch (error) {
        process.stdout.write(JSON.stringify({ id: request.id, error: error.message }) + "\n");
      }
      break;
    }
    case "shutdown":
      process.exit(0);
  }
});