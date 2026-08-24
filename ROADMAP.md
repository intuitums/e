# Roadmap

This file tracks work that has not shipped. Completed work belongs in
[CHANGELOG.md](CHANGELOG.md).

## Next

- **Session branching** — add parent relationships to session entries and a
  way to rewind or branch from an earlier point.
- **Cost tracking** — allow models to declare token prices and show the cost of
  a turn.
- **Import** — provide an explicit command for copying selected credentials or
  sessions from another tool.
- **Extension protocol** — add lifecycle events, streamed tool updates,
  resource contributions, and provider registration when an extension has a
  concrete need for them.

## Under consideration

- **Permission modes** — optional allow, deny, and ask behavior for tool calls.
- **Windows support** — the core is portable, but the terminal test tools and
  executable discovery need platform-specific work.

## Not planned

An embedded scripting runtime, a terminal daemon, telemetry, and accounts are
not planned. MCP and subagents may be supplied by extensions, but are not
planned as built-in subsystems.
