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

/// The build's user-facing identity. While we dogfood, that's the `dogfood`
/// codename rather than a version number — when e ships for real this becomes
/// the released version. Cargo's manifest `version` stays a placeholder
/// because Cargo requires valid SemVer there.
pub const VERSION: &str = "dogfood";
