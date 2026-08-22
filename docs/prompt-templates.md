# Prompt templates

A markdown file at `~/.e/prompts/<name>.md` becomes the `/name` command.

```markdown
---
description: review the changes
argument-hint: [path]
---
Review ${1:-everything} carefully. Focus on $2.
```

- `description` shows in the `/` picker; `argument-hint` after it.
- The body is submitted as the prompt after bash-style substitution:
  `$1`..`$9` positional, `$@` / `$ARGUMENTS` all args, `${N:-default}`,
  `${@:-default}`, `${@:2}` (args from the 2nd on). Quoted arguments group
  as one word.
- New files are picked up immediately — templates are read per use.

## Repo-local templates

A trusted repository can carry its own commands in `.e/prompts/`:
`<repo>/.e/prompts/<name>.md` becomes `/name`, same format as above. They load
only after `/trust`, like the repo's AGENTS.md, and shadow a global template
of the same name — the closer context wins.
