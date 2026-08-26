# Working on e

Instructions for an agent editing this repo.

## Build and check

```sh
cargo build          # dev
cargo test           # the whole contract — run this before you call anything done
cargo fmt --check && cargo clippy --all-targets -- -D warnings
./scripts/guard.sh   # the security-surface audit — CI runs all of these
```

`cargo test` is not optional. The visual design is pinned byte-for-byte in
`tests/` against the reference design's own literals. If a rendering change
makes the tests fail, you drifted the look — fix the code, don't loosen the
test.

## Where things live

```
src/core/    the harness, terminal-free
  agent/          the turn loop (mod.rs): request → stream → run tools → repeat;
                  steering · compact.rs (threshold, keep-recent cut, summarize)
                  · context.rs (system prompt, AGENTS.md, skills catalog)
  provider/       the seam (mod.rs) — one Request, one Event stream, the SSE
                  splitter · completions.rs · responses.rs · anthropic.rs (the
                  three wire dialects) · registry.rs + providers/*.json
                  (providers are data: gateway, dialect, auth surface, seed
                  models) · catalog/ (assembly, availability, scope;
                  remote.rs = the live /models sync)
  auth/           credentials (mod.rs) · login.rs (OAuth, device-code, API keys)
  config/         the ~/.e surface: home.rs (paths) · store.rs (merge-write)
                  · settings.rs · trust.rs (per-directory trust) ·
                  keybindings.rs (composer chord overrides)
  resources/      skills.rs · prompts.rs (/name templates) · docs.rs (the
                  embedded guides behind `e docs`)
  api/            the extension host: subprocesses over a JSONL line
                  protocol (docs/extensions.md) — tools, commands, hooks
  tools/          read · write · edit · grep (optional `glob` filter) · bash
                  (optional `background`/`handle` for long-lived processes)
                  — the whole surface; directory listing and file-finding go
                  through bash, and skills load through read (the catalog
                  carries their paths)
  session.rs · output.rs · workspace.rs — session.rs is a tree, not just a
                  line: id/parent per message, `/tree` branches in place
src/tui/     the frontend (short paths re-export from the groups)
  paint/          render · screen · theme · background · highlight
  content/        markdown · transcript · composer · statusline
  surfaces/       panel · menu · settingspanel · authpanel · trustpanel
  app/            mod.rs (App state, keys, the frame loop) · events.rs
                  (session-event handling) · menus.rs (footer menus) ·
                  login.rs (sign-in flows)
src/main.rs  CLI entry — flags, ask/docs/auth/update, then tui::app::run
```

## Running one thing, not everything

Each `tests/*.rs` file is its own binary; the fast loops are:

```sh
cargo test --test stream            # agent turn loop against a mock provider
cargo test --test providers         # the three wire dialects' request/stream shapes
cargo test --test parity            # byte-pinned rendering (run after any look change)
cargo test --test toolloop          # end-to-end tool execution
cargo test name_of_one_test         # any single test, by name substring
```

New integration tests use `tests/common/` (`mod common;`) — `Home` for an
isolated `E_HOME`, `env_lock()` around anything env-global, `serve_sse` +
`test_model` for a mock provider. Don't hand-roll a second mock harness.

## How the look stays consistent

Every colour comes from the theme (`theme.fg("token", text)`), never a raw SGR
literal — the palette is the single source of truth, and it is the reference
design's, audited value-for-value. Panel dividers (`tui/surfaces/panel.rs`)
use the `border` token (divider_style, 240/250), not `dim` — markdown's own
thematic-break rule and blockquote rail are a different, dimmer reference
element and are pinned as `dim` in `tests/parity.rs`; don't conflate the two.
Selection is brightness alone — bold bright ink for the current row, `dim`
for the rest, no caret.

Every footer surface (the `/@$` pickers, `/settings`) frames through
`tui/surfaces/panel.rs`: top divider, header, blank, body, bottom divider, with
the hint on the status row — never a second hint inside a panel. Add a new
surface? Route it through `panel.rs` so it can't diverge.

## Conventions

- One event stream. The frontend subscribes once; text, tools, usage, errors all
  arrive on it in order (`SessionEvent`). Don't add side channels.
- Keep the harness small. Prefer a spawned process over a daemon and a gate
  over a pipeline. Add complexity only when the feature requires it.
- `~/.e/` is the only home e reads. Never reach into another tool's directory.
- **Don't hardcode what a user might change.** Looks, wordings, and behaviours a
  person could sensibly prefer are read from `~/.e/` with a built-in default —
  themes from `~/.e/themes/`, and skills, prompts, instructions, the system
  prompt the same way. When you add something user-facing, make it a file-backed
  override, not a constant. When data isn't enough there is the extension API
  (`core/api/`, docs/extensions.md) — grow its protocol by need, never by
  symmetry, and keep hooks fail-open.
- Verify UI changes with a real frame, not by reasoning about bytes. `scripts/`
  has a pty capture-and-replay harness; that is how the look gets checked.
- `scripts/guard.sh` pins the trust boundary: allowed network hosts, the
  sovereign home, store-only config writes, where `unsafe` lives, SHA-pinned
  CI actions. If a change legitimately moves a boundary, update the guard in
  the same commit — never work around it.
- Commit metadata (trailers, attribution) belongs to each agent's own global
  config, not this repo. Don't add identity trailers here by default; write
  clean, descriptive commit messages and leave attribution to personal
  preference.
