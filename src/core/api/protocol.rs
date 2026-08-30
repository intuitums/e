//! The extension wire protocol: JSON, one message per line, over the
//! extension process's stdin/stdout.
//!
//! e → extension requests (each expects a response with the same `id`):
//!   {"id":1,"method":"initialize","params":{"protocol":1,"capabilities":["tool.update"],"e_version":"…","cwd":"…","config":{…}}}
//!   {"id":7,"method":"tool_call","params":{"name":"…","arguments":{…}}}
//!   {"id":9,"method":"command","params":{"name":"…","args":"…"}}
//!   {"id":2,"method":"hook.startup","params":{"cwd":"…","argv":[…]}}
//!   {"id":4,"method":"hook.tool_call","params":{"name":"…","arguments":{…}}}
//!   {"id":5,"method":"hook.input","params":{"text":"…"}}
//! e → extension notifications (no response):
//!   {"method":"event","params":{"name":"turn_end","aborted":false}}
//!   {"method":"shutdown"}
//! extension → e:
//!   {"id":1,"result":{…}} | {"id":1,"error":"message"}
//!   {"method":"notify","params":{"message":"…"}}          (any time)
//!   {"method":"tool.update","params":{"id":7,"stream":"stdout","chunk":"…"}}
//!
//! The initialize result is the manifest:
//!   {"name":"…","version":"…",
//!    "tools":[{"name","description","parameters":{JSON Schema}}…],
//!    "commands":[{"name","description"}…],
//!    "flags":[{"name","description"}…],   (shown in --help /help)
//!    "hooks":["startup","tool_call","input"…]}
//!
//! `initialize` params carry the extension's own config from
//! `~/.e/settings.json` under `"extensions":{"<name>":{…}}` — a
//! namespaced place to keep extension options without squatting on a
//! top-level settings key.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

/// The original request/response contract remains version 1. New optional
/// behavior is advertised as initialize capabilities so strict v1
/// extensions are never forced onto a different protocol for an additive
/// notification they may simply ignore.
pub const PROTOCOL_VERSION: u32 = 1;

/// The manifest an extension returns from `initialize`.
#[derive(Debug, Default, Deserialize)]
pub struct Manifest {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub tools: Vec<ToolDecl>,
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    #[serde(default)]
    pub flags: Vec<FlagDecl>,
    #[serde(default)]
    pub hooks: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ToolDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
}

#[derive(Debug, Deserialize)]
pub struct CommandDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// A command-line flag an extension understands, for `--help`/`/help` and
/// for startup-arg parsing. A `type` of `"string"` (or the default
/// `"boolean"`) makes e parse the flag from startup argv: booleans match
/// `--name`, `--name=true|false`, `--no-name`; strings match
/// `--name=value` or `--name value` (a following token that starts with
/// `-` is not consumed as a value). Parsed values ride the startup hook's
/// `flags` params. A name that isn't a clean `--name` token (e.g. a
/// display string `"-w, --worktree"`) is surfaced in `--help` only and
/// never parsed.
#[derive(Debug, Deserialize)]
pub struct FlagDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// "boolean" (default) or "string" — whether e parses this flag.
    #[serde(default = "default_flag_type", rename = "type")]
    pub flag_type: String,
    /// The manifest's optional `"default"` — the value to use when the
    /// flag is absent. e keeps it so the declaration contract is honored
    /// end to end, but never fabricates it into the parsed `flags` a
    /// receiver gets: absent stays absent, so a handler can tell "passed
    /// false" from "not passed". The extension applies the default itself
    /// (the scaffold's `flag()` does exactly that).
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

fn default_flag_type() -> String {
    "boolean".into()
}

impl FlagDecl {
    /// The `--name` token this flag parses, when the name is a clean
    /// identifier; None for display-only strings.
    pub fn long_form(&self) -> Option<String> {
        let clean = !self.name.is_empty()
            && self
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-');
        clean.then(|| format!("--{}", self.name))
    }

    /// How this flag renders in `--help`/`/help`: `--name` (or
    /// `--name <value>` for string flags) when typed, its raw name
    /// otherwise (display strings like `"-w, --worktree"`).
    pub fn help_token(&self) -> String {
        match self.long_form() {
            Some(long) if self.flag_type == "string" => format!("{long} <value>"),
            Some(long) => long,
            None => self.name.clone(),
        }
    }
}

/// A tool result from an extension.
#[derive(Debug, Default, Deserialize)]
pub struct ToolResult {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    /// Name this session; shown in /resume and the transcript.
    #[serde(default)]
    pub session_name: Option<String>,
}

/// A command result: a transcript notice and/or a prompt to submit, and
/// optionally a session name.
#[derive(Debug, Default, Deserialize)]
pub struct CommandResult {
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
}

/// A tool_call hook verdict. Empty object means allow.
#[derive(Debug, Default, Deserialize)]
pub struct HookVerdict {
    #[serde(default)]
    pub block: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// An input-hook verdict. `consume` drops the line (an optional notice
/// replaces it); `replace` submits different text instead. Empty allows.
#[derive(Debug, Default, Deserialize)]
pub struct InputVerdict {
    #[serde(default)]
    pub consume: bool,
    #[serde(default)]
    pub replace: Option<String>,
    #[serde(default)]
    pub notice: Option<String>,
}

/// A startup hook may consume arguments and request a same-binary relaunch.
#[derive(Debug, Default, Deserialize)]
pub struct StartupResult {
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub relaunch: Option<Relaunch>,
}

#[derive(Debug, Deserialize)]
pub struct Relaunch {
    pub cwd: String,
    #[serde(default)]
    pub env: BTreeMap<String, Option<String>>,
}

/// One parsed line arriving from an extension.
#[derive(Debug)]
pub enum Incoming {
    Response {
        id: u64,
        result: Result<Value, String>,
    },
    Notify {
        message: String,
    },
    ToolUpdate {
        id: u64,
        stream: crate::core::tools::OutputStream,
        chunk: String,
    },
}

pub fn parse_incoming(line: &str) -> Option<Incoming> {
    let value: Value = serde_json::from_str(line).ok()?;
    if let Some(id) = value.get("id").and_then(Value::as_u64) {
        if let Some(err) = value.get("error") {
            let message = err
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| err.to_string());
            return Some(Incoming::Response {
                id,
                result: Err(message),
            });
        }
        let result = value.get("result").cloned().unwrap_or(Value::Null);
        return Some(Incoming::Response {
            id,
            result: Ok(result),
        });
    }
    if value.get("method").and_then(Value::as_str) == Some("notify") {
        let message = value["params"]["message"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if !message.is_empty() {
            return Some(Incoming::Notify { message });
        }
    }
    if value.get("method").and_then(Value::as_str) == Some("tool.update") {
        let id = value["params"]["id"].as_u64()?;
        let stream = serde_json::from_value(value["params"]["stream"].clone()).ok()?;
        let chunk = value["params"]["chunk"].as_str()?.to_string();
        if !chunk.is_empty() {
            return Some(Incoming::ToolUpdate { id, stream, chunk });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_decl_keeps_the_declared_default() {
        let typed: FlagDecl = serde_json::from_str(
            r#"{"name":"tag","type":"string","default":"default-tag","description":"a tag"}"#,
        )
        .unwrap();
        assert_eq!(typed.default, Some(serde_json::json!("default-tag")));

        // Absent stays absent — and untyped flags stay boolean.
        let bare: FlagDecl = serde_json::from_str(r#"{"name":"dry"}"#).unwrap();
        assert_eq!(bare.default, None);
        assert_eq!(bare.flag_type, "boolean");
    }

    #[test]
    fn tool_updates_are_typed_and_correlated() {
        let parsed = parse_incoming(
            r#"{"method":"tool.update","params":{"id":7,"stream":"stderr","chunk":"working\n"}}"#,
        )
        .unwrap();
        match parsed {
            Incoming::ToolUpdate { id, stream, chunk } => {
                assert_eq!(id, 7);
                assert_eq!(stream, crate::core::tools::OutputStream::Stderr);
                assert_eq!(chunk, "working\n");
            }
            other => panic!("unexpected incoming message: {other:?}"),
        }
    }

    #[test]
    fn released_v1_manifest_fixture_remains_readable() {
        let fixture = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/extensions/v1-manifest.json"
        ));
        let manifest: Manifest = serde_json::from_str(fixture).unwrap();
        assert_eq!(manifest.name, "fixture");
        assert_eq!(manifest.tools[0].name, "hello");
        assert_eq!(manifest.flags[0].flag_type, "boolean");
    }
}
