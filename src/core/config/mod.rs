//! The `~/.e` surface: where the home lives, the merge-write store that
//! keeps it safe, settings, per-directory trust, the composer keymap, and
//! the frame layout.

pub mod home;
pub mod keybindings;
pub mod layout;
pub mod settings;
pub mod store;
pub mod trust;
