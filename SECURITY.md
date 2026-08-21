# Security Policy

This document explains e's security model and where its boundaries are, so
you can tell a vulnerability from expected behavior before reporting.

e is a coding agent that runs locally, inside the security boundary of the
user running it. It executes model-directed tools **without a permission
gate by default** — that is the documented design, not a flaw. It is the
user's responsibility to supervise it or contain it in a container, VM, or
sandbox.

e treats the local user account — and every file that account can write —
as inside the same trust boundary as the e process itself. If an attacker
can already modify your home directory, your workspace, your shell startup
files, or `~/.e/`, they can influence e and every other developer tool you
run; reports that depend on that prior access are not vulnerabilities
unless they show how e *grants* it or crosses an OS privilege boundary.

Likewise, e relies on the user installing trustworthy extensions and skills
and working in trusted repositories. A repository's `AGENTS.md`, code
comments, or skill files can trivially prompt-inject any coding agent; this
cannot be fully protected against and is out of scope.

What e **does** promise — the lines whose crossing is a vulnerability — is
pinned in `scripts/guard.sh` and enforced in CI:

- the binary talks only to its listed sign-in and model providers
- e reads only `~/.e/` — never another tool's store
- credentials are written `0600` through one atomic merge-write path
- extensions get exactly the protocol surface in
  [docs/extensions.md](docs/extensions.md), nothing more

## Reporting a vulnerability

Report privately through
[GitHub Security Advisories](https://github.com/fschrhunt/e/security/advisories/new).
Do not open a public issue for anything security-sensitive.

Please include:

- a description of the issue and its impact
- steps to reproduce, a proof of concept, or relevant logs
- the affected version or commit, and configuration if it matters
- any known mitigations

Reports are reviewed and disclosure coordinated as appropriate.

## In scope

- e sending data to a host outside its pinned provider set
- reads or writes outside `~/.e/` and the working directory that the user
  did not ask for
- an extension escalating beyond the documented protocol surface
- corruption or leakage of `auth.json` / `settings.json` caused by e itself
- vulnerabilities in the released binary or this repository's code and CI

## Out of scope

- local code execution and the absence of a sandbox (yolo is the default,
  by design)
- behavior of extensions or skills the user installed
- risks from working in untrusted repositories, including prompt injection
  via `AGENTS.md`, comments, or file contents
- reports requiring prior write access to the user's machine or home
  directory (dotfiles, `~/.e/`, workspace files, environment, shell config)
- malicious model output, and actions the user approved or initiated
- denial-of-service requiring trusted local input or config

## Notes for reporters

The most useful reports demonstrate a current, reproducible boundary
bypass with real impact against the latest `main` — the exact path, the
commit, and a proof of concept. A report showing that attacker-writable
trusted state changes e's behavior is expected under this model and will
be closed; a report showing how e hands out that access is exactly what
this policy is for.
