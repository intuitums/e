//! The skill tool: page a SKILL.md body into the conversation by name.

use serde_json::{json, Value};
use std::path::Path;

use super::{schema_object, truncate, ToolOutcome, ToolOutput};
use crate::core::resources::skills;

pub fn schema() -> Value {
    schema_object(
        "skill",
        "Load a skill's instructions by name. Use when a listed skill fits the task.",
        json!({"name": {"type": "string"}}),
        &["name"],
    )
}

pub fn run(args: &Value, cwd: &Path) -> ToolOutput {
    let Some(name) = args["name"].as_str() else {
        return ToolOutput {
            content: "skill: missing name".into(),
            outcome: ToolOutcome::Failed,
            summary: "error".into(),
        };
    };
    match skills::get(name, cwd) {
        Some(skill) if !skill.disable_model_invocation => ToolOutput {
            content: truncate(skill.body),
            outcome: ToolOutcome::Completed,
            summary: name.into(),
        },
        Some(_) => ToolOutput {
            content: format!("skill '{name}' is user-only"),
            outcome: ToolOutcome::Blocked,
            summary: "denied".into(),
        },
        None => ToolOutput {
            content: format!("no skill named '{name}'"),
            outcome: ToolOutcome::Failed,
            summary: "not found".into(),
        },
    }
}
