# Architecture decisions

Use a numbered Markdown file such as `0001-session-format-version.md` with:

```text
# Title
Status: proposed | accepted | superseded
Date: YYYY-MM-DD

## Context
## Decision
## Consequences
```

Record only choices that are expensive to reverse: persistence and wire
formats, trust boundaries, process architecture, or cross-cutting invariants.
Keep implementation discussion in the pull request.
