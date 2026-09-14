//! Where the regions of the frame go: `~/.e/layout.json`, file-backed like
//! themes and keybindings, with a built-in default that reproduces e's
//! look. An extension opens a pane and proposes a side; the user's file
//! outranks it. The status row is a template of tokens, so what it says is
//! the user's choice too.
//!
//! ```json
//! {
//!   "split_min": 110,
//!   "focus": "ctrl+t",
//!   "banner": true,
//!   "panes": {
//!     "*":    { "side": "right", "width": 40 },
//!     "diff": { "side": "left",  "width": 50 }
//!   },
//!   "status": {
//!     "left":  ["{model} / {effort}", "{context}"],
//!     "right": ["{status}"]
//!   }
//! }
//! ```
//!
//! `panes` is keyed by pane id, `*` being the default for any pane; `side`
//! is `left` or `right`, `width` the pane's share of the terminal in
//! percent (30–70). Below `split_min` columns a focused pane fills the
//! screen and an unfocused one hides. `focus` is the chord that moves
//! focus between the conversation and the pane. `banner` hides the welcome
//! line when false.
//!
//! Status templates are lists of segments; each segment's `{tokens}`
//! expand and a segment whose tokens all came up empty is dropped, as is
//! a ` / ` part around an empty token, so `"{model} / {effort}"` reads
//! `model` alone for a model without an effort knob. Tokens: `{model}`,
//! `{effort}`, `{context}` (percent, blank under 1%), `{cwd}`, `{session}`,
//! `{status}` (every extension's slot, ` · ` joined), `{status:<ext>}`.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Which side of the conversation a pane sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

/// One pane's placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub side: Side,
    /// The pane's share of the terminal width, in percent.
    pub width: usize,
}

impl Default for Placement {
    fn default() -> Self {
        Placement {
            side: Side::Right,
            width: 40,
        }
    }
}

/// The loaded layout: defaults filled in for everything the file left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Columns needed to show a pane beside the conversation.
    pub split_min: usize,
    /// The chord that moves focus between the conversation and a pane.
    pub focus: String,
    /// Whether the welcome banner is shown.
    pub banner: bool,
    /// Per-pane placements, plus the `*` default.
    panes: BTreeMap<String, Placement>,
    pub status_left: Vec<String>,
    pub status_right: Vec<String>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            split_min: 110,
            focus: "ctrl+t".into(),
            banner: true,
            panes: BTreeMap::new(),
            status_left: vec!["{model} / {effort}".into(), "{context}".into()],
            status_right: vec!["{status}".into()],
        }
    }
}

impl Layout {
    /// Where a pane goes: its own entry, else `*`, else the built-in
    /// default, each field falling through separately. `proposed` is what
    /// the extension asked for; the user's file outranks it.
    pub fn placement(&self, id: &str, proposed: Option<Side>) -> Placement {
        let own = self.panes.get(id);
        let any = self.panes.get("*");
        let side = own
            .map(|p| p.side)
            .or(proposed)
            .or(any.map(|p| p.side))
            .unwrap_or(Placement::default().side);
        let width = own
            .or(any)
            .map(|p| p.width)
            .unwrap_or(Placement::default().width);
        Placement { side, width }
    }
}

#[derive(Deserialize)]
struct RawPlacement {
    side: Option<Side>,
    width: Option<u64>,
}

#[derive(Deserialize)]
struct RawStatus {
    left: Option<Vec<String>>,
    right: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct Raw {
    split_min: Option<u64>,
    focus: Option<String>,
    banner: Option<bool>,
    panes: Option<BTreeMap<String, RawPlacement>>,
    status: Option<RawStatus>,
}

/// Load `~/.e/layout.json`. A missing or malformed file fails open to the
/// defaults — a layout typo must never hide the conversation.
pub fn load() -> Layout {
    let Ok(json) = std::fs::read_to_string(crate::core::config::home::layout_path()) else {
        return Layout::default();
    };
    parse(&json).unwrap_or_default()
}

/// The layout a file's text describes; `None` when it is not JSON of the
/// expected shape.
pub fn parse(json: &str) -> Option<Layout> {
    let raw: Raw = serde_json::from_str(json).ok()?;
    let mut layout = Layout::default();
    if let Some(min) = raw.split_min {
        layout.split_min = usize::try_from(min).unwrap_or(usize::MAX).max(60);
    }
    if let Some(focus) = raw.focus {
        let chord = crate::core::config::keybindings::normalize_chord(&focus);
        if !chord.is_empty() {
            layout.focus = chord;
        }
    }
    if let Some(banner) = raw.banner {
        layout.banner = banner;
    }
    for (id, raw) in raw.panes.unwrap_or_default() {
        let base = layout.placement(&id, None);
        layout.panes.insert(
            id,
            Placement {
                side: raw.side.unwrap_or(base.side),
                width: raw
                    .width
                    .map(|w| usize::try_from(w).unwrap_or(70).clamp(30, 70))
                    .unwrap_or(base.width),
            },
        );
    }
    if let Some(status) = raw.status {
        if let Some(left) = status.left {
            layout.status_left = left;
        }
        if let Some(right) = status.right {
            layout.status_right = right;
        }
    }
    Some(layout)
}

/// Expand one status template: every `{token}` through `lookup`, ` / `
/// parts around an empty token dropped, and `None` when nothing is left.
pub fn expand(segment: &str, lookup: &dyn Fn(&str) -> String) -> Option<String> {
    let mut out = String::new();
    let mut rest = segment;
    let mut any_token = false;
    let mut any_value = false;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let token = &rest[start + 1..start + end];
        let value = lookup(token);
        any_token = true;
        any_value |= !value.is_empty();
        out.push_str(&value);
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    if any_token && !any_value {
        return None;
    }
    let parts: Vec<&str> = out
        .split(" / ")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let joined = parts.join(" / ");
    (!joined.is_empty()).then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_outranks_the_extension_and_the_default_fills_the_rest() {
        let layout = parse(
            r#"{"panes":{"*":{"width":45},"diff":{"side":"left"}},"focus":"Ctrl+Y","split_min":40}"#,
        )
        .unwrap();
        // The diff pane: its own side, the `*` width.
        assert_eq!(
            layout.placement("diff", Some(Side::Right)),
            Placement {
                side: Side::Left,
                width: 45
            }
        );
        // An unlisted pane: the extension's side, the `*` width.
        assert_eq!(
            layout.placement("plan", Some(Side::Left)),
            Placement {
                side: Side::Left,
                width: 45
            }
        );
        // Nothing said anywhere: the built-in default.
        assert_eq!(Layout::default().placement("x", None), Placement::default());
        assert_eq!(layout.focus, "ctrl+y");
        assert_eq!(layout.split_min, 60, "never below the floor");
        assert!(parse("{").is_none());
        assert_eq!(
            parse(r#"{"panes":{"x":{"width":900}}}"#)
                .unwrap()
                .placement("x", None)
                .width,
            70
        );
    }

    #[test]
    fn status_templates_drop_empty_tokens_and_their_separators() {
        let lookup = |token: &str| -> String {
            match token {
                "model" => "gpt-5".into(),
                "effort" | "context" => String::new(),
                "status" => "plan mode".into(),
                _ => String::new(),
            }
        };
        assert_eq!(
            expand("{model} / {effort}", &lookup).as_deref(),
            Some("gpt-5")
        );
        assert_eq!(expand("{context}", &lookup), None);
        assert_eq!(expand("{status}", &lookup).as_deref(), Some("plan mode"));
        assert_eq!(expand("plain text", &lookup).as_deref(), Some("plain text"));
        assert_eq!(expand("{unknown}", &lookup), None);
    }
}
