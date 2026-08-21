# Working on e

Instructions for an agent editing this repo. Read [DESIGN.md](DESIGN.md) first —
it is the why; this is the how.

## Build and check

```sh
cargo build          # dev
cargo test           # the whole contract — run this before you call anything done
```

`cargo test` is not optional. The visual design is pinned byte-for-byte in
`tests/` against the reference design's own literals. If a rendering change
makes the tests fail, you drifted the look — fix the code, don't loosen the
test.

## Where things live

```
src/core/    the harness, terminal-free
  agent.rs        the turn loop: request → stream → run tools → repeat; steering
  provider.rs     the seam — one Request, one Event stream, the SSE splitter
  completions.rs · responses.rs    the two wire dialects
  tools/          read · write · edit · ls · grep · bash · skill
  session.rs · context.rs · model.rs · auth.rs · login.rs · settings.rs · skills.rs
src/tui/     the frontend
  render.rs       SGR primitives          screen.rs      the diffing painter
  markdown.rs     md → styled lines       transcript.rs  blocks + gap policy
  highlight.rs    code tinting            composer.rs    the input editor
  panel.rs        the shared footer frame (picker + settings both use it)
  menu.rs · settingspanel.rs · authpanel.rs · statusline.rs · theme.rs · background.rs
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
  override, not a constant. (This is DESIGN.md §2; it is the extensibility that
  matters. A code-plugin API is separate and still budgeted — don't add one
  without a decision.)
- Verify UI changes with a real frame, not by reasoning about bytes. `scripts/`
  has a pty capture-and-replay harness; that is how the look gets checked.
- Keep the commit trailer: `Co-authored-by: Claude <noreply@anthropic.com>`.
