//! e — a coding agent for your terminal.
//!
//! `core/` is the small, terminal-free harness. `tui/` is
//! the terminal frontend, grouped as paint / content / surfaces / app.

pub mod core;
pub mod tui;

/// The build's user-facing identity. While we dogfood, that's the `dogfood`
/// codename rather than a version number — when e ships for real this becomes
/// the released version. Cargo's manifest `version` stays a placeholder
/// because Cargo requires valid SemVer there.
pub const VERSION: &str = "dogfood";
