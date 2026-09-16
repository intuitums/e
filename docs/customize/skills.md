---
title: Skills
description: SKILL.md directories and how the model pages them in
order: 3
---

# Skills

A skill is a directory: `~/.e/skills/<name>/SKILL.md` (the open SKILL.md
convention — other tools can read the same files).

```markdown
---
name: release
description: how to cut a release of this project
---
Step one …
```

- The catalog (name + description + the SKILL.md path) is advertised in the
  system prompt; the model reads the body in with the ordinary `read` tool
  when the task matches — only descriptions stay in context until then.
- A `description:` may span lines (`>` / `|` block scalars, or indented
  continuation); it folds to one line in the catalog and picker.
- `disable-model-invocation: true` keeps a skill out of the catalog — only
  the `$` picker reaches it.
- The `$` picker inserts a skill by hand.
- Files are read per use — add or edit a skill and it is live immediately.

## Package skills

An installed [package](packages.md) contributes its `skills/` directory the
same way. A global skill of the same name shadows a package's; the `$` picker
labels package skills `Package`.

## Repo-local skills

A trusted repository can carry its own skills in `.e/skills/`:
`<repo>/.e/skills/<name>/SKILL.md`, same format as above. They load only
after `/trust`, like the repo's AGENTS.md, and shadow a global skill of the
same name — the closer context wins.
