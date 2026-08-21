//! e — a TUI for coding agents.
//!
//! `core/` is the harness — budgeted (DESIGN.md §3), terminal-free. `tui/` is
//! the terminal frontend: SGR styling, the markdown line renderer, the
//! diffing screen, the composer, transcript, and status line.

pub mod core;
pub mod tui;

/// Width of styled text (ANSI-aware) — shared by the frame and the menus.
pub mod render_width {
    pub use crate::tui::markdown::visible_width;
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
