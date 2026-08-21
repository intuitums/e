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

- The catalog (name + description) is advertised in the system prompt; the
  model pages the body in through the `skill` tool when it needs it.
- The `$` picker inserts a skill by hand.
- Files are read per use — add or edit a skill and it is live immediately.
