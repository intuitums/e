## Checklist

- [ ] `cargo test` passes — the parity suite is the visual spec; if a rendering change fails it, fix the code, don't loosen the test
- [ ] `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are clean
- [ ] `./scripts/guard.sh` passes — if you had to change the guard itself (new network host, new write path, new unsafe block), say why in this PR
- [ ] `CHANGELOG.md` has an entry under `Unreleased` (skip only for pure meta: CI, templates, result files)
- [ ] Comments and docs touched by this change are updated
- [ ] Persisted/extension/CLI contract changes include compatibility fixtures and migration behavior
- [ ] Performance-sensitive changes pass `./x bench` and include before/after evidence when claiming an improvement
- [ ] Regression tests fail against the unfixed implementation for the intended reason
