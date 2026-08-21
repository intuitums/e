//! `~/.e/settings.json`: preferences, read-merge-write so unrelated keys are
//! preserved. Typed accessors sit on top; readers elsewhere (model, prompt)
//! parse the same file independently.

use serde_json::Value;

use crate::core::home;

fn read() -> Value {
    std::fs::read_to_string(home::settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

fn write(value: &Value) {
    let _ = home::ensure();
    if let Ok(text) = serde_json::to_string_pretty(value) {
        let _ = std::fs::write(home::settings_path(), text);
    }
}

pub fn get_string(key: &str) -> Option<String> {
    read().get(key).and_then(|v| v.as_str().map(String::from))
}

/// Set one key, preserving the rest of the file.
pub fn set_string(key: &str, val: &str) {
    let mut value = read();
    value[key] = Value::String(val.to_string());
    write(&value);
}

/// A settings choice: a label, a category, and the options to cycle through.
/// Options are owned so some (theme) can be computed from `~/.e/` at runtime —
/// user themes are just files, not a compiled list.
pub struct Setting {
    pub key: String,
    pub label: String,
    pub category: &'static str,
    pub options: Vec<String>,
    pub default: String,
}

impl Setting {
    pub fn current(&self) -> String {
        get_string(&self.key)
            .filter(|v| self.options.iter().any(|o| o == v))
            .unwrap_or_else(|| self.default.clone())
    }
    /// Advance to the next option and persist it.
    pub fn cycle(&self, dir: i32) {
        let cur = self.current();
        let idx = self.options.iter().position(|o| *o == cur).unwrap_or(0) as i32;
        let n = self.options.len().max(1) as i32;
        let next = &self.options[(((idx + dir) % n + n) % n) as usize];
        set_string(&self.key, next);
    }
}

/// Theme names selectable today: `auto`, the built-ins, and any file in
/// `~/.e/themes/`. Editable — drop a `<name>.json` in and it appears here.
pub fn theme_names() -> Vec<String> {
    let mut names = vec!["auto".to_string(), "light".to_string(), "dark".to_string()];
    if let Ok(entries) = std::fs::read_dir(home::themes_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|x| x == "json").unwrap_or(false) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if !names.iter().any(|n| n == stem) {
                        names.push(stem.to_string());
                    }
                }
            }
        }
    }
    names
}

/// The settings shown in /settings, in category order.
pub fn all() -> Vec<Setting> {
    vec![
        Setting {
            key: "theme".into(),
            label: "Theme".into(),
            category: "Interface",
            options: theme_names(),
            default: "auto".into(),
        },
        Setting {
            key: "effort".into(),
            label: "Reasoning effort".into(),
            category: "Agent",
            options: vec!["low".into(), "medium".into(), "high".into()],
            default: "high".into(),
        },
    ]
}

pub fn effort() -> String {
    get_string("effort")
        .filter(|v| ["low", "medium", "high"].contains(&v.as_str()))
        .unwrap_or_else(|| "high".into())
}

pub fn theme() -> String {
    get_string("theme").unwrap_or_else(|| "auto".into())
}
