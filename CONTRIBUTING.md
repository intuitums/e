# Contributing

Thanks for wanting to improve e. Read [DESIGN.md](DESIGN.md) first — it is
short, and it explains what gets merged here and what doesn't.

## The three gates

Every PR runs three checks, and all of them are runnable locally:

```sh
cargo test                # the whole contract, including the visual spec
cargo fmt --check         # rustfmt, stock settings
cargo clippy --all-targets -- -D warnings
./scripts/guard.sh        # the security-surface audit
```

**The parity suite is the design document.** The look is pinned
byte-for-byte in `tests/`. If your change fails a parity test, the fix is in
your code, not in the test — loosening a pinned literal is a design change
and needs to be argued as one.

**The guard is the trust boundary.** `scripts/guard.sh` pins the promises e
makes to its users: which network hosts the binary may talk to, that e reads
only `~/.e/` and never another tool's store, that credential and config
writes go through the merge-write path in `core/store.rs`, that `unsafe`
stays in its one audited file, and that CI actions are pinned by commit SHA.
A PR that moves one of these boundaries must change `guard.sh` in the same
diff — deliberately, visibly, with a sentence in the PR saying why. A PR
that trips the guard without touching it will not merge.

## What gets merged

- **Small over general.** Fewer moving parts wins. No abstraction layers or
  configuration beyond what the change in front of you needs.
- **Extensible over hardcoded.** If a user could sensibly prefer a different
  look, wording, or behaviour, read it from `~/.e/` with a built-in default
  (DESIGN.md §2). For code-level needs there is the extension API
  ([docs/extensions.md](docs/extensions.md)) — grow its protocol by need,
  never by symmetry, and keep hooks fail-open.
- **Focused tests.** Each new test pins one behavior worth protecting. No
  smoke tests, no asserting the framework works.
- **Inside the budget.** The harness stays readable in an afternoon
  (DESIGN.md §3). A feature that can't pay for its complexity stays out —
  check [ROADMAP.md](ROADMAP.md) under "Not planned" before proposing MCP,
  subagents, or an embedded runtime.

Sensitive paths — the extension host (`src/core/api/`), auth, the store,
the provider wire code, and `.github/` — additionally require code-owner
review; CI green is not sufficient there.

## Practical notes

- UI changes are verified with a real frame, not by reasoning about bytes:
  `scripts/` has a pty capture-and-replay harness.
- Update every comment and doc your change touches. A stale comment is
  worse than no comment.
- By contributing you agree your work is licensed under [MIT](LICENSE).
