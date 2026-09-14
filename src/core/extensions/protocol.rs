//! The extension wire protocol: JSON, one message per line, over the
//! extension process's stdin/stdout.
//!
//! e → extension requests (each expects a response with the same `id`):
//!   {"id":1,"method":"initialize","params":{"protocol":1,"capabilities":["tool.update"],"e_version":"…","cwd":"…","extensions_config":{…}}}
//!   {"id":7,"method":"tool_call","params":{"name":"…","arguments":{…}}}
//!   {"id":9,"method":"command","params":{"name":"…","args":"…"}}
//!   {"id":2,"method":"hook.startup","params":{"cwd":"…","argv":[…],"flags":{…}}}
//!   {"id":4,"method":"hook.tool_call","params":{"name":"…","arguments":{…}}}
//!   {"id":5,"method":"hook.input","params":{"text":"…"}}
//!   {"id":6,"method":"hook.before_turn","params":{"prompt":"…"}}
//!   {"id":8,"method":"hook.tool_result","params":{"name":"…","content":"…","is_error":false}}
//!   {"id":9,"method":"hook.compact_summary","params":{"summary":"…"}}
//!   {"id":3,"method":"shortcut","params":{"key":"ctrl+alt+g"}}
//!   {"id":11,"method":"command.complete","params":{"name":"deploy","prefix":"st"}}
//! e → extension notifications (no response):
//!   {"method":"flags","params":{"flags":{…}}}              (at start, to extensions declaring typed flags)
//!   {"method":"event","params":{"name":"turn_end","extra":{"aborted":false}}}
//!   {"method":"ui.key","params":{"key":"down"}}          (interactive panel)
//!   {"method":"shutdown"}
//! extension → e:
//!   {"id":1,"result":{…}} | {"id":1,"error":"message"}
//!   {"method":"notify","params":{"message":"…"}}          (any time)
//!   {"method":"tool.update","params":{"id":7,"stream":"stdout","chunk":"…"}}
//!   {"id":"x1","method":"ui.select","params":{…}}        a request e answers
//!   {"id":"x2","method":"session.info","params":{}}
//!
//! The initialize result is the manifest:
//!   {"name":"…","version":"…",
//!    "tools":[{"name","description","parameters":{JSON Schema},
//!              "label":{"category","running","completed","target"}}…],
//!    "commands":[{"name","description","arguments"?,"completions"?}…],
//!    "flags":[{"name","description","type"?,"default"?}…],   (shown in --help /help; typed ones are parsed)
//!    "hooks":["startup","tool_call","input","before_turn","tool_result","compact_summary"],
//!    "events":["session_start","turn_start","tool_end"…],
//!    "shortcuts":[{"key":"ctrl+alt+g","description":"…"}]}
//!
//! Requests from an extension carry the extension's own `id` (any JSON
//! value) and are answered with the same id. Their methods are the `ui.*`
//! and `session.*` families documented in docs/extensions.md. The two id
//! spaces never meet: direction tells them apart.
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

/// What this e can do beyond version 1, listed in `initialize` params. Each
/// name is a family in docs/extensions.md; an extension that ignores them
/// all is a valid version-1 extension.
pub const CAPABILITIES: &[&str] = &[
    "tool.update",
    "events",
    "hooks",
    "display",
    "ui",
    "session",
    "shortcuts",
    "pane",
    "widget",
];

/// Every lifecycle event a manifest may subscribe to. Unknown names in a
/// manifest are ignored, so a newer extension on an older e degrades to
/// silence rather than a startup failure.
pub const EVENTS: &[&str] = &[
    "session_start",
    "session_shutdown",
    "turn_start",
    "turn_end",
    "tool_start",
    "tool_end",
    "compact_start",
    "compact_end",
    "model_change",
    "effort_change",
];

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
    /// Lifecycle events to receive (see [`EVENTS`]). Absent means the
    /// version-1 contract: `turn_end` only.
    #[serde(default)]
    pub events: Option<Vec<String>>,
    #[serde(default)]
    pub shortcuts: Vec<ShortcutDecl>,
}

#[derive(Debug, Deserialize)]
pub struct ToolDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
    /// How the tool's transcript row reads; without it the row says
    /// `Running <name>` / `Ran <name>`.
    #[serde(default)]
    pub label: Option<ToolLabel>,
}

/// A tool's transcript grammar, the built-in `Reading` / `Read <path>`
/// shape: a category for the batch tally, the running and completed verbs,
/// and the name of the argument whose value the row shows as its target.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ToolLabel {
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub running: String,
    #[serde(default)]
    pub completed: String,
    #[serde(default)]
    pub target: String,
}

/// A chord an extension answers, declared in the manifest. Chords e keeps
/// for itself (docs/keybindings.md) are never offered.
#[derive(Clone, Debug, Deserialize)]
pub struct ShortcutDecl {
    pub key: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct CommandDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// What the command takes after its name (`<env>`, `[path]`), shown in
    /// the `/` picker; picking such a command fills `/name ` in the
    /// composer instead of running it bare.
    #[serde(default)]
    pub arguments: Option<String>,
    /// Whether the extension answers `command.complete` with argument
    /// choices as the user types them.
    #[serde(default)]
    pub completions: bool,
}

/// One argument completion an extension offers: the text to insert, and
/// what the picker shows for it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Completion {
    pub value: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Completions {
    #[serde(default)]
    pub items: Vec<Completion>,
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

/// How a body paints: plain text, the transcript's markdown, or a unified
/// diff converted to the reference row grammar (line numbers, `+`/`-`
/// marker column, `⋯` between hunks). Unknown names read as text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Markdown,
    Diff,
    #[default]
    #[serde(other)]
    Text,
}

/// A block an extension shows: a command's `show`, a `ui.show` request, or
/// the detail behind a tool row. `body` is untrusted text — the frontend
/// sanitizes before paint — and is bounded by [`MAX_SHOW_BYTES`].
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Show {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub format: Format,
}

/// The largest `show`/`display` body e paints; longer ones are clipped
/// with a trailing note rather than refused, so a long diff still shows.
pub const MAX_SHOW_BYTES: usize = 64 * 1024;

/// A tool result from an extension. `content` is what the model reads;
/// `summary`, `display`, and `format` shape the transcript row and the
/// ctrl+o detail like a built-in tool's, and never reach the model.
#[derive(Debug, Default, Deserialize)]
pub struct ToolResult {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    /// Name this session; shown in /resume and the transcript.
    #[serde(default)]
    pub session_name: Option<String>,
    /// The row's suffix (`+12 -3`, `4 files`); absent shows nothing.
    #[serde(default)]
    pub summary: Option<String>,
    /// Detail for the viewer instead of `content`.
    #[serde(default)]
    pub display: Option<String>,
    #[serde(default)]
    pub format: Format,
}

/// A command result: a transcript notice, a block to show, and/or a prompt
/// to submit, and optionally a session name.
#[derive(Debug, Default, Deserialize)]
pub struct CommandResult {
    #[serde(default)]
    pub notice: Option<String>,
    #[serde(default)]
    pub show: Option<Show>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
}

/// A `before_turn` hook result: a paragraph appended to the system prompt
/// for this turn, and/or a message added to the conversation before the
/// request — hidden from the transcript when `internal`.
#[derive(Debug, Default, Deserialize)]
pub struct BeforeTurnResult {
    #[serde(default)]
    pub system_suffix: Option<String>,
    #[serde(default)]
    pub message: Option<InjectedMessage>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InjectedMessage {
    pub content: String,
    #[serde(default = "default_true")]
    pub internal: bool,
}

fn default_true() -> bool {
    true
}

/// A `tool_result` hook result: replacement content, or nothing to keep it.
#[derive(Debug, Default, Deserialize)]
pub struct ToolResultPatch {
    #[serde(default)]
    pub content: Option<String>,
}

/// A `compact_summary` hook result: the summary to store instead.
#[derive(Debug, Default, Deserialize)]
pub struct CompactSummaryResult {
    #[serde(default)]
    pub summary: Option<String>,
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
    /// A `ui.*` or `session.*` request the extension wants answered. `id`
    /// is the extension's own and is echoed verbatim.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

pub fn parse_incoming(line: &str) -> Option<Incoming> {
    let value: Value = serde_json::from_str(line).ok()?;
    // A line with both an id and a method is the extension asking, not
    // answering — checked first because e's own request ids are integers
    // and an extension may well use integers too.
    if let (Some(id), Some(method)) = (value.get("id"), value.get("method").and_then(Value::as_str))
    {
        if !id.is_null() && method != "notify" && method != "tool.update" {
            return Some(Incoming::Request {
                id: id.clone(),
                method: method.to_string(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
            });
        }
    }
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
    fn an_unknown_format_reads_as_text_instead_of_failing_the_result() {
        let result: ToolResult =
            serde_json::from_str(r#"{"content":"x","format":"sixel"}"#).unwrap();
        assert_eq!(result.format, Format::Text);
        let result: ToolResult =
            serde_json::from_str(r#"{"content":"x","format":"diff"}"#).unwrap();
        assert_eq!(result.format, Format::Diff);
    }

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
    fn extension_requests_are_told_apart_from_responses_by_method() {
        let request =
            parse_incoming(r#"{"id":"q1","method":"ui.select","params":{"title":"pick"}}"#)
                .unwrap();
        match request {
            Incoming::Request { id, method, params } => {
                assert_eq!(id, serde_json::json!("q1"));
                assert_eq!(method, "ui.select");
                assert_eq!(params["title"], "pick");
            }
            other => panic!("unexpected: {other:?}"),
        }
        // An integer id with a method is still a request; without one, a
        // response to e.
        assert!(matches!(
            parse_incoming(r#"{"id":4,"method":"session.info"}"#).unwrap(),
            Incoming::Request { .. }
        ));
        assert!(matches!(
            parse_incoming(r#"{"id":4,"result":{}}"#).unwrap(),
            Incoming::Response { id: 4, .. }
        ));
    }

    #[test]
    fn version_two_fields_are_optional_and_bounded_by_defaults() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"name":"x","tools":[{"name":"diff","label":{"running":"Diffing","completed":"Diffed","target":"path"}}],
                "events":["turn_start","bogus"],"shortcuts":[{"key":"ctrl+g"}]}"#,
        )
        .unwrap();
        assert_eq!(manifest.tools[0].label.as_ref().unwrap().target, "path");
        assert_eq!(manifest.events.as_deref().unwrap().len(), 2);
        assert_eq!(manifest.shortcuts[0].key, "ctrl+g");
        let result: ToolResult =
            serde_json::from_str(r#"{"content":"ok","format":"diff","summary":"+1 -1"}"#).unwrap();
        assert_eq!(result.format, Format::Diff);
        let plain: ToolResult = serde_json::from_str(r#"{"content":"ok"}"#).unwrap();
        assert_eq!(plain.format, Format::Text);
        let command: CommandResult =
            serde_json::from_str(r#"{"show":{"title":"t","body":"b","format":"markdown"}}"#)
                .unwrap();
        assert_eq!(command.show.unwrap().format, Format::Markdown);
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
