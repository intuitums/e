//! The `~/.e` surface: where the home lives, the merge-write store that
//! keeps it safe, settings, and per-directory trust.

pub mod home;
pub mod settings;
pub mod store;
pub mod trust;
