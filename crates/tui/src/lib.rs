//! e-tui — the terminal frontend: renderer, frame, editor, menus.

pub mod app {
    pub mod fmt;
}
pub mod render {
    pub mod ansi;
    pub mod highlight;
    pub mod markdown;
    pub mod theme;
}
