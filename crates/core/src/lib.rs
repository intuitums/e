//! e-core — the harness kernel: providers, agent loop, tools, sessions,
//! config, permissions. No terminal dependencies live here.
//!
//! M0 carries only the version; subsystems land milestone by milestone.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
