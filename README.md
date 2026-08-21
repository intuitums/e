<div align="center">

# 𝑒

**A coding agent that lives in your terminal.**

Its own agent loop, its own tools, its own sessions — one small, fast Rust
binary. No runtime, no daemon, no account. Grayscale, typography-first, and
pinned pixel-for-pixel by its own test suite.

<sub>[Design](DESIGN.md) · [Getting started](#getting-started) · [Commands](#commands) · [How it works](#how-it-works)</sub>

</div>

---

```
𝑒 v0.2.0 · Run /help for commands

┃ read src/main.rs and tell me what the ctrl+c behavior is

  ● Read main.rs  240 lines

  Ctrl+C is two-stage. The first press interrupts a streaming turn, or clears
  the composer if you're typing; it then arms a 1.5s window in which a second
  press exits. On its own it never quits — you always confirm.

  1s (↑4.2k ↓63)

┃

opencode-go/deepseek-v4-flash · 3%
```

## What it is

𝑒 talks to language models, runs tools to read and change your code, and
remembers the conversation — the whole loop a coding agent needs — in about
four thousand lines of Rust you could read on a plane. It speaks to models
directly over their own wire protocols; there is no SDK, no vendor runtime, and
nothing phones home.

It is opinionated on purpose. Three principles, spelled out in
[DESIGN.md](DESIGN.md):

|   | |
|---|---|
| **The look is law** | The visual design is an executable spec — glyphs, spacing, palette, panel geometry are byte-pinned in `tests/`. Drift is a build failure, not a surprise. |
| **Own home, open formats** | Everything 𝑒 knows lives under `~/.e/` and it reads nothing else at runtime. But every format there is an open convention — `AGENTS.md`, `SKILL.md`, JSONL sessions — so other tools can read it too. |
| **Readable in an afternoon** | The core has a line budget. Big features earn their complexity or stay out: a spawned process instead of a terminal daemon, a gate instead of a review pipeline. |

## Getting started

```sh
cargo build --release          # one static binary at target/release/e
ln -s "$PWD/bin/e" ~/.local/bin/e   # optional: put `e` on your PATH
```

Sign in — 𝑒 keeps its own credentials under `~/.e/`, it never borrows another
tool's:

```
/login
```

Pick **Sign in with an account** to authorize a ChatGPT login in your browser,
or **Sign in with an API key** to paste one. Then just talk:

```
e            # open a session in the current directory
e -c         # continue this directory's most recent session
```

## The interface

The transcript grows down your normal terminal into its scrollback — no alt
screen, no panes, nothing to get lost in. Emphasis is weight and underline, not
color; the whole palette is a grayscale ramp that follows your terminal's
light or dark background automatically.

A `┃` rail marks your turns and the composer. Tool calls are single `●` rows.
Code lands in shrink-wrapped, labeled panels. The status line carries the model
and how much context you've used; the tab title shows `𝑒 · <path>`.

Three keystrokes open a picker over the composer:

| Key | Opens |
|:---:|---|
| `/` | commands, filtered as you type |
| `@` | files in the workspace, fuzzy-matched and inserted inline |
| `$` | your skills |

<kbd>↑</kbd><kbd>↓</kbd> navigate · <kbd>Enter</kbd> chooses · <kbd>Esc</kbd> closes.
Type while a turn streams and your message **steers** it — queued and delivered
between steps, not dropped. <kbd>Esc</kbd> interrupts; <kbd>Ctrl</kbd>+<kbd>C</kbd>
twice exits.

## Commands

| | |
|---|---|
| `/login` | sign in — account or API key |
| `/model` | list models, or switch and remember the choice |
| `/resume` | reopen a saved session and replay it |
| `/new` | start a fresh session |
| `/copy` | copy the last reply to the clipboard |
| `/help` · `/version` · `/quit` | the usual |

## Tools

The model calls these itself, mid-turn. 𝑒 runs them with no permission gate by
default — it is a **yolo** agent, as intended for a tool you trust in your own
workspace.

`read` · `write` · `edit` · `ls` · `grep` · `bash` · `skill`

`bash` is a spawned process with captured output, not a terminal emulator; tool
output is truncated to a byte cap. Failed tool results feed back to the model
so it can recover on its own.

## Configure it

Everything is under `~/.e/`, all optional:

```
~/.e/
  settings.json     preferences, incl. an optional `system_prompt` override
  auth.json         your credentials — written by /login, never read elsewhere
  AGENTS.md         global instructions for the agent (empty by default; yours to fill)
  sessions/         one JSONL log per conversation
  skills/           SKILL.md directories, surfaced by $ and in the prompt
```

An `AGENTS.md` — global here, or per-project in your repo — is layered into the
agent's context as project instructions. It ships as nothing; write whatever you
want. The default system prompt follows the same shape as the reference harness;
set `system_prompt` in `settings.json` to replace it wholesale.

## How it works

```
you ──▶ composer ──▶ agent loop ──┬──▶ provider ──▶ model
                          ▲        │      (raw wire protocol, streamed)
                          │        └──▶ tools ──▶ your workspace
                          └──── one session stream ────▶ transcript
```

A turn is: send the conversation, stream the reply, and if the model asked for
tools, run them, feed the results back, and go again — until a reply arrives
with no tool calls. Every event — text, reasoning, tool start and end, usage —
arrives on **one ordered stream**, so the interface never guesses at state.

Two model dialects ship today: the chat-completions family (API-key providers
like OpenCode Go) and the Responses API (the ChatGPT backend, with browser
OAuth and token refresh). Adding a provider is a table entry and, at most, a
dialect branch.

```
src/
  core/        the harness — terminal-free
    agent.rs       the loop, steering, retry
    provider.rs    the seam: one request, one event stream
    completions.rs · responses.rs    the two wire dialects
    tools/         read · write · edit · ls · grep · bash · skill
    session.rs · context.rs · model.rs · auth.rs · login.rs · skills.rs
  tui/         the terminal frontend
    render.rs      SGR styling primitives
    markdown.rs    markdown → styled lines
    highlight.rs   code-panel syntax tinting
    screen.rs      the diffing painter
    composer.rs · transcript.rs · statusline.rs · menu.rs · authpanel.rs · theme.rs
tests/         the visual + behavioral contract
themes/        the two palettes
```

## Develop

```sh
cargo test           # the parity suite: byte-pinned visuals + the agent loop
cargo run            # run from source
```

`scripts/` holds a pty capture-and-replay harness for checking real frames
against the spec.

## License

[MIT](LICENSE).
