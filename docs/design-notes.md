# Design notes from surveying other coding agents

Findings from reading other harnesses' documentation against e's design
(DESIGN.md). Kept as guidance for future feature work; not shipped through
`e docs`.

## Resource loading: prompts and skills

The common shape across harnesses:

| | common practice | e |
|---|---|---|
| Global prompts | a prompts folder under the tool's home | `~/.e/prompts/*.md` |
| Project prompts | a dot-folder in the repo, trusted-gated | `.e/prompts/*.md`, trusted-gated |
| Global skills | a skills folder under the tool's home | `~/.e/skills/` |
| Project skills | dot-folder(s) in the repo, trusted-gated | `.e/skills/`, trusted-gated |
| Format | frontmatter markdown / SKILL.md standard | same open formats |

Choices worth keeping:
- One location per scope. Some harnesses discover loose root-level `.md`
  files inside skill directories and walk ancestor directories for extra
  project folders; that is surface area nobody asked for here.
- Repo resources shadow globals on a name clash — the closer context wins.
  (Others let the global win; shadowing matches how AGENTS.md layers.)
- No package manifests or settings-array source lists until someone needs
  them; data files only.

## Inheritance model

Context assembly everywhere follows the same layering, which e already
implements: base identity → skills catalog → global AGENTS.md → project
AGENTS.md (trust-gated). Repo-local resources slot into existing merge
points; no new layering concept was needed. Trust is the single gate for
everything a repo contributes — instructions, skills, prompts alike.

## Thinking levels

One harness binds shift+tab to cycling thinking levels with per-model
clamping, exposing the level to bash tools via an env var. e already had
the knob (a settings `effort` string wired into all three wire dialects)
but no key. Shift+tab now cycles through whatever levels the current model
declares — the list is data (`efforts` in providers/*.json, overridable
via models.json), so xhigh/max appear when a model exposes them. No
extension event exists in e yet — add one only when an extension needs it.

## Extensions, for contrast

The largest harnesses embed scripting runtimes and give extensions typed
hooks into nearly every internal event. e's line protocol over stdio grows
by need (DESIGN.md §2). The lesson isn't the mechanism, it's the seam list:
model switching, thinking level, tool calls, session events — the places a
user eventually wants a hook. Watch that list; don't build it.
