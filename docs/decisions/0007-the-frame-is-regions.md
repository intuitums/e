# The frame is regions: every part of e is data an extension or the user can set

Status: accepted
Date: 2026-09-14

## Context

Decision 0005 gave extensions pi's reach across a process boundary under
one rule: data crosses the line, never cells. It covered what an
extension can say — a block, a picker, a notice, a footer panel, a status
slot — but not where things go. The `/diff` review that once lived in the
binary was a live pane beside the conversation; when it became a package
it fell back to a transcript block, because the protocol had no side
pane. The same gap shows up for a plan, a test runner, a log tail, or
anything a user wants to keep in view while the conversation scrolls.

The stated goal is broader than a pane: every part of e should be
adjustable — by an extension that ships a behaviour, and by the user who
wants the pane on the other side or a different status row. pi answers
this with in-process components (`ctx.ui.custom`, entry renderers) that
draw their own cells. e cannot, and should not: a component drawing cells
bypasses the theme and the sanitizer, and one bad extension can paint
over the composer or eat ctrl+c.

## Decision

The frame is a fixed set of named regions. Every region is driven by data,
and three rules govern who sets it:

1. **Only data crosses the pipe.** An extension describes a list, a diff,
   text, markdown, or rows with theme tokens. e paints, through the theme.
2. **e owns interaction, extensions own content.** Focus, scrolling, the
   cursor, selection, the mouse, the split, and the narrow-terminal
   fallback are e's, written once. An extension is told what the user did
   (`pane.select`, `pane.activate`, `pane.key`, `pane.closed`) and answers
   with new content.
3. **The user outranks the extension.** Placement, widths, the status
   row's wording, and which regions show live in `~/.e/layout.json` with
   built-in defaults, the way keybindings and themes already work. An
   extension proposes a side for its pane; the file decides.

The regions and how each is set:

| Region | Extension | User (`layout.json`) |
| --- | --- | --- |
| banner | — | `banner: false` hides it |
| transcript | `ui.show`, tool `display`, the `render` hook | — |
| activity row below the transcript | `ui.activity {key, text}` | the `activity` template |
| pane (left or right) | `ui.pane {id, title, side, sections}` | `panes.<id>.side`, `.width`, `split_min`, `focus` |
| widget strip above the composer | `ui.widget {key, lines}` | — |
| composer | `ui.compose`, `ui.input` | `keybindings.json` |
| status row | `ui.status {key, text}` | `status.left`, `status.right` templates |
| footer | `ui.panel`, `ui.select`, `ui.confirm` | — |

A pane's sections are `list`, `diff`, `text`, `markdown`, and `rows` —
the `show` grammar plus a selectable list. Lists take eight rows and
scroll; the rest share the height. The pane holds 256 KiB and is
refreshed by sending it again under the same `id`, keeping the user's
place in every section whose `id` survived. One pane at a time: another
extension's replaces it and the owner is told. Enter on any non-list
section attaches the selected rows to the composer as a snapshot, so
every pane gets the attach-to-draft behaviour the old diff panel had
without writing it.

The status row is a template of tokens (`{model}`, `{effort}`,
`{context}`, `{cwd}`, `{session}`, `{status}`, `{status:<name>}`) whose
default reproduces the previous row byte for byte; a segment whose tokens
came up empty drops out, so the template never paints a dangling
separator. The activity row (`Thinking (3s) (↑1k ↓20)`) is the same kind
of template — `{phase}`, `{elapsed}`, `{tokens}`, `{activity}` — so the
clock or the token counts are the user's to keep or drop, and an
extension's `ui.activity` text has a place on it.

## Consequences

- The protocol number stays 1; `pane` and `widget` join the advertised
  capabilities. `ui.status` takes an optional `key`.
- The `e-diff` package returns to being a sidebar: a file list and the
  selected file's patch, refreshed after every tool, exactly the design
  the binary once carried — as a package, on a surface any package can
  use.
- `~/.e/layout.json` is a persisted contract under `docs/compatibility.md`.
- The `render` hook is pi's message renderer under rule 1: an extension
  that lists `renders: ["tool:bash", "assistant"]` is asked for a body and
  a format when such an entry completes, and e paints the answer. It
  runs off the paint path and fails open, so a slow renderer costs
  nothing but its own effect. `ui.editor` is the multi-line answer field
  the same rule allows.
- What extensions still cannot do: draw cells, or replace the composer.
