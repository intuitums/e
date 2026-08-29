//! The terminal frontend.
//!
//! Grouped so the tree answers "what is this?":
//! - `paint/` — SGR, screen differ, theme, background, highlight
//! - `content/` — markdown, transcript, composer, statusline
//! - `surfaces/` — footer panels (picker, settings, auth, trust)
//! - `app/` — the interactive frame loop (extracted from the binary)
//!
//! Short paths (`tui::theme`, `tui::composer`, …) re-export from the groups
//! so call sites and tests stay readable.

pub mod app;
pub mod content;
pub mod paint;
pub mod surfaces;

pub use content::{composer, markdown, statusline, transcript};
pub use paint::{background, highlight, render, screen, theme};
pub use surfaces::{authpanel, menu, panel, questionpanel, settingspanel, trustpanel};
