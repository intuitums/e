//! `~/.e/settings.json`: preferences, read-merge-write so unrelated keys are
//! preserved. Typed accessors sit on top; readers elsewhere (model, prompt)
//! parse the same file independently.

use serde_json::Value;

use crate::core::config::home;

/// Current shape of `settings.json`. Readers remain key-based and therefore
/// accept unversioned and future files without discarding unknown settings.
pub const FORMAT_VERSION: u32 = 1;

fn stamp_format(obj: &mut serde_json::Map<String, Value>) {
    obj.insert("format_version".into(), Value::from(FORMAT_VERSION));
}

pub fn get_string(key: &str) -> Option<String> {
    crate::core::config::store::read_object(&home::settings_path())
        .unwrap_or_default()
        .get(key)
        .and_then(|v| v.as_str().map(String::from))
}

/// A number key: absent means "no value set" — callers apply their own
/// built-in default.
pub fn get_u64(key: &str) -> Option<u64> {
    crate::core::config::store::read_object(&home::settings_path())
        .unwrap_or_default()
        .get(key)
        .and_then(|v| v.as_u64())
}

/// Set one key. Every other key on disk — known or not — is preserved, the
/// write is atomic, and a corrupt file is quarantined rather than reset.
pub fn set_string(key: &str, val: &str) -> std::io::Result<()> {
    crate::core::config::store::update_versioned(
        &home::settings_path(),
        0o644,
        FORMAT_VERSION,
        |obj| {
            stamp_format(obj);
            obj.insert(key.to_string(), Value::String(val.to_string()));
        },
    )
}

/// A string-list key: absent means "no value set" (for scoped models, that
/// reads as "no scope — everything available is in play").
pub fn get_strings(key: &str) -> Option<Vec<String>> {
    crate::core::config::store::read_object(&home::settings_path())
        .unwrap_or_default()
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
}

pub fn set_strings(key: &str, values: &[String]) -> std::io::Result<()> {
    crate::core::config::store::update_versioned(
        &home::settings_path(),
        0o644,
        FORMAT_VERSION,
        |obj| {
            stamp_format(obj);
            obj.insert(
                key.to_string(),
                Value::Array(values.iter().map(|v| Value::String(v.clone())).collect()),
            );
        },
    )
}

/// Remove a key entirely; every other key survives, as always.
pub fn remove(key: &str) -> std::io::Result<()> {
    crate::core::config::store::update_versioned(
        &home::settings_path(),
        0o644,
        FORMAT_VERSION,
        |obj| {
            stamp_format(obj);
            obj.remove(key);
        },
    )
}

/// The whole `extensions` object from settings — each entry namespaced by
/// extension name (`{"extensions":{"<name>":{…}}}`), passed to every
/// extension's initialize so none has to squat on a top-level key.
pub fn extensions_config() -> serde_json::Value {
    crate::core::config::store::read_object(&home::settings_path())
        .unwrap_or_default()
        .get("extensions")
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()))
}

/// A settings choice: a label, a category, and the options to cycle through./// Options are owned so some (theme) can be computed from `~/.e/` at runtime —
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
    pub fn cycle(&self, dir: i32) -> std::io::Result<()> {
        let cur = self.current();
        let idx = self.options.iter().position(|o| *o == cur).unwrap_or(0) as i32;
        let n = self.options.len().max(1) as i32;
        let next = &self.options[(((idx + dir) % n + n) % n) as usize];
        set_string(&self.key, next)
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

/// The settings shown in /settings, in category order. `effort_levels` are
/// the current model's declared effort levels — the cycle is whatever the
/// model supports, not a fixed list.
pub fn all(effort_levels: Vec<String>) -> Vec<Setting> {
    vec![
        Setting {
            key: "theme".into(),
            label: "Theme".into(),
            category: "Interface",
            options: theme_names(),
            default: "auto".into(),
        },
        Setting {
            key: "show_thinking".into(),
            label: "Show thinking".into(),
            category: "Interface",
            options: vec!["on".into(), "off".into()],
            default: "off".into(),
        },
        Setting {
            key: "auto_update".into(),
            label: "Auto-update".into(),
            category: "Updates",
            options: vec!["on".into(), "off".into()],
            default: "on".into(),
        },
        Setting {
            key: "effort".into(),
            label: "Reasoning effort".into(),
            category: "Agent",
            options: if effort_levels.is_empty() {
                vec!["low".into(), "medium".into(), "high".into()]
            } else {
                effort_levels
            },
            default: "high".into(),
        },
    ]
}

pub fn theme() -> String {
    get_string("theme").unwrap_or_else(|| "auto".into())
}

/// Whether the launch-time background update runs.
pub fn auto_update() -> bool {
    get_string("auto_update").as_deref() != Some("off")
}
