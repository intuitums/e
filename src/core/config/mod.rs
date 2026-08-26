//! The `~/.e` surface: where the home lives, the merge-write store that
//! keeps it safe, settings, per-directory trust, and the composer keymap.

pub mod home;
pub mod keybindings;
pub mod settings;
pub mod store;
pub mod trust;
