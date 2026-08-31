# A Rust SDK as its own package

Status: accepted
Date: 2026-08-30

## Context

Programmatic access to e's agent core today means driving the CLI (JSON
output, the JSONL RPC protocol) or speaking the extension wire protocol as a
child process. The library target's public Rust items are explicitly not a
stable API (`docs/compatibility.md`), which anticipated that a supported Rust
SDK would live behind an explicitly documented crate boundary with a
semantic-versioning policy.

## Decision

The SDK is a new package, `e-sdk`, in a top-level `sdk/` directory, as a
member of the root Cargo workspace and a path-dependency consumer of the `e`
library target. It is not part of `core/` and not an extension: it is a
second, in-repo consumer of the library target, alongside the binary and the
integration tests.

The API surface the SDK consumes is the SDK's contract. Stabilizing any part
of it is deliberate, per-release work documented in `docs/sdk.md`. The first
released SDK version declares the semantic-versioning policy; before that,
the SDK's surface is unstable and may change freely.

## Consequences

`cargo build`, `cargo test`, `cargo fmt`, and `./x check` cover the SDK with
no script changes, because it joins the root workspace. The library target
gains a third consumer (binary, tests, SDK), so visibility and API hygiene in
consumed modules now serve an in-repo user with a contract, not just a
convenience. `docs/compatibility.md` and `docs/architecture.md` name the SDK
as the boundary for in-process programmatic access; `fuzz/` keeps its own
workspace, as cargo-fuzz requires.
