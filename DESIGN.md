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

## 2. Own home, open formats — and everything in it is yours

e is sovereign over its own state, and extensible through it. Everything it knows lives under one
unified home — `~/.e/` — and it never reaches into another tool's territory
to run:

```
~/.e/
  settings.json     preferences
  auth.json         credentials — e's own, never read from elsewhere
  AGENTS.md         global instructions
  sessions/         JSONL session logs
  skills/           SKILL.md skill directories
  prompts/          slash prompt templates
  themes/           palette overrides
  extensions/       executables speaking the line protocol (docs/extensions.md)
```

Sovereignty is not a silo, because every format in that home is an **open
convention, not a dialect**: instructions are `AGENTS.md` (the open spec —
never a vendor file like CLAUDE.md), skills are `SKILL.md` directories,
sessions are plain JSONL in an established schema, prompts are frontmatter
markdown. Other tools can read e's home without e installed; that is the
interop, and it points outward.

Migration is **explicit, never implicit**: `e import` copies credentials or
sessions from another tool's store once, with the user watching. e never
silently borrows at runtime — if it isn't in `~/.e/`, e doesn't have it.

And the home is the **extension surface**. Everything a user might reasonably
want to change is data they drop in `~/.e/`, with the built-in as the
fallback — never something hardcoded they'd have to edit Rust to touch.
Themes are files in `~/.e/themes/`; skills, prompts, instructions, and the
system prompt are all overridable the same way. This is the extensibility that
matters — a person shapes e by owning its data, not by writing a plugin. And
when data isn't enough, the home is also the **code surface**: an extension is
any executable dropped in `~/.e/extensions/`, in any language, speaking a
line protocol over stdio (docs/extensions.md) — tools, commands, hooks. The
process boundary is the API; there is no embedded runtime to learn or link
against. The rule
for building: if it's a look, a wording, or a behaviour a user could sensibly
prefer, read it from `~/.e/` with a sane default, don't nail it into the
binary.

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

Not an embedded-runtime platform: extensions exist (principle 2), but they
are subprocesses over stdio, chosen precisely because a scripting runtime —
an interpreter, a dylib ABI, a package ecosystem — would blow the budget
(principle 3). The protocol grows by need, not symmetry. Not a product
surface: no telemetry, no account, no upsell, no update channel phoning
home. Not a research playground: the spec-first discipline means novelty
lands behind tests or not at all.
