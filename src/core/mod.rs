//! The harness — terminal-free. `agent/` is the turn loop and its satellites
//! (compaction, the system prompt); `providers/` is the wire seam, the four
//! dialects, and the catalog; `auth/` holds credentials and the sign-in
//! flows; `config/` is the ~/.e surface (paths, the merge-write store,
//! settings, trust); `resources/` loads skills and prompt templates; `extensions/`
//! is the extension host; `tools/` the built-in tools.

pub mod agent;
pub mod auth;
pub mod cli;
pub mod config;
pub mod extensions;
pub mod providers;
pub mod resources;
pub mod session;
pub mod tools;
pub mod update;

pub mod output;
pub mod workspace;
