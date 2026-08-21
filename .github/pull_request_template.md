## What

<!-- One or two sentences. What changes, and why. -->

## Checklist

- [ ] `cargo test` passes — the parity suite is the visual spec; if a rendering change fails it, fix the code, don't loosen the test
- [ ] `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are clean
- [ ] `./scripts/guard.sh` passes — if you had to change the guard itself (new network host, new write path, new unsafe block), say why in this PR
- [ ] Comments and docs touched by this change are updated
