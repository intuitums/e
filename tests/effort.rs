//! Reasoning effort: the cycle is the model's own declared level list —
/// xhigh and friends appear when a model exposes them — never a fixed
/// low/medium/high ladder. Resolution falls back per model when the saved
/// value belongs to another model.
use e::core::agent::{effort, next_effort};

#[test]
fn cycles_whatever_the_model_declares() {
    let levels = vec!["low".into(), "medium".into(), "high".into(), "xhigh".into()];
    assert_eq!(next_effort(&levels, "low"), "medium");
    assert_eq!(next_effort(&levels, "medium"), "high");
    assert_eq!(next_effort(&levels, "high"), "xhigh");
    assert_eq!(next_effort(&levels, "xhigh"), "low");

    let two = vec!["minimal".to_string(), "max".to_string()];
    assert_eq!(next_effort(&two, "minimal"), "max");
}

#[test]
fn resolution_prefers_saved_then_model_default() {
    let levels = vec!["low".to_string(), "high".to_string(), "xhigh".to_string()];
    // Saved and supported wins verbatim.
    assert_eq!(effort(&levels, Some("low")), Some("low".into()));
    assert_eq!(effort(&levels, Some("xhigh")), Some("xhigh".into()));
    // Saved from another model (or unset): `high` when declared.
    assert_eq!(effort(&levels, Some("max")), Some("high".into()));
    assert_eq!(effort(&levels, None), Some("high".into()));
    // Without `high` in the list: the first declared level.
    let no_high = vec!["off".to_string(), "medium".to_string()];
    assert_eq!(effort(&no_high, None), Some("off".into()));
    // No knob at all.
    assert_eq!(effort(&[], Some("low")), None);
}
