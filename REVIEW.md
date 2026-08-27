# Review guide

Every review checks correctness, a focused diff, updated tests/docs, and the
repository contract in `./x check`. Apply the relevant situational checks
below; ordinary changes do not need to discuss every section.

## Behavioral changes

- Require an automated test for the externally observable behavior.
- For regressions, demonstrate that the new test fails against the unfixed
  implementation for the intended reason.
- Await observable conditions rather than sleeping. Use local servers on
  port 0 and isolated homes; tests must not contact public services.
- Preserve or deliberately replace existing assertions. A green test made
  less precise is still a regression.

## Persistence and protocols

- Preserve unknown fields and old fixtures. Reject unsupported future data
  explicitly instead of partially interpreting it.
- Update the format/protocol version only for incompatible changes; additive
  fields remain tolerant.
- Require migration behavior, compatibility documentation, a changelog entry,
  and an architecture decision for difficult-to-reverse changes.

## Security surface

- Check destinations for network, filesystem, subprocess, terminal, and
  credential changes. `scripts/guard.sh` should move only when the reviewed
  boundary intentionally moves.
- Diagnostic and error paths must not print secrets. Untrusted terminal text
  is sanitized before display.
- New `unsafe` blocks need a precise safety invariant and focused tests.

## Performance

- Performance claims include the command, before/after numbers, machine, and
  workload. `./x bench` is a regression ceiling, not proof of an improvement.
- Hot-path changes preserve linear or bounded behavior for long streams,
  transcripts, tool output, and provider payloads.

## Cross-platform and release

- Separate behavioral differences with explicit platform branches rather
  than weakening assertions everywhere.
- A release-affecting change keeps tag/version/changelog checks, locked
  builds, installer smoke tests, checksums, SBOM generation, and provenance
  intact.
