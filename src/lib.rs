//! e — a coding agent for your terminal.
//!
//! `core/` is the harness — budgeted (DESIGN.md §3), terminal-free. `tui/` is
//! the terminal frontend, grouped as paint / content / surfaces / app.

pub mod core;
pub mod tui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
