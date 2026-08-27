# Roadmap

This file tracks work that has not shipped. Completed work belongs in
[CHANGELOG.md](CHANGELOG.md).

## Next

- **Import** — provide an explicit command for copying selected credentials or
  sessions from another tool.
- **Extension resources** — add resource contributions and provider
  registration when an extension has a concrete need for them. Lifecycle
  events and streamed tool updates already ship.

- **More wire dialects** — Vertex and Bedrock need cloud-auth plumbing
  (GCP tokens, SigV4) beyond the API-key path; a native Mistral
  conversations dialect only if the compat endpoint proves lossy. New
  OpenAI-compatible providers are data-only follow-ups on the current rails.

## Under consideration

- **Permission modes** — optional allow, deny, and ask behavior for tool calls.
- **Windows support** — the core is portable, but the terminal test tools and
  executable discovery need platform-specific work.

## Not planned

An embedded scripting runtime, a terminal daemon, telemetry, and accounts are
not planned. MCP and subagents ship as extension examples, not built-in
subsystems.
