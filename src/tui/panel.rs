//! The shared frame for footer surfaces — the picker and the settings screen.
//!
//! One `─` divider above, the header, a blank, the body, one `─` divider
//! below. The hint always rides the status row, never the panel, so every
//! interface is framed identically and no surface grows a second nav line.

use crate::tui::theme::Theme;

pub fn frame(theme: &Theme, width: usize, header: String, body: Vec<String>) -> Vec<String> {
    let divider = theme.fg("dim", &"─".repeat(width));
    let mut out = Vec::with_capacity(body.len() + 4);
    out.push(divider.clone());
    out.push(header);
    out.push(String::new());
    out.extend(body);
    out.push(divider);
    out
}
