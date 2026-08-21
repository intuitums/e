//! e — a TUI for coding agents.
//!
//! `core/` is the harness — budgeted (DESIGN.md §3), terminal-free. `tui/` is
//! the terminal frontend: SGR styling, the markdown line renderer, the
//! diffing screen, the composer, transcript, and status line.

pub mod core;
pub mod tui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
