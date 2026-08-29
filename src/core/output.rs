//! Display formatters: token counts, durations, model labels.
//! Shapes are byte-pinned by the parity tests.

/// Elapsed time in the activity-row grammar, the reference's compact shape:
/// `5s`, `18m0s`, `1h1m3s` — no separators, no zero padding, never a bare
/// `636s`. Lives in core so terminal-free code (wake records) shares the
/// exact shape the TUI prints.
pub fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m{}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    }
}

/// `42` → `42`, `9600` → `9.6k`, `15000` → `15k`, `999000` → `999k`.
pub fn format_tokens(tokens: u64) -> String {
    if tokens < 1000 {
        return tokens.to_string();
    }
    let whole = tokens / 1000;
    let tenths = (tokens % 1000) / 100;
    if whole < 10 && tenths > 0 {
        format!("{whole}.{tenths}k")
    } else {
        format!("{whole}k")
    }
}

/// `4s`, `2m 10s`, `1h 01m`.
pub fn format_duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3600 {
        return format!("{}m {}s", seconds / 60, seconds % 60);
    }
    format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
}

/// Compact USD estimate without rounding a small non-zero turn to `$0.00`.
pub fn format_cost(usd: f64) -> String {
    if usd < 0.01 {
        format!("${usd:.4}")
    } else {
        format!("${usd:.2}")
    }
}

/// `anthropic/claude-opus-4.7` → `opus 4.7`; anything else keeps its bare id.
pub fn compact_model_label(model: &str) -> String {
    let bare = model.rsplit('/').next().unwrap_or(model);
    if let Some(name) = bare.strip_prefix("claude-") {
        for family in ["opus", "sonnet", "haiku"] {
            if let Some(rest) = name.strip_prefix(&format!("{family}-")) {
                return format!("{family} {rest}");
            }
        }
        return name.to_string();
    }
    bare.to_string()
}
