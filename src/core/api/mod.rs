//! The extension API: executable subprocesses over a line protocol.
//!
//! An extension is any executable in `~/.e/extensions/`, in any language,
//! speaking the line protocol in `protocol.rs`. Extensions can add tools the
//! model calls (overriding built-ins by name), add slash commands, observe
//! lifecycle events, gate tool calls, and rewrite startup arguments through
//! hooks. There is no
//! embedded scripting runtime — the process boundary is the API, which keeps
//! the harness inside its budget and extensions in whatever language their
//! author likes.
//!
//! See docs/extensions.md for the protocol reference and a worked example.

mod host;
mod protocol;

pub use host::{ExtensionHost, StartupAction};
pub use protocol::{
    CommandResult, HookVerdict, InputVerdict, Manifest, ToolResult, PROTOCOL_VERSION,
};
