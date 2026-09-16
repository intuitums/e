---
title: Sandboxing
description: e's trust model and how to isolate a session
order: 4
---

# Sandboxing

e has no built-in permission system. It runs with the permissions of the
user and process that launch it: the `read`, `write`, `edit`, and `bash`
tools can touch anything that process can — the whole filesystem, any
network the machine can reach, any credential in the environment. This is
deliberate (CLAUDE.md: "keep the harness small"), not an oversight, but it
means the isolation has to come from outside e, not from inside it.

Local storage still has privacy boundaries. On Unix, e makes its state home
private on writes and session reopening, creates session logs owner-only,
and creates credential staging files owner-only before writing secrets.
These permissions protect against other local accounts, not tools or
extensions running as your own user. See [compatibility.md](../extend/compatibility.md)
for how older session files are handled.

`scripts/guard.sh` is a different thing and doesn't cover this: it audits
*e's own build* (allowed network hosts, the `~/.e` home, where `unsafe`
lives, SHA-pinned CI actions), not the permissions of a running session. A
clean `guard.sh` says nothing about what a live `e` process can reach.

The rest of the extension surface (`docs/customize/extensions.md`: events, the
`before_turn` and `tool_result` hooks, `ui.*` and `session.*` requests) is
the same posture. An extension can narrow the toolset (`session.tools`),
append to the system prompt, redact tool output, and ask the user things —
none of which contains it. It cannot emit terminal bytes, rewrite the
provider request, or replace the system prompt, and every request it makes
is bounded and answered; that is what the boundary protects, and it is
e's own integrity, not the machine's.

Packages (`e install`) widen this, not narrow it: a package's extensions
launch as your user like any other, and its skills and prompts steer the
model. Installing is cloning with your own `git`; nothing runs at install
time, everything runs at the next launch. Read a package before installing
it and pin the ref you read (`@v1`).

## What e gives you: the `tool_call` hook

Extensions can gate individual tool calls — see
[`docs/customize/extensions.md`](../customize/extensions.md#results-by-method) and the
[`gate.mjs`](../customize/examples/gate.mjs) / [`protected.mjs`](../customize/examples/protected.mjs)
examples, which deny destructive bash patterns and credential-shaped paths
respectively. This is a real, useful speed bump, but it is fail-open by
design: e's own docs are explicit that "a slow or crashed extension never
blocks the agent." A hook is a guard against clearly-bad, anticipated
patterns — not a boundary that holds against a compromised or adversarial
extension, a model that finds a pattern the denylist missed, or a bug in
the hook itself. Treat it as a second layer, not the isolation.

## Getting a real boundary

For an actual boundary, isolate the process:

- **Container the whole session.** Run `e` itself inside Docker (or an
  equivalent) with the mounts, network, and credentials scoped to what the
  session actually needs. Coarsest-grained, simplest to reason about, and
  the default recommendation if you don't need finer control.
- **Restricted user or VM.** Run `e` as a low-privilege user, or inside a
  VM, when a container's shared kernel isn't isolation enough for your
  threat model.
- **OS-level sandboxing of just `bash`.** e's extension protocol lets a
  tool declaration override a built-in by name (`docs/customize/extensions.md`:
  "add tools — and override a built-in by using its name"), so an
  extension can replace `bash` with a version that wraps the command in
  `bwrap` (Linux), `firejail`, or `sandbox-exec` (macOS) before running it.
  There is no example of this in `docs/customize/examples/` yet — it's real work
  to get right (see the next section) — but the mechanism exists today. If
  you build one: preserve the built-in bash schema's full contract
  (`command`, `timeout`, `background`, `handle`, `signal` — see
  `src/core/tools/bash.rs`) or explicitly reject what you don't support,
  rather than silently dropping it; a tool that claims to support
  `background: true` and then hangs or errors opaquely is worse than one
  that says plainly "not supported here."

## Why there's no built-in sandbox example yet

Hand-rolled sandbox flags (bwrap's bind-mount list, a seccomp filter, a
`sandbox-exec` profile) are easy to get subtly wrong in a way that *looks*
isolated but isn't — a missing `--unshare-net`, a bind mount that's
writable when it should be read-only. Getting that right for real is its
own project: `thule` is the planned first-party answer, built to give e a
real execution boundary directly instead of leaving every user to wrap
`bash` themselves. Until it lands, a future `docs/customize/examples/sandbox.mjs`
built on the `tool_call`-override mechanism above should wrap a maintained
sandboxing tool and fail loudly (refuse the call) when that tool isn't
installed, never fall back to running the command unsandboxed.
