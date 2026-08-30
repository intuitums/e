//! e — a coding agent for your terminal.
//!
//! `core/` is the small, terminal-free harness. `tui/` is
//! the terminal frontend, grouped as paint / content / surfaces / app.
//!
//! The library target exists so the binary and integration tests share one
//! implementation. Its Rust items are not a stable third-party API; e's
//! supported external surface is the CLI, file formats, and extension wire
//! protocol documented in `docs/compatibility.md`.

pub mod core;
pub mod tui;

/// The build's user-facing version, kept in sync with the `version` in
/// `Cargo.toml` — `scripts/release-check.sh` requires both to equal the
/// release tag before a `vX.Y.Z` tag can publish.
pub const VERSION: &str = "0.0.1";
