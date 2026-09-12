#!/usr/bin/env node
/** project routes a launch to another directory with `e --project <path>`.
 *
 * This startup-hook example uses a typed string flag and a same-binary
 * relaunch. The bootstrap marker prevents a relaunch loop; the second process
 * removes it before the session starts.
 *
 * Copy scaffold.mjs + project.mjs into ~/.e/extensions/ (chmod +x), restart
 * e, then run `e --project ../another-project "inspect this repository"`.
 */

import { realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { connect } from "./scaffold.mjs";

const BOOTSTRAP_ENV = "E_PROJECT_BOOTSTRAPPED";

/** Expand a leading home shorthand before resolving relative to the launch cwd. */
function expandHome(path) {
  if (path === "~") return homedir();
  return path.startsWith("~/") ? join(homedir(), path.slice(2)) : path;
}

/** Resolve an existing project directory or stop startup with a useful error. */
function projectDirectory(cwd, requested) {
  if (typeof requested !== "string" || !requested.trim()) {
    throw new Error("--project requires a directory");
  }
  let path;
  try {
    path = realpathSync(resolve(cwd, expandHome(requested.trim())));
  } catch {
    throw new Error(`project directory does not exist: ${requested}`);
  }
  if (!statSync(path).isDirectory()) {
    throw new Error(`project path is not a directory: ${requested}`);
  }
  return path;
}

const ext = connect({
  manifest: {
    name: "project",
    version: "1.0",
    description: "e --project: relaunch in another project directory (example)",
    flags: [
      { name: "project", type: "string", description: "relaunch e in this directory" },
    ],
    hooks: ["startup"],
  },
  startup({ cwd, argv }) {
    if (process.env[BOOTSTRAP_ENV] === "1") {
      return { argv, env: { [BOOTSTRAP_ENV]: null } };
    }
    const requested = ext.flag("project");
    if (requested === undefined) return { argv };
    return {
      argv,
      relaunch: {
        cwd: projectDirectory(cwd, requested),
        env: { [BOOTSTRAP_ENV]: "1" },
      },
    };
  },
});
ext.run();
