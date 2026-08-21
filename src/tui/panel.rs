//! The shared frame for footer surfaces — the picker and the settings screen.
//!
//! One `─` divider above, the header, a blank, the body, one `─` divider
//! below. The hint always rides the status row, never the panel, so every
//! interface is framed identically and no surface grows a second nav line.

use crate::tui::theme::Theme;

pub fn frame(theme: &Theme, width: usize, header: String, body: Vec<String>) -> Vec<String> {
    // fx colours dividers with divider_style (240 dark / 250 light), dimmer
    // than body dim — the  token carries exactly those values.
    let divider = theme.fg("border", &"─".repeat(width));
    let mut out = Vec::with_capacity(body.len() + 4);
    out.push(divider.clone());
    out.push(header);
    out.push(String::new());
    out.extend(body);
    out.push(divider);
    out
}
