//! The extension API: executable subprocesses over a line protocol.
//!
//! An extension is any executable in `~/.e/extensions/` — a top-level file, or
//! the entry point of a subdirectory that bundles its own files — in any
//! language, speaking the line protocol in `protocol.rs`. Extensions can add tools the
//! model calls (overriding built-ins by name), add slash commands, observe
//! lifecycle events, gate tool calls, and rewrite startup arguments through
//! hooks. There is no
//! embedded scripting runtime — the process boundary is the API, which keeps
//! the harness inside its budget and extensions in whatever language their
//! author likes.
//!
//! See docs/customize/extensions.md for the protocol reference and a worked example.

mod host;
mod protocol;

pub use host::{
    normalize_chord, read_bounded_line, shortcut_allowed, BeforeTurn, ExtensionHost, HostRequest,
    StartupAction, ToolProgress,
};
pub use protocol::{
    parse_incoming, BeforeTurnResult, CommandResult, Completion, Format, HookVerdict, Incoming,
    InjectedMessage, InputVerdict, Manifest, ShortcutDecl, Show, ToolLabel, ToolResult,
    CAPABILITIES, EVENTS, MAX_SHOW_BYTES, PROTOCOL_VERSION,
};
