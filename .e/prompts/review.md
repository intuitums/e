---
description: Review changes in this repo — a PR, branch, or uncommitted work
argument-hint: "[base ref]"
---

# Review changes

Base ref: `${ARGUMENTS:-<auto-detect>}`

Read-only review. Do not edit, stage, commit, push, modify a PR, install
dependencies, run fixing migrations, publish, or deploy. Remote fetches are
allowed to establish a current base. Never expose credentials or claim an
unrun check passed.

## Scope

1. Choose the broadest meaningful scope:
   - Current branch has a PR → review the PR (head vs base).
   - Staged, unstaged, or untracked changes → review the working tree.
   - Otherwise → review branch commits against the base.

2. Resolve the base ref in order:
   - Supplied base ref (must resolve to a valid Git ref).
   - Existing PR base branch.
   - Remote default branch (`upstream`, then `origin`).
   - Plausible trunk: `main`, `master`, `develop`, `trunk`.
   - Ask when several candidates remain plausible.

   Do not confuse the feature branch's tracking branch with its PR base.
   Refresh the chosen remote base with a bounded fetch when possible; call
   out stale base otherwise.

3. Note branch, HEAD, status, how the base was chosen, merge base, commits
   and files in range, and ahead/behind state.

4. Keep separate throughout:
   - **Change under review** — committed diff in the merge-base range, or
     working-tree changes when reviewing uncommitted work.
   - **Local-only work** — staged, unstaged, or untracked changes not part
     of the committed change.
   - **Pre-existing defects** — failures not introduced by this change.

   Local-only work matters only when the change depends on it. Never blame
   pre-existing failures without evidence.

If this is not a Git repository or no defensible base can be established, stop
and explain what is missing.

## Inspect

5. Read the full relevant diff and enough surrounding code to understand the
   change. Read applicable repository instructions and infer intent from the
   request, branch, commits, diff, and any linked PR.

6. Focus on real defects:
   - conflict residue, credentials, or accidental artifacts;
   - broken, unsafe, debug, or unfinished behavior that can ship;
   - missing migrations, config, docs, or generated files;
   - dependency or lockfile changes that don't fit the intent;
   - meaningful regressions without focused coverage;
   - unrelated changes that make the change unsafe or hard to review;
   - local-only work the committed change depends on.

7. Use a read-only subagent only when the diff is large or specialized review
   would help. Verify delegated findings yourself. Do not report speculation.

## Repo checks

Every review checks correctness, a focused diff, updated tests/docs, and the
repository contract in `./x check`. Apply the situational checks that match
the change; ordinary changes do not need to discuss every section.

### Behavioral changes

- Require an automated test for the externally observable behavior.
- For regressions, demonstrate that the new test fails against the unfixed
  implementation for the intended reason.
- Await observable conditions rather than sleeping. Use local servers on
  port 0 and isolated homes; tests must not contact public services.
- Preserve or deliberately replace existing assertions. A green test made
  less precise is still a regression.

### Persistence and protocols

- Preserve unknown fields and old fixtures. Reject unsupported future data
  explicitly instead of partially interpreting it.
- Update the format/protocol version only for incompatible changes; additive
  fields remain tolerant.
- Require migration behavior, compatibility documentation, a changelog entry,
  and an architecture decision for difficult-to-reverse changes.

### Security surface

- Check destinations for network, filesystem, subprocess, terminal, and
  credential changes. `scripts/guard.sh` should move only when the reviewed
  boundary intentionally moves.
- Diagnostic and error paths must not print secrets. Untrusted terminal text
  is sanitized before display.
- New `unsafe` blocks need a precise safety invariant and focused tests.

### Performance

- Performance claims include the command, before/after numbers, machine, and
  workload. `./x bench` is a regression ceiling, not proof of an improvement.
- Hot-path changes preserve linear or bounded behavior for long streams,
  transcripts, tool output, and provider payloads.

### Cross-platform and release

- Separate behavioral differences with explicit platform branches rather
  than weakening assertions everywhere.
- A release-affecting change keeps tag/version/changelog checks, locked
  builds, installer smoke tests, checksums, SBOM generation, and provenance
  intact.

## Validate

8. Run the smallest useful non-fixing checks the repo already supports
   (`AGENTS.md`, package scripts, CI config). Do not install tools or start
   credentialed or external services.

9. For each check, record the command and outcome. Attribute failures to the
   change only when the diff or a focused reproduction shows it. Unrun checks
   are a concern unless equivalent CI passed on the exact reviewed commit.

10. When reviewing a PR, inspect forge state for the exact HEAD commit:
    - mergeability and conflicts;
    - draft status;
    - required CI on exact HEAD;
    - required approvals or change requests;
    - unresolved review threads when reliably available.

    Do not count stale CI or unavailable integrations as passing.

11. Recheck the working tree after validation. Report anything the review
    created.

## Report

12. Lead with the most important finding. Include only non-empty sections,
    ordered by impact:

    - **Fix first** — blocking issues with `path:line` evidence.
    - **Issues** — non-blocking issues.
    - **Checks** — commands and outcomes.
    - **Not verified** — skipped checks, stale base/CI, coverage limits.
    - **Scope** — what was reviewed, base, commits/files, change vs
      local-only work, final tree state.

    No severity labels, raw diffs, long logs, empty sections, or generic
    praise. Keep it short.
