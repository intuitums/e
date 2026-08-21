//! The extension wire protocol: JSON, one message per line, over the
//! extension process's stdin/stdout.
//!
//! e → extension requests (each expects a response with the same `id`):
//!   {"id":1,"method":"initialize","params":{"protocol":1,"e_version":"…","cwd":"…"}}
//!   {"id":7,"method":"tool_call","params":{"name":"…","arguments":{…}}}
//!   {"id":9,"method":"command","params":{"name":"…","args":"…"}}
//!   {"id":4,"method":"hook.tool_call","params":{"name":"…","arguments":{…}}}
//! e → extension notifications (no response):
//!   {"method":"event","params":{"name":"turn_end","aborted":false}}
//!   {"method":"shutdown"}
//! extension → e:
//!   {"id":1,"result":{…}} | {"id":1,"error":"message"}
//!   {"method":"notify","params":{"message":"…"}}          (any time)
//!
//! The initialize result is the manifest:
//!   {"name":"…","version":"…",
//!    "tools":[{"name","description","parameters":{JSON Schema}}…],
//!    "commands":[{"name","description"}…],
//!    "hooks":["tool_call"…]}

use serde::Deserialize;
use serde_json::Value;

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

/// A tool result from an extension.
#[derive(Debug, Default, Deserialize)]
pub struct ToolResult {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// A command result: a transcript notice and/or a prompt to submit.
#[derive(Debug, Default, Deserialize)]
pub struct CommandResult {
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
}

/// A tool_call hook verdict. Empty object means allow.
#[derive(Debug, Default, Deserialize)]
pub struct HookVerdict {
    #[serde(default)]
    pub block: bool,
    #[serde(default)]
    pub reason: Option<String>,
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
    None
}
