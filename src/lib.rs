//! e — a TUI for coding agents.
//!
//! Layout mirrors the reference design's tree: `core/` is the harness kernel
//! (engine-facing, no terminal dependencies), `ui/` is the frontend, with
//! `ui/render_engine/` holding the line-level machinery. `tools/` and
//! `builtins/` arrive with the agent loop.

pub mod core;
pub mod ui;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
