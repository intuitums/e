//! Theme loading: `themes/{light,dark}.json` → a resolved palette.
//!
//! The files carry a `vars` block (eight grayscale xterm-256 values — the
//! entire ramp) and a `colors` block mapping ~50 semantic tokens to either a
//! var name, a direct value, or `""` for the terminal default. The parity
//! tests pin both the var values and that light/dark are structural mirrors.

use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
struct ThemeFile {
    vars: HashMap<String, serde_json::Value>,
    colors: HashMap<String, serde_json::Value>,
}

#[derive(Clone, Debug)]
pub struct Theme {
    /// token → SGR foreground prefix ("" tokens map to the default-fg reset).
    fg: HashMap<String, String>,
    /// raw var values, kept for tests and lightness inference.
    pub vars: HashMap<String, i64>,
}

fn fg_code(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(n) => Some(format!("\x1b[38;5;{}m", n.as_i64()?)),
        serde_json::Value::String(s) if s.is_empty() => Some("\x1b[39m".to_string()),
        serde_json::Value::String(s) if s.starts_with('#') => {
            // Exactly six ASCII hex digits after '#'; anything else is a
            // user-edit typo and must fall back, not panic on a slice.
            let hex = &s[1..];
            if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let (r, g, b) = (
                u8::from_str_radix(&hex[0..2], 16).ok()?,
                u8::from_str_radix(&hex[2..4], 16).ok()?,
                u8::from_str_radix(&hex[4..6], 16).ok()?,
            );
            Some(format!("\x1b[38;2;{r};{g};{b}m"))
        }
        _ => None,
    }
}

impl Theme {
    pub fn from_json(json: &str) -> Result<Self, String> {
        let file: ThemeFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let mut vars = HashMap::new();
        for (name, value) in &file.vars {
            if let Some(n) = value.as_i64() {
                vars.insert(name.clone(), n);
            }
        }
        let mut fg = HashMap::new();
        for (token, value) in &file.colors {
            // A color is a var reference, or a literal (number / hex / "").
            let resolved = match value {
                serde_json::Value::String(s) if file.vars.contains_key(s.as_str()) => {
                    fg_code(&file.vars[s.as_str()])
                }
                other => fg_code(other),
            };
            if let Some(code) = resolved {
                fg.insert(token.clone(), code);
            }
        }
        Ok(Theme { fg, vars })
    }

    /// Wrap text in the token's foreground, closing with the default-fg reset.
    pub fn fg(&self, token: &str, text: &str) -> String {
        match self.fg.get(token) {
            Some(code) if code != "\x1b[39m" => format!("{code}{text}\x1b[39m"),
            _ => text.to_string(),
        }
    }

    /// The raw SGR prefix for a token ("" for default).
    pub fn fg_prefix(&self, token: &str) -> &str {
        match self.fg.get(token) {
            Some(code) if code != "\x1b[39m" => code,
            _ => "",
        }
    }

    /// The token's value as a background prefix ("" for default). The theme
    /// files carry one value per token; whether it paints ink or ground is
    /// the caller's choice — the reference's filled selection rows use the
    /// same palette entries as backgrounds.
    pub fn bg_prefix(&self, token: &str) -> String {
        match self.fg.get(token) {
            Some(code) if code != "\x1b[39m" => code.replacen("\x1b[38;", "\x1b[48;", 1),
            _ => String::new(),
        }
    }

    /// The diff-marker token for one side of a diff: the truecolor value when
    /// the terminal advertises 24-bit color, the reference's 256-color
    /// fallback otherwise.
    pub fn diff_marker_token(added: bool) -> &'static str {
        let truecolor = std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false);
        match (added, truecolor) {
            (true, true) => "toolDiffAddedMarker",
            (true, false) => "toolDiffAddedMarkerFallback",
            (false, true) => "toolDiffRemovedMarker",
            (false, false) => "toolDiffRemovedMarkerFallback",
        }
    }
}

/// The two palettes are compiled into the binary — no runtime files, no
/// themes directory. The raw JSON is exposed so tests can assert on it.
pub const LIGHT_JSON: &str = include_str!("theme_light.json");
pub const DARK_JSON: &str = include_str!("theme_dark.json");

pub fn bundled_json(light: bool) -> &'static str {
    if light {
        LIGHT_JSON
    } else {
        DARK_JSON
    }
}

/// Load one of the two bundled palettes.
pub fn load_bundled(light: bool) -> Result<Theme, String> {
    Theme::from_json(bundled_json(light))
}

/// A user theme from `~/.e/themes/<name>.json`, if present and valid.
pub fn load_user(name: &str) -> Option<Theme> {
    let path = crate::core::config::home::themes_dir().join(format!("{name}.json"));
    let json = std::fs::read_to_string(path).ok()?;
    Theme::from_json(&json).ok()
}

/// Resolve the effective theme for a selection and a detected background.
/// `~/.e/themes/<name>.json` wins over the built-ins for any name — so even
/// `light`/`dark` are overridable — falling back to the embedded pair.
pub fn resolve(selection: &str, detected_light: bool) -> Theme {
    let name = if selection == "auto" {
        if detected_light {
            "light"
        } else {
            "dark"
        }
    } else {
        selection
    };
    if let Some(theme) = load_user(name) {
        return theme;
    }
    let light = name == "light" || (name != "dark" && detected_light);
    // The dark theme is embedded in this binary; if it failed to parse,
    // that is a build bug CI catches, not a runtime state. Scoped allow,
    // proof: compile-time data.
    #[allow(clippy::expect_used)]
    load_bundled(light).unwrap_or_else(|_| load_bundled(false).expect("embedded dark"))
}
