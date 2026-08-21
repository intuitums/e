# Working on e

Instructions for an agent editing this repo. Read [DESIGN.md](DESIGN.md) first —
it is the why; this is the how.

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
                  three wire dialects) · catalog.rs (models, availability, scope)
  auth/           credentials (mod.rs) · login.rs (OAuth, device-code, API keys)
  config/         the ~/.e surface: home.rs (paths) · store.rs (merge-write)
                  · settings.rs · trust.rs (per-directory trust)
  resources/      skills.rs · prompts.rs (/name templates) · docs.rs (the
                  embedded guides behind `e docs`)
  api/            the extension host: subprocesses over a JSONL line
                  protocol (docs/extensions.md) — tools, commands, hooks
  tools/          read · write · edit · ls · grep · bash · skill
  session.rs · output.rs · workspace.rs
src/tui/     the frontend
  render.rs       SGR primitives          screen.rs      the diffing painter
  markdown.rs     md → styled lines       transcript.rs  blocks + gap policy
  highlight.rs    code tinting            composer.rs    the input editor
  panel.rs        the shared footer frame (picker + settings both use it)
  menu.rs · settingspanel.rs · authpanel.rs · trustpanel.rs · statusline.rs
  · theme.rs · background.rs
src/main.rs  the frame loop, key handling, command dispatch
```

## How the look stays consistent

Every colour comes from the theme (`theme.fg("token", text)`), never a raw SGR
literal — the palette is the single source of truth, and it is the reference
design's, audited value-for-value. Dividers are the `border` token (fx's
`divider_style`, 240/250), not `dim`. Selection is brightness alone — bold
bright ink for the current row, `dim` for the rest, no caret.

Every footer surface (the `/@$` pickers, `/settings`) frames through
`tui/panel.rs`: top divider, header, blank, body, bottom divider, with the hint
on the status row — never a second hint inside a panel. Add a new surface? Route
it through `panel.rs` so it can't diverge.

## Conventions

- One event stream. The frontend subscribes once; text, tools, usage, errors all
  arrive on it in order (`SessionEvent`). Don't add side channels.
- The harness is budgeted (DESIGN.md §3). Prefer a spawned process over a daemon,
  a gate over a pipeline. A feature that can't pay for itself stays out.
- `~/.e/` is the only home e reads. Never reach into another tool's directory.
- **Don't hardcode what a user might change.** Looks, wordings, and behaviours a
  person could sensibly prefer are read from `~/.e/` with a built-in default —
  themes from `~/.e/themes/`, and skills, prompts, instructions, the system
  prompt the same way. When you add something user-facing, make it a file-backed
  override, not a constant. When data isn't enough there is the extension API
  (`core/api/`, docs/extensions.md) — grow its protocol by need, never by
  symmetry, and keep hooks fail-open. (This is DESIGN.md §2.)
- Verify UI changes with a real frame, not by reasoning about bytes. `scripts/`
  has a pty capture-and-replay harness; that is how the look gets checked.
- `scripts/guard.sh` pins the trust boundary: allowed network hosts, the
  sovereign home, store-only config writes, where `unsafe` lives, SHA-pinned
  CI actions. If a change legitimately moves a boundary, update the guard in
  the same commit — never work around it.
- Every PR adds its entry to `CHANGELOG.md` under `Unreleased` (pure meta —
  CI, templates, benchmark results — may skip). Parallel PRs all edit that
  same region, so expect the changelog to be the merge conflict: rebase and
  stack the entries, newest first.
- Keep the commit trailer: `Co-authored-by: Claude <noreply@anthropic.com>`.
