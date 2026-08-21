//! `~/.e/settings.json`: preferences, read-merge-write so unrelated keys are
//! preserved. Typed accessors sit on top; readers elsewhere (model, prompt)
//! parse the same file independently.

use serde_json::Value;

use crate::core::config::home;

pub fn get_string(key: &str) -> Option<String> {
    crate::core::config::store::read_object(&home::settings_path())
        .get(key)
        .and_then(|v| v.as_str().map(String::from))
}

/// Set one key. Every other key on disk — known or not — is preserved, the
/// write is atomic, and a corrupt file is quarantined rather than reset.
pub fn set_string(key: &str, val: &str) {
    let _ = crate::core::config::store::update(&home::settings_path(), 0o644, |obj| {
        obj.insert(key.to_string(), Value::String(val.to_string()));
    });
}

/// A string-list key: absent means "no value set" (for scoped models, that
/// reads as "no scope — everything available is in play").
pub fn get_strings(key: &str) -> Option<Vec<String>> {
    crate::core::config::store::read_object(&home::settings_path())
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
}

pub fn set_strings(key: &str, values: &[String]) {
    let _ = crate::core::config::store::update(&home::settings_path(), 0o644, |obj| {
        obj.insert(
            key.to_string(),
            Value::Array(values.iter().map(|v| Value::String(v.clone())).collect()),
        );
    });
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
