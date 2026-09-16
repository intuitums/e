# Instructions

e reads instructions from `AGENTS.md` files and puts them in the system
prompt, wrapped as project instructions with the file's path:

- `~/.e/AGENTS.md` — yours, for every project.
- `<workspace>/AGENTS.md` — the project's, loaded once the directory is
  trusted (`/trust`, or `e trust [dir]` when there is no terminal). A run
  refuses an untrusted workspace outright, because an untrusted repository
  could otherwise steer the agent.
- `<workspace>/<dir>/…/AGENTS.md` — nested instructions, loaded the first
  time a tool reads, writes, edits, or searches a path under that directory,
  as a message in the conversation. The nearest file arrives last, so it
  reads as the most specific. Each nested file loads once per session.

Nested files are how a monorepo keeps its rules local: `services/api/AGENTS.md`
is not in context until the agent touches `services/api/`, and then it is,
without anyone asking. They load only in a trusted workspace, and only for
paths inside it.

## Trust is the precondition

e runs in a directory whose own instructions you have accepted, or it does not
run there: a session that started untrusted would work in a repository with no
say in what the model was told. The first visit asks, with the terminal's trust
panel; declining exits and records nothing, so the next launch asks again.
`e trust [dir]` records the answer for a session with no terminal, and
`e untrust [dir]` refuses that directory deliberately. Trust extends to
everything inside a trusted ancestor, which is why the user's home directory is
never recorded as trusted.

Files are capped at 32 KiB each. Reasoning, skills, and prompt templates
have their own guides (`e docs skills`, `e docs prompt-templates`).
