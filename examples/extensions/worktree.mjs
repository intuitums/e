#!/usr/bin/env node
/** worktree — a startup-hook example on the scaffold: `e -w [branch]`
 *  creates a managed Git worktree and relaunches e there, consuming the
 *  flag so the relaunched session is a normal one. This is the pattern
 *  workspaces extensions are built around.
 *
 * Leaves land in <root>/worktrees/<repo>/<branch>, branched from the repo's
 * default branch after a fetch. Set the root with E_WORKTREE_ROOT (default
 * ~/workspaces).
 */

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { homedir } from "node:os";
import { connect } from "./scaffold.mjs";

function git(cwd, args, allow = false, timeoutMs = 0) {
  const r = spawnSync("git", ["-C", cwd, ...args], {
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout: timeoutMs,
  });
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

function prepareWorktree(cwd, requestedBranch) {
  const repoRoot = git(cwd, ["rev-parse", "--show-toplevel"], true);
  if (!repoRoot) throw new Error("-w requires running e inside a Git repository");
  const repo = owningRepo(repoRoot);
  git(repo, ["fetch", "--prune", "origin"], true, 15000); // newest default first
  const base =
    git(repo, ["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"], true) ||
    "origin/main";
  const branch = requestedBranch || `wt-${Date.now().toString(36)}`;
  const path = join(root(), "worktrees", basename(repo), branch);
  mkdirSync(dirname(path), { recursive: true });
  git(repo, ["worktree", "add", "-b", branch, path, base]);
  return path;
}

const ext = connect({
  manifest: {
    name: "worktree",
    version: "1.1",
    description: "e -w: managed Git worktree launch (example)",
    flags: [{ name: "worktree", type: "string", description: "launch e in a fresh worktree" }],
    hooks: ["startup"],
  },
  startup({ cwd, argv }) {
    // The typed flag was parsed by e: a string (branch name), null for a
    // bare --worktree (pick a random name), undefined when not given.
    const branch = ext.flag("worktree");
    if (branch === undefined) return { argv }; // no --worktree: pass through
    const next = argv.filter((a) => a !== "--worktree" && !a.startsWith("--worktree="));
    return { argv: next, relaunch: { cwd: prepareWorktree(cwd, branch || undefined) } };
  },
});
ext.run();