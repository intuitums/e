//! e — a coding agent for your terminal.
//!
//! `core/` is the small, terminal-free harness. `tui/` is the terminal
//! frontend, grouped as paint / content / surfaces / app. `rpc/` is the
//! headless one: the JSONL session server behind `e rpc`.
//!
//! The library target exists so the binary and integration tests share one
//! implementation, with the `sdk/` package (e-sdk) as a third in-repo
//! consumer. Its Rust items are not a stable third-party API by themselves;
//! the supported Rust surface is the `e-sdk` package behind the boundary
//! described in `docs/extend/compatibility.md`, and e's other supported external
//! surfaces are the CLI, file formats, and extension wire protocol documented
//! there.
//
// Shipped code denies explicit panic sites outside test builds. Every allowed
// site needs a proof comment explaining why runtime input cannot reach it.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable
    )
)]

pub mod core;
pub mod rpc;
pub mod tui;

/// Release identity supplied by the release workflow; local Cargo builds keep the manifest version.
pub const VERSION: &str = env!("E_VERSION");
/// Update and state boundary. Local and PR builds never follow a release channel.
pub const CHANNEL: &str = env!("E_CHANNEL");
/// Exact source revision for published builds, or `local` for ordinary Cargo builds.
pub const COMMIT: &str = env!("E_COMMIT");

/// The client name e identifies itself with to gateways that recognize their
/// callers (sent as the provider's declared `client_header`, e.g. OpenCode's
/// `x-opencode-client`). Honest identity — e names itself, it does not
/// impersonate another client.
pub const CLIENT: &str = "e";
