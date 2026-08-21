# Changelog

## 0.3.0 — 2026-08-21

- **Extension API** (`src/core/api/`): executables in `~/.e/extensions/`
  run as long-lived subprocesses speaking a JSONL line protocol — custom
  tools (overriding built-ins by name), slash commands in the `/` picker,
  a `tool_call` gate hook (fail-open), `turn_end` events, and transcript
  notices. Protocol reference and a worked shell example in
  [docs/extensions.md](docs/extensions.md).
- **Editable themes**: `~/.e/themes/<name>.json` appears in `/settings`;
  a user file wins over the built-in for the same name.
- **Non-destructive config**: every settings/auth write is read-merge-write
  with an atomic rename — unknown keys survive, corrupt files are
  quarantined (`.corrupt-<ms>`), never overwritten.
- Steering fix: a message typed mid-turn is held and folded into the
  running turn (it was being rejected with a notice).

## 0.2.0 — 2026-08

- The harness, rewritten in Rust from scratch: own agent loop (request →
  stream → tools → repeat), steering, delivery-aware retry, one ordered
  session event stream.
- Two wire dialects (chat-completions SSE, responses) with API-key and
  OAuth/PKCE sign-in; `/login` flow with account-or-key choice.
- Built-in tools: read · write · edit · ls · grep · bash · skill.
- JSONL sessions under `~/.e/sessions/<cwd-slug>/`; `-c` and `/resume`.
- Context: system prompt (overridable via `settings.json`), global and
  project `AGENTS.md`, skills catalog.
- The full fx-shape TUI in Rust: line-differ renderer, markdown, code
  panels, pickers, settings, auth panel — pinned by the parity suite.

## 0.1.0 — 2026-08

- First release: a TypeScript TUI frontend with the fx visual design and
  the byte-for-byte parity test suite that still governs the look.
