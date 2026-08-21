# What e is

Three principles. Everything in this repo either serves one of them or
doesn't belong.

## 1. The look is law

Most terminal tools treat their appearance as a vibe — whatever the current
code happens to emit. In e, the visual design is an **executable
specification**: glyphs, SGR sequences, panel geometry, palette indices,
spacing policy, and status wordings are pinned byte-for-byte in
`tests/parity.rs`. A change that drifts the look is a build failure, not a
regression someone notices next week.

This is why the frontend needs no TUI framework. The design is a line
printer with a differ; the spec is small enough to test and the tests are
the design document. When the renderer was rewritten from TypeScript to
Rust, the suite carried over and the output didn't change — that is the
property this principle buys.

## 2. A guest, not a landlord

Every coding agent builds its own silo — its own credential store, session
format, config dialect — and treats the machine as its private territory.
e does the opposite: it treats the agent ecosystem on a machine as **shared
state it is a guest in**.

- Credentials are borrowed read-only from stores that already exist before
  e's own is consulted. Installing e never demands a re-login.
- Sessions are written in an established JSONL schema other tooling can
  read, under e's own directory — interoperable, never invasive.
- Conventions that already span harnesses on this machine — `AGENTS.md`,
  `skills/`, prompt templates — are honored in the same shapes, so one
  canon serves every tool.

The test for any integration: could e be deleted tomorrow leaving no trace,
and could its files be read without it installed? Both must stay yes.

## 3. Readable in an afternoon

The reference points in this space run from ~100k to ~345k lines of core.
e's kernel has a budget: **the whole harness stays small enough to read end
to end in a sitting** — on the order of 15k lines. The budget is a design
input, not an aspiration:

- Shell execution is a spawned process with captured output, not a terminal
  daemon with a VT emulator. If a session needs a real terminal, the user
  has one.
- Permissions are a gate — allow, deny, ask, with a read-only fallback —
  not a model-driven review pipeline.
- Big capabilities (MCP, subagents, web tooling) are admitted only when
  they can pay their complexity inside the budget, or they stay out.

When a feature and the budget conflict, the feature loses or the budget is
raised *explicitly, in this file* — never silently.

# What e is not

Not a plugin platform: there is no extension API, and the way to change e
is to change e. Not a product surface: no telemetry, no account, no
upsell, no update channel phoning home. Not a research playground: the
spec-first discipline means novelty lands behind tests or not at all.
