//! e-tui — the terminal frontend: renderer, frame, editor, menus.

pub mod app {
    pub mod blocks;
    pub mod editor;
    pub mod fmt;
    pub mod status;
}
pub mod render {
    pub mod ansi;
    pub mod highlight;
    pub mod markdown;
    pub mod theme;
}
pub mod term {
    pub mod screen;
}
