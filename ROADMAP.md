# Roadmap

Where e is going, in order. Each line lands only if it fits the budget
(DESIGN.md §3) — features that can't pay for their complexity stay off this
list, not quietly on it.

## Landed

- **0.1** — the look: fx-shape TUI, byte-pinned parity suite
- **0.2** — the harness: Rust rewrite; agent loop, tools, steering; two wire
  dialects (chat-completions, responses) with OAuth; JSONL sessions;
  AGENTS.md + skills context; sovereign `~/.e/` home
- **0.3** — the surface: extension API (`~/.e/extensions/`, line protocol —
  tools, commands, hooks, events); editable themes; non-destructive config
  writes

## Next

- **`e import`** — the explicit one-time migration command (credentials,
  sessions) DESIGN.md promises; today the promise is documented but the
  command doesn't exist
- **`/compact`** — summarize-and-rewrite when context fills; the token
  estimate is already in the statusline
- **Prompt templates** — `~/.e/prompts/*.md` with `$ARGUMENTS`, surfaced in
  the `/` picker next to extension commands

## Later, if they pay for themselves

- **Permission modes** — yolo is the only mode today, by decision; an
  ask/auto gate is designed (allow · deny · ask, read-only fallback) but
  waits for demand
- **More events and hooks** — `turn_start`, `session_start`, a
  `tool_result` post-hook; grow the protocol by need, not symmetry
- **Windows** — the harness is portable in principle; the pty test rig and
  the executable-bit discovery are not

## Not planned

MCP, subagents, an embedded scripting runtime, a PTY terminal daemon,
telemetry, accounts. See "What e is not" in DESIGN.md.
