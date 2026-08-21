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
            let hex = &s[1..];
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
}

/// Load one of the two bundled palettes from the repo's `themes/` directory.
pub fn load_bundled(light: bool) -> Result<Theme, String> {
    let name = if light { "light" } else { "dark" };
    let path = concat_repo_path(&format!("themes/{name}.json"));
    let json = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    Theme::from_json(&json)
}

fn concat_repo_path(rel: &str) -> String {
    // Resolved relative to the crate at build time; works for `cargo run`
    // and tests. The release binary embeds the themes instead (see M8).
    format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel)
}
