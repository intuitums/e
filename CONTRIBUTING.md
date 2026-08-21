# Contributing to e

This guide exists to save both sides time. Read [DESIGN.md](DESIGN.md) first —
it is short, and it decides what gets merged here.

## Philosophy

**e's core is minimal.** The whole harness is meant to be readable in an
afternoon, and that budget is a design input, not an aspiration.

If your feature does not need to live in the core, it should be an extension
([docs/extensions.md](docs/extensions.md)) or a file in `~/.e/` — a theme, a
skill, a prompt. PRs that grow the core where the extension surface would do
will be declined, kindly. Even new hook points for extensions are weighed
carefully: the protocol grows by need, never by symmetry.

The second pillar: **the look is law.** The visual design is pinned
byte-for-byte in `tests/`. If a rendering change fails the parity suite, the
fix is in your code, not in the test — loosening a pinned literal is a design
change and must be argued as one.

## The one rule

**You must understand your code.** If you cannot explain what your change
does and how it interacts with the rest of the system, the PR will be closed.

Using AI to write code is fine — this repo is substantially built that way.
Submitting generated code you have not read and cannot defend is not. If you
run an agent, run it from the repo root so it picks up `AGENTS.md`; your
agent must follow the rules in that file.

## Issues

Keep them short, concrete, and worth reading:

- Use the issue templates.
- If it does not fit on one screen, it is too long.
- Write in your own voice.
- State what happened, why it matters, and — for features — check
  [ROADMAP.md](ROADMAP.md) first: some things are deliberately out
  (DESIGN.md, "What e is not").
- If you want to implement it yourself, say so.

Low-signal, duplicate, or automated issues may be closed without discussion.

## Pull requests

Before opening a PR, all four of these must pass locally — CI runs the same:

```sh
cargo test                                   # the whole contract, incl. the visual spec
cargo fmt --check
cargo clippy --all-targets -- -D warnings
./scripts/guard.sh                           # the security-surface audit
```

The guard pins e's trust boundary: which hosts the binary may talk to, that
e reads only `~/.e/`, that credential writes go through the merge-write
store, where `unsafe` lives, and that CI actions stay SHA-pinned. If your
change legitimately moves one of those boundaries, change `guard.sh` in the
same diff and say why in the PR. A PR that trips the guard without touching
it will not merge.

A few conventions:

- Focused tests: each new test pins one behavior worth protecting. No smoke
  tests, no asserting the framework works.
- Update every comment and doc your change touches — a stale comment is
  worse than no comment.
- UI changes are verified with a real frame (`scripts/` has a pty
  capture-and-replay harness), not by reasoning about bytes.
- Do not edit `CHANGELOG.md`; entries are added by the maintainer.

Sensitive paths — the extension host (`src/core/api/`), auth, the store, the
provider wire code, `.github/` — additionally require code-owner review.
Green CI is not sufficient there.

By contributing you agree your work is licensed under [MIT](LICENSE).
