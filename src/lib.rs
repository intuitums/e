//! e — a TUI for coding agents.
//!
//! The tree speaks e's own vocabulary (DESIGN.md): `kernel/` is the budgeted,
//! terminal-free harness; `ui/` is the frontend, with `ui/frame/` holding the
//! line machinery — markdown folded to styled lines, syntax tinting, and the
//! diffing painter.

pub mod kernel;
pub mod ui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
