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

/// A choice among a fixed set of string options, cycled in settings.
pub struct Choice {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub options: &'static [&'static str],
    pub default: &'static str,
}

impl Choice {
    pub fn current(&self) -> String {
        get_string(self.key)
            .filter(|v| self.options.contains(&v.as_str()))
            .unwrap_or_else(|| self.default.to_string())
    }
    /// Advance to the next option and persist it.
    pub fn cycle(&self, dir: i32) {
        let cur = self.current();
        let idx = self.options.iter().position(|o| *o == cur).unwrap_or(0) as i32;
        let n = self.options.len() as i32;
        let next = self.options[(((idx + dir) % n + n) % n) as usize];
        set_string(self.key, next);
    }
}

/// e's fixed-option settings, grouped for the /settings screen.
pub const THEME: Choice = Choice {
    key: "theme",
    label: "Theme",
    description: "auto follows the terminal background",
    options: &["auto", "light", "dark"],
    default: "auto",
};

pub const EFFORT: Choice = Choice {
    key: "effort",
    label: "Reasoning effort",
    description: "for models that support it",
    options: &["low", "medium", "high"],
    default: "high",
};

pub fn effort() -> String {
    EFFORT.current()
}
