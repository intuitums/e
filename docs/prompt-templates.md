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
