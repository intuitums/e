# e

**A TUI for coding agents.** A fast, minimal terminal frontend that drives an
agent engine as a library — same sessions, settings, models, skills, and
extensions as the engine's own CLI — while owning every pixel of the interface.

```
𝑒 v0.1.0 · Run /help for commands

┃ what is a TUI?

  A text user interface.

  2s (↑48 ↓42)

┃

deepseek-v4-flash · high · …/repos/e
```

## Design

Three principles, spelled out in [DESIGN.md](DESIGN.md): the look is law
(the visual design is a byte-pinned executable spec), a guest not a landlord
(credentials, sessions, and conventions interoperate with what's already on
the machine), and readable in an afternoon (the kernel has a line budget and
features must pay for themselves inside it).

Grayscale, typography-first, zero chrome: emphasis is weight and underline, not
color. The transcript grows down the normal screen into scrollback — no alt
screen, no bordered panes. A `┃` rail marks your turns and the composer; tool
calls are single `●` rows; code renders in shrink-wrapped labeled panels; a
one-line status bar carries model, effort, context, and workspace.

Every visual contract — heading styles, list glyphs, panel geometry, palette,
activity wording — is pinned by the test suite (`npm test`), so the look cannot
drift silently.

## Status

The harness is being built milestone by milestone; the frame, renderer, and
parity suite are in. Providers, the agent loop, tools, and sessions are next.
The list below is the feature contract the milestones are building toward.

## The contract

- streaming turns: markdown with heading hierarchy, lists, code panels with
  syntax highlighting, quote rails, rules, OSC-8 hyperlinks
- `●` tool rows, live activity line (`Thinking (12s)` →
  `running | 4 files read … (↑2.2k ↓14)`), per-turn duration/token trailer
- slash picker with fuzzy filtering; /help /new /clear /resume /rename /copy
  /compact /model /models /status /version /quit — plus any commands your
  installed engine extensions register
- /resume: session picker → switch → full styled replay
- esc interrupts; ctrl+c twice exits; ctrl+d on empty exits; `-c` continues
  the most recent session
- automatic light/dark from the terminal background

## Layout

```
src/
  core/       the harness — budgeted (DESIGN.md §3), terminal-free
  tui/        the terminal frontend: render.rs (SGR styling), markdown.rs,
              highlight.rs, screen.rs (the differ), composer.rs,
              transcript.rs, statusline.rs, theme.rs
tests/        the parity suite (the byte-pinned visual contract)
scripts/      pty capture + replay harness
themes/       the two palettes
```

Tools and the rest of the core (providers, agent loop, sessions,
permissions) land milestone by milestone.

## Run

```
e            # bin/e — runs the release binary, building it if absent
cargo run    # development
```

## Roadmap

Tool approval flow and permission modes, extension dialog surfaces, full-screen
catalog menus, queued-prompt review, inline images, transcript expansion,
compiled packaging.

## License

MIT.
