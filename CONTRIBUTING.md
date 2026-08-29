# Contributing

Contributions should solve one clear problem and leave the repository easy to
verify.

## Issues and pull requests

Use the bug or feature issue form when reporting a problem or proposing
behavior. Bug reports need a reproducible case and version; feature requests
should explain the need. An implementation sketch is optional.

Describe a pull request in whatever format fits the change. The PR template
only supplies the checks that must be completed.

## Working in the repository

- Run your tools from the repository root so they pick up `AGENTS.md`.
- Keep the diff focused. Avoid unrelated cleanup.
- Add or update tests for behavior that changes.
- Update user documentation and code comments when their subject changes.
- Verify terminal UI changes with the capture-and-replay tools in `scripts/`.
- Add a `CHANGELOG.md` entry under `Unreleased`, except for CI, templates,
  and result files.

## Checks

Run the same checks as CI before opening a pull request:

```sh
./x check
./x bench
```

The repository pins its Rust toolchain in `rust-toolchain.toml`. `./x` is the
single definition of local and CI verification; `./x test <cargo-test-args>`
and `./x lint` provide narrower development loops.

`cargo test` includes byte-level terminal rendering tests. If an intentional
UI change alters those fixtures, make the reason clear in the change.

`scripts/guard.sh` checks security-sensitive boundaries such as network
hosts, configuration writes, uses of `unsafe`, and pinned CI actions. Update
the guard only when the boundary itself is intentionally changing.

Changes to the extension host, authentication, configuration store, provider
wire code, and `.github/` require code-owner review.

Any supported-surface change
must follow [the compatibility policy](docs/compatibility.md) and update or add
a retained fixture.

By contributing, you agree that your work is licensed under [MIT](LICENSE).
