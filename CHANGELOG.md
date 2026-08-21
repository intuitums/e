# Changelog

## Unreleased

- Scoped models, the reference workflow: `/models` (renamed from `/model`,
  which still works) lists every signed-in model; `/scoped-models` is a
  multi-select — Space toggles, no scope means everything, the first toggle
  narrows to just that model — persisted as `scoped_models` in settings.
  ctrl+p / ctrl+shift+p cycles through the scope (or all available models),
  wrapping, skipping signed-out entries, persisting each switch; the
  statusline is the feedback.
- The model picker shows only models from signed-in providers; `/model`
  resolution works on the same set, so a pick is always usable. The default
  model follows availability instead of sticking (a configured model whose
  provider is signed out falls back, with a notice). Signed out entirely, e
  says so at launch — "use /login" — the reference behavior; after a login,
  a stranded model switches to an available one automatically.
- OpenAI and Anthropic API keys: the key panel now lists OpenCode Go, xAI,
  OpenAI (platform responses dialect at api.openai.com), and Anthropic — a
  new Messages dialect (`core/anthropic.rs`) with streamed thinking, tool
  use, prompt caching, and effort mapped to a thinking budget. gpt-5.x and
  claude-\* models join the catalog with their real context windows.
  `api.anthropic.com` joins the guard allowlist.
- xAI support: sign in with a SuperGrok / X Premium subscription (device
  code — a code to confirm in the browser) or an API key; grok-4.6,
  grok-4.3, and grok-build-0.1 join the catalog with their real context
  windows. Access tokens refresh lazily. `auth.x.ai` and `api.x.ai` join
  the guard's network allowlist.
- The sign-in flow now has a provider step per method, labeled with
  display names: OpenAI Codex, OpenCode Go, xAI.
- `!<cmd>` runs a shell command directly; the output shows in the
  transcript and is recorded into history, so the model sees what you did.
  The composer rail turns the `bashMode` theme color (green) the moment a
  draft starts with `!` — the reference convention: color, not words — and
  the finished block renders `$ cmd` in the same color, output muted.
- `e ask "prompt"` — one full agent turn without the TUI: styled output on
  a terminal, plain streaming text when piped. The session is saved, so
  `e -c` continues it.
- Prompt templates: `~/.e/prompts/<name>.md` becomes `/name` in the picker,
  with frontmatter `description` / `argument-hint` and bash-style argument
  substitution (`$1`, `$@`, `${1:-default}`, `${@:2}`).
- Trust: the first visit to a directory asks once whether e may load its
  AGENTS.md; declined directories still work but their instructions stay
  out of context (`/trust` grants later). Remembered in `~/.e/trust.json`.
- `~/.e/models.json` entries can declare a `context_window` (per model, or
  as a provider default) — compaction and the statusline follow the active
  model's real window. A file entry with a built-in's name now replaces the
  built-in, the same file-wins rule as themes.
- `/compact`: summarizes the older part of the session and continues in a
  fresh session file, keeping roughly the most recent 20k tokens of
  messages verbatim (the cut never separates a tool result from its call).
  Compaction only runs between turns — a mid-turn `/compact` defers to the
  end of the turn — and it also triggers automatically when real context
  usage crosses the model's window minus a 16k reserve. Messages typed
  while compacting are held and submitted after the swap; the old session
  stays fully resumable under `/resume`.
- Dependencies current: rand 0.10, sha2 0.11, base64 0.23, crossterm 0.29,
  pulldown-cmark 0.13. The parity suite pins the rendered output, so the
  markdown and terminal bumps are verified byte-for-byte.
- Open-source hardening: CI (fmt, clippy, tests on Linux + macOS),
  `scripts/guard.sh` — a security-surface audit pinning the allowed network
  hosts, the sovereign `~/.e/` home, store-only credential writes, the one
  `unsafe` file, and SHA-pinned workflow actions. CONTRIBUTING.md,
  SECURITY.md, CODEOWNERS, issue/PR templates, dependabot, weekly
  `cargo audit`, and branch protection on `main`.
- The codebase is now rustfmt-formatted and clippy-clean; both are CI gates.

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
