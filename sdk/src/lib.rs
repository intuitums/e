//! e-sdk — e's coding agent as a library.
//!
//! A [`Session`] is one conversation against one working directory, run by
//! the same core the terminal frontend drives: the built-in tools (read,
//! write, edit, grep, bash), skills and AGENTS.md context, automatic
//! compaction, on-disk session logs, and optionally the user's extensions.
//! A [`Turn`] is one prompt's run: an ordered stream of [`Event`]s that ends
//! in a [`Reply`].
//!
//! ```no_run
//! use e_sdk::{Event, Session};
//!
//! # async fn demo() -> Result<(), e_sdk::Error> {
//! let mut session = Session::builder().cwd("/path/to/project").build().await?;
//! let mut turn = session.prompt("What does this repository do?");
//! while let Some(event) = turn.next().await {
//!     if let Event::Text(delta) = event {
//!         print!("{delta}");
//!     }
//! }
//! let reply = turn.finish().await?;
//! println!("\n{} output tokens", reply.usage.output);
//! session.close().await;
//! # Ok(())
//! # }
//! ```
//!
//! What the types enforce:
//!
//! - **One turn at a time.** `prompt` borrows the session mutably for the
//!   turn's lifetime; a second prompt cannot start until the first has
//!   finished or been dropped. Mid-turn input goes through [`Turn::steer`].
//! - **Nothing is lost.** Events are backpressured, never buffered without
//!   bound; a slow consumer pauses the model, and a failed turn hands back
//!   its partial [`Reply`] inside the [`TurnError`].
//! - **Dropping a running turn interrupts it**, exactly like Esc in the
//!   terminal. The session stays usable.
//! - **Nothing touches `~/.e` unless asked.** Conversations are memory-only
//!   unless [`SessionBuilder::persist`] is set, extensions start only with
//!   [`SessionBuilder::extensions`], and the SDK never edits settings.
//!
//! The SDK needs a Tokio runtime: the core spawns its turn worker and runs
//! tool I/O on the blocking pool.
//!
//! Status: this surface is unstable until the first release declares its
//! semantic-versioning policy (`docs/extend/compatibility.md`).
// Same contract as the library it wraps: no panic sites outside test builds.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable
    )
)]

mod error;
mod session;
mod turn;

pub use error::{Error, TurnError};
pub use session::{Prompt, Session, SessionBuilder, Tools};
pub use turn::{Event, Reply, Stop, ToolStats, Turn};

/// One conversation record, as e persists it. The tagged `kind` separates
/// user, assistant, tool, and reasoning entries; this is the session file's
/// own message shape, so a saved history round-trips unchanged.
pub use e::core::providers::ChatMessage as Message;
/// An image attachment: a media type plus base64 data. `Image::from_path`
/// reads and validates a PNG, JPEG, GIF, or WebP file.
pub use e::core::providers::ImageInput as Image;
/// Token counts with disjoint categories: `input` excludes cache reads and
/// writes; `prompt_tokens()` is their sum.
pub use e::core::providers::Usage;
/// A session file on disk, as listed by [`SessionBuilder::saved`].
pub use e::core::session::SessionInfo as SavedSession;
pub use e::core::tools::{OutputStream, ToolOutcome};

/// Read a saved session's active conversation without taking ownership of
/// the file: the messages a `resume` of `path` would load. Pass them to
/// [`SessionBuilder::history`] to continue a transcript in memory only.
pub fn transcript(path: impl AsRef<std::path::Path>) -> Result<Vec<Message>, Error> {
    Ok(e::core::session::SessionLog::load(path.as_ref())?)
}
