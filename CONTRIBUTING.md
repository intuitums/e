# Contributing

e is a small, fast coding agent for the terminal: one Rust binary, no daemon,
no plugin runtime beyond executable JSONL extensions. That smallness is the
point, and it shapes what a good contribution looks like. The best ones solve
one clear problem, add the least code that solves it, and leave the repository
easy to verify. If a change needs an abstraction layer, a config option, or a
new protocol message to land, say why in the PR; if it doesn't, don't add one.

## Getting set up

```sh
git clone https://github.com/intuitums/e
cd e
cargo build
./x test
```

The Rust toolchain is pinned in `rust-toolchain.toml`; rustup installs the
right version on its own. A full build plus test run is the fastest way to
find out whether your machine is set up correctly.

## Reporting issues

Use the bug or feature issue form. Bug reports need a reproducible case and
the version (`e --version`, or the commit if you built from source). Feature
requests should explain the need before the design; an implementation sketch
is welcome but optional. A feature that could be an extension is usually
better as one — the extension API in [docs/extensions.md](docs/extensions.md)
exists precisely so most additions never have to touch the binary.

## Finding your way around

[docs/architecture.md](docs/architecture.md) is the guided tour. The short
version:

- `src/core/` — the harness. The turn loop and steering (`agent/`), the four
  provider wire dialects (`providers/`), credentials and sign-in (`auth/`),
  the `~/.e/` config store (`config/`), the extension host (`api/`), and the
  built-in tools (`tools/`).
- `src/tui/` — the frontend. Painting and theming (`paint/`), markdown and
  the composer (`content/`), panels and pickers (`surfaces/`), and the frame
  loop (`app/`).
- `src/main.rs` — CLI entry: flags, `e ask`, `e docs`, `e auth`, then the TUI.

Work from the repository root so your tools pick up `AGENTS.md`, the agent
guide. It carries the same conventions as this document, written for agents
editing the repo rather than humans opening PRs.

## Testing

`./x test` runs the whole behavioral contract and is what CI runs. You rarely
need all of it in a loop; each `tests/*.rs` file is its own binary:

```sh
cargo test --test stream      # the agent turn loop, against a mock provider
cargo test --test providers   # the wire dialects' request and stream shapes
cargo test --test parity      # byte-pinned terminal rendering
cargo test --test toolloop    # end-to-end tool execution
cargo test name_substring     # any single test, by name
```

The parity suite is the visual spec. Rendering fixtures are pinned
byte-for-byte against the reference design's own literals, so if a rendering
change makes them fail, the look drifted and the code is what changes. Don't
loosen the test to make a change pass.

New integration tests use the shared harness in `tests/common/`: `Home` for
an isolated `E_HOME`, `env_lock()` around anything env-global, and
`serve_sse` plus `test_model` for a mock provider. Don't hand-roll a second
mock harness; extend the existing one if it's missing something.

## Checks before you open a PR

```sh
./x check    # format, clippy, full test suite, security-surface guard
./x bench    # release-mode performance budgets
```

`./x` is the single definition of green; CI runs the same commands, so
nothing merges on a private definition of passing. `./x lint` and
`./x test <args>` give narrower loops while you work.

`scripts/guard.sh` pins the trust boundary: which hosts the binary may talk
to, that config writes go through the store, where `unsafe` lives, and that
CI actions are SHA-pinned. If your change legitimately moves a boundary, say
so in the PR and update the guard in the same commit. Never work around it.

For terminal UI changes, verify with a real frame before calling it done.
`scripts/ptycap.py` captures and replays one; reasoning about escape
sequences in your head is how rendering bugs ship.

## House rules

- Keep the diff focused. Unrelated cleanup belongs in its own PR, even when
  you spotted it mid-change.
- Add or update tests for behavior that changes. A regression test should
  fail against the unfixed code, for the intended reason.
- Update comments and docs your change touches. A stale comment is worse
  than none.
- Add a `CHANGELOG.md` entry under `Unreleased` for anything user-visible.
  CI, templates, and test result files don't need one.
- Comment functions, classes, and modules with a concise note on purpose and
  contract, not a line-by-line narration.

Persisted and user-facing contracts (CLI, sessions, configuration, the
extension protocol) follow the compatibility policy in
[docs/compatibility.md](docs/compatibility.md). Fixtures under
`tests/fixtures/` are release artifacts: once committed, newer readers must
keep loading them or the PR documents the migration. When a change touches
one of those contracts, add or update a retained fixture in the same PR.

## Review

Every change needs the maintainer's review. Paths that form the trust
boundary — the extension host, authentication, the config store, provider
wire code, session persistence, `install.sh`, and `.github/` — are called out
in [CODEOWNERS](.github/CODEOWNERS) and cannot merge on green checks alone.

Describe the PR in whatever format fits the change; the template only carries
the checklist CI and review expect.

## License

By contributing, you agree that your work is released under the repository's
[MIT license](LICENSE).
