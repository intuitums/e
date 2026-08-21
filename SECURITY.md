# Security

## Reporting

Report vulnerabilities privately via
[GitHub security advisories](https://github.com/fschrhunt/e/security/advisories/new)
— not in a public issue. You'll get a response within a few days.

## What counts

e holds credentials (`~/.e/auth.json`, written `0600`), runs model-directed
tools with no permission gate by default, and executes user-installed
extensions as subprocesses. Reports are especially welcome for anything that:

- makes e send data to a host outside its pinned provider set
  (`scripts/guard.sh` lists them)
- reads or writes outside `~/.e/` and the working directory in a way the
  user didn't ask for
- lets a repository's files (AGENTS.md, skills) or a malicious extension
  escalate beyond their documented surface
- corrupts or leaks `auth.json` / `settings.json`

## What's out of scope

e runs the model's tool calls without asking — that is the documented
default (yolo), not a vulnerability. Extensions are trusted code the user
installed; that they *can* do powerful things is by design. What they must
not be able to do is exceed the protocol surface documented in
[docs/extensions.md](docs/extensions.md).
