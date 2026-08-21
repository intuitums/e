//! e — a TUI for coding agents.
//!
//! `core/` is the harness — budgeted (DESIGN.md §3), terminal-free. `ui/` is
//! the frontend: SGR styling, the markdown line renderer, the diffing screen,
//! the composer, transcript, and status line.

pub mod core;
pub mod ui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
