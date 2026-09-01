//! e — a coding agent for your terminal.
//!
//! `core/` is the small, terminal-free harness. `tui/` is
//! the terminal frontend, grouped as paint / content / surfaces / app.
//!
//! The library target exists so the binary and integration tests share one
//! implementation, with the `sdk/` package (e-sdk) as a third in-repo
//! consumer. Its Rust items are not a stable third-party API by themselves;
//! the supported Rust surface is the `e-sdk` package behind the boundary
//! described in `docs/compatibility.md`, and e's other supported external
//! surfaces are the CLI, file formats, and extension wire protocol documented
//! there.

pub mod core;
pub mod tui;

/// The build's user-facing version, kept in sync with the `version` in
/// `Cargo.toml` — `scripts/release-check.sh` requires both to equal the
/// release tag before a `vX.Y.Z` tag can publish.
pub const VERSION: &str = "0.0.0";
