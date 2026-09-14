//! The extension process host: discovers extensions in `~/.e/extensions/` — a
//! top-level executable file, or a subdirectory bundling its own files (its
//! executable plus helpers like a scaffold or data) — keeps one long-lived
//! process per extension, and routes tools, commands, hooks, and events over
//! the line protocol.
//!
//! Failure posture: discovery and runtime hooks fail open and are reported.
//! An extension that advertises a startup hook owns startup argument handling,
//! so its explicit failure is fatal rather than leaking consumed arguments
//! into the user's prompt.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::protocol::{
    self, BeforeTurnResult, CommandResult, CompactSummaryResult, Completion, Completions,
    HookVerdict, Incoming, InjectedMessage, InputVerdict, Manifest, Relaunch, RenderResult, Show,
    StartupResult, ToolLabel, ToolResult, ToolResultPatch,
};
use crate::core::config::home;

const INIT_TIMEOUT: Duration = Duration::from_secs(5);
const HOOK_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_TIMEOUT: Duration = Duration::from_secs(300);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// Argument completions race the user's typing; a slow answer is dropped.
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_EXTENSION_LINE_BYTES: usize = 1024 * 1024;
/// How long to wait on a killed child before leaving it to tokio's orphan
/// reaper — quitting must never hang on one.
const REAP_TIMEOUT: Duration = Duration::from_secs(1);
/// Requests one extension may have unanswered at once (`ui.*`,
/// `session.*`). Past this, a request is answered with an error at once —
/// a runaway extension cannot queue work against the user's attention.
const MAX_INFLIGHT_REQUESTS: usize = 32;

#[derive(Clone, Debug)]
pub struct ToolProgress {
    pub stream: crate::core::tools::OutputStream,
    pub chunk: String,
}

/// A `ui.*` or `session.*` request from an extension, handed to whoever
/// owns the surface (the TUI) to answer. Dropping it unanswered replies
/// with an error, so a request never leaves the extension waiting forever.
#[derive(Debug)]
pub struct HostRequest {
    /// The manifest name of the asking extension.
    pub extension: String,
    pub method: String,
    pub params: Value,
    reply: Option<oneshot::Sender<Result<Value, String>>>,
}

impl HostRequest {
    pub fn respond(mut self, result: Result<Value, String>) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(result);
        }
    }

    /// Answer with `{}`.
    pub fn ok(self) {
        self.respond(Ok(json!({})));
    }

    /// A request with no extension behind it, for tests of whoever answers
    /// them: the receiver sees what the extension would have read.
    pub fn fake(
        extension: &str,
        method: &str,
        params: Value,
    ) -> (HostRequest, oneshot::Receiver<Result<Value, String>>) {
        let (tx, rx) = oneshot::channel();
        (
            HostRequest {
                extension: extension.into(),
                method: method.into(),
                params,
                reply: Some(tx),
            },
            rx,
        )
    }
}

impl Drop for HostRequest {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(Err("request dropped".into()));
        }
    }
}

/// What `before_turn` hooks contributed, in extension order.
#[derive(Debug, Default)]
pub struct BeforeTurn {
    pub system_suffixes: Vec<String>,
    pub messages: Vec<InjectedMessage>,
}

struct Extension {
    manifest: Manifest,
    /// Outgoing lines to the process's stdin.
    writer: mpsc::Sender<String>,
    link: Arc<Link>,
}

/// What one extension process shares between the host, its two pipe tasks,
/// and shutdown: liveness, the waiters to fail when it goes, the child to
/// reap, and the notice that announces an exit nobody asked for.
struct Link {
    /// False once either process pipe proves the extension has exited (or
    /// shutdown retired it). Pending requests fail immediately and new
    /// ones are refused.
    alive: AtomicBool,
    /// Requests awaiting a response, keyed by wire id.
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    progress: Mutex<HashMap<u64, mpsc::Sender<ToolProgress>>>,
    child: tokio::sync::Mutex<Option<tokio::process::Child>>,
    /// Manifest notice and unexpected-exit flag, coordinated across handshake
    /// and pipe tasks. Pre-initialize failures are reported by `start`.
    exit_notice: Mutex<(Option<String>, bool)>,
    notices: mpsc::Sender<String>,
}

impl Link {
    /// Mark the extension dead and fail every waiter. True for the caller
    /// that got there first; every later call is a no-op.
    fn retire(&self) -> bool {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return false;
        }
        // Dropping the senders wakes every request through its
        // `Ok(Err(_)) => extension exited` path instead of its long timeout.
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        true
    }

    /// A pipe found the process gone without being asked: retire it and
    /// say so in the transcript — exactly once, by whichever pipe noticed
    /// first, and only after the handshake.
    fn exited(&self) {
        if self.retire() {
            let mut state = self.exit_notice.lock().unwrap_or_else(|e| e.into_inner());
            state.1 = true;
            if let Some(notice) = &state.0 {
                let _ = self.notices.try_send(notice.clone());
            }
        }
    }

    /// Install the manifest's notice, including an EOF that beat the handshake.
    fn install_exit_notice(&self, notice: String) {
        let mut state = self.exit_notice.lock().unwrap_or_else(|e| e.into_inner());
        if state.1 {
            let _ = self.notices.try_send(notice.clone());
        }
        state.0 = Some(notice);
    }

    /// Kill the child if it still runs and wait it, so it never lingers as
    /// a zombie — for the session, or (on the relaunch path) across the
    /// exec into the next e, where nothing could reap it any more. A child
    /// that outlives `REAP_TIMEOUT` is dropped to tokio's orphan reaper.
    /// The slot stays locked across the wait so a second reaper (the
    /// reader task on EOF, `shutdown` a beat later) blocks until the first
    /// has finished instead of returning while the wait is still pending.
    async fn reap(&self) {
        let mut slot = self.child.lock().await;
        if let Some(child) = slot.as_mut() {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(REAP_TIMEOUT, child.wait()).await;
            *slot = None;
        }
    }

    /// Kill the child without waiting for its exit. Cancellation guards call
    /// this before handing the wait to a background reaper.
    fn kill_now(&self) {
        self.retire();
        if let Ok(mut slot) = self.child.try_lock() {
            if let Some(child) = slot.as_mut() {
                let _ = child.start_kill();
            }
        }
    }
}

pub struct ExtensionHost {
    extensions: Vec<Extension>,
    ids: AtomicU64,
}

/// Result of startup-hook chaining before normal CLI parsing.
pub enum StartupAction {
    Continue(Vec<String>),
    Relaunch {
        argv: Vec<String>,
        request: Relaunch,
    },
}

/// One flag declaration matching an argv token — see `ExtensionHost::match_flag`.
enum FlagMatch {
    /// Always record this: every bool form, and a string flag's `--name=value`
    /// or unclaimed bare form (`--name` not followed by a value-shaped token).
    Definite { name: String, value: FlagValue },
    /// A bare string flag (`--name value`) whose next token looks like a
    /// value. Only the first such match for one arg actually claims it —
    /// callers that care (`parse_flags`) arbitrate that; a losing claim
    /// still counts as "matched" (the arg is typed, just not this flag's
    /// value) but records nothing.
    ClaimsNext { name: String, next: String },
}

enum FlagValue {
    Bool(bool),
    Str(Option<String>),
}

impl FlagValue {
    fn into_json(self) -> serde_json::Value {
        match self {
            FlagValue::Bool(on) => serde_json::json!(on),
            FlagValue::Str(Some(value)) => serde_json::json!(value),
            FlagValue::Str(None) => serde_json::Value::Null,
        }
    }
}

impl ExtensionHost {
    /// Discover and start every extension for this process: its working
    /// directory and command line are the extensions'. `notices` receives
    /// extension `notify` messages and startup diagnostics for the transcript.
    /// `requests` receives the extensions' own `ui.*` / `session.*`
    /// requests; `None` is a headless host (`e rpc`, the SDK, tests) where
    /// every such request is answered "no ui" at once and `initialize` says so.
    pub async fn start(
        notices: mpsc::Sender<String>,
        requests: Option<mpsc::Sender<HostRequest>>,
    ) -> Arc<ExtensionHost> {
        Self::start_in(
            notices,
            std::env::current_dir().unwrap_or_default(),
            std::env::args().skip(1).collect(),
            requests,
        )
        .await
    }

    /// `start` for an embedding whose workspace is not the process's: every
    /// extension runs in `cwd` and is told so at `initialize`, and `argv` is
    /// what typed extension flags are parsed from — pass an empty vector when
    /// the host's command line has nothing to do with e.
    pub async fn start_in(
        notices: mpsc::Sender<String>,
        cwd: PathBuf,
        argv: Vec<String>,
        requests: Option<mpsc::Sender<HostRequest>>,
    ) -> Arc<ExtensionHost> {
        // Spawn and hand-shake every extension concurrently: a slow (or
        // timing-out) child must not delay the ones after it, so startup
        // costs one handshake, not their sum. Results are collected in
        // discovery order — tool-clash resolution below is
        // first-declaration-wins and must stay deterministic.
        let paths = discover();
        for spec in crate::core::resources::packages::missing() {
            let _ = notices
                .send(format!("package {spec}: not installed — run `e install`"))
                .await;
        }
        // If an embedding drops this future during a handshake, kill every
        // child that has started and hand its wait to the runtime. Each child
        // registers as soon as its link exists.
        let spawned_links: Arc<Mutex<Vec<Arc<Link>>>> = Arc::new(Mutex::new(Vec::new()));
        struct StartupGuard {
            spawned: Arc<Mutex<Vec<Arc<Link>>>>,
            armed: bool,
        }
        impl Drop for StartupGuard {
            fn drop(&mut self) {
                if self.armed {
                    for link in self
                        .spawned
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                    {
                        link.kill_now();
                        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                            let link = link.clone();
                            runtime.spawn(async move { link.reap().await });
                        }
                    }
                }
            }
        }
        let mut guard = StartupGuard {
            spawned: spawned_links.clone(),
            armed: true,
        };
        let started = futures::future::join_all(paths.iter().map(|path| {
            let notices = notices.clone();
            let cwd = cwd.clone();
            let spawned = &spawned_links;
            let requests = requests.clone();
            async move {
                (
                    path,
                    spawn(path, &cwd, notices, Some(spawned), requests).await,
                )
            }
        }))
        .await;
        let mut extensions = Vec::new();
        for (path, result) in started {
            match result {
                Ok(ext) => extensions.push(ext),
                Err(reason) => {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let _ = notices.send(format!("extension {name}: {reason}")).await;
                }
            }
        }
        // Tool names must be unambiguous: schema merging and call routing
        // both resolve first-declaration-wins, and a duplicate would
        // advertise one contract while executing another owner. Later
        // duplicates are dropped with a visible notice.
        let mut seen_tools: std::collections::HashSet<String> = Default::default();
        for ext in &mut extensions {
            let name = ext.manifest.name.clone();
            ext.manifest.tools.retain(|tool| {
                let fresh = seen_tools.insert(tool.name.clone());
                if !fresh {
                    let _ = notices.try_send(format!(
                        "extension {name}: tool {} already provided by another extension — ignored",
                        tool.name
                    ));
                }
                fresh
            });
        }
        // Shortcuts resolve the same way: one owner per chord, first wins —
        // and only chords e leaves unbound are offered at all.
        let mut seen_keys: std::collections::HashSet<String> = Default::default();
        for ext in &mut extensions {
            let name = ext.manifest.name.clone();
            ext.manifest.shortcuts.retain(|shortcut| {
                let key = normalize_chord(&shortcut.key);
                if !shortcut_allowed(&key) {
                    let _ = notices.try_send(format!(
                        "extension {name}: shortcut {} is not available to extensions — ignored",
                        shortcut.key
                    ));
                    return false;
                }
                let fresh = seen_keys.insert(key.clone());
                if !fresh {
                    let _ = notices.try_send(format!(
                        "extension {name}: shortcut {} already taken — ignored",
                        shortcut.key
                    ));
                }
                fresh
            });
        }
        let host = Arc::new(ExtensionHost {
            extensions,
            ids: AtomicU64::new(1),
        });
        // Every extension that declares typed flags gets them now, before any
        // startup hook — a tool-only extension can read its flags anytime, not
        // just during startup. `flags` is a notification (no reply expected).
        let parsed = host.parse_flags(&argv);
        if !parsed.as_object().map(|m| m.is_empty()).unwrap_or(true) {
            for ext in &host.extensions {
                if ext.manifest.flags.iter().any(|f| f.long_form().is_some()) {
                    let line = json!({
                        "method": "flags",
                        "params": { "flags": parsed },
                    })
                    .to_string();
                    let _ = ext.writer.try_send(line);
                }
            }
        }
        // The host now owns every link and handles normal shutdown.
        guard.armed = false;
        host
    }

    /// An empty host, for sessions with no extensions (and for tests).
    pub fn empty() -> Arc<ExtensionHost> {
        Arc::new(ExtensionHost {
            extensions: Vec::new(),
            ids: AtomicU64::new(1),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
    }

    /// Extension identities for diagnostics. No config, arguments, or child
    /// process details are exposed here.
    pub fn identities(&self) -> Vec<(String, String)> {
        self.extensions
            .iter()
            .map(|extension| {
                (
                    extension.manifest.name.clone(),
                    extension.manifest.version.clone(),
                )
            })
            .collect()
    }

    /// Names, declared versions, and liveness only — enough for diagnostics
    /// without exposing extension configuration or protocol messages.
    pub fn diagnostic_status(&self) -> Vec<(String, String, bool)> {
        self.extensions
            .iter()
            .map(|extension| {
                (
                    extension.manifest.name.clone(),
                    extension.manifest.version.clone(),
                    extension.link.alive.load(Ordering::SeqCst),
                )
            })
            .collect()
    }

    /// Whether `arg` matches one flag's declared long form. Shared by
    /// `parse_flags` (which records the value) and `strip_typed_flags`
    /// (which only needs to know how much of argv this flag consumes), so
    /// the two never drift on what counts as a match. A bare string flag
    /// (`--name value`) whose next token looks like a value returns
    /// `ClaimsNext` rather than a value directly: whether *this particular*
    /// match gets to claim `next` depends on whether an earlier-declared
    /// flag already claimed it for the same arg, which only `parse_flags`
    /// tracks (`strip_typed_flags` just needs to know the shape matched, to
    /// skip the same number of tokens).
    fn match_flag(
        flag: &crate::core::extensions::protocol::FlagDecl,
        arg: &str,
        next: Option<&str>,
    ) -> Option<FlagMatch> {
        let long = flag.long_form()?;
        if flag.flag_type == "string" {
            if let Some(value) = arg.strip_prefix(&format!("{long}=")) {
                return Some(FlagMatch::Definite {
                    name: flag.name.clone(),
                    value: FlagValue::Str(Some(value.to_string())),
                });
            }
            if arg != long {
                return None;
            }
            return Some(match next {
                Some(next) if next != "-" && !next.starts_with('-') => FlagMatch::ClaimsNext {
                    name: flag.name.clone(),
                    next: next.to_string(),
                },
                _ => FlagMatch::Definite {
                    name: flag.name.clone(),
                    value: FlagValue::Str(None),
                },
            });
        }
        if arg == long {
            return Some(FlagMatch::Definite {
                name: flag.name.clone(),
                value: FlagValue::Bool(true),
            });
        }
        if let Some(value) = arg.strip_prefix(&format!("{long}=")) {
            let on = matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            );
            return Some(FlagMatch::Definite {
                name: flag.name.clone(),
                value: FlagValue::Bool(on),
            });
        }
        if arg
            .strip_prefix("--no-")
            .is_some_and(|rest| rest == flag.name)
        {
            return Some(FlagMatch::Definite {
                name: flag.name.clone(),
                value: FlagValue::Bool(false),
            });
        }
        None
    }

    /// Parse argv against every extension's typed flag declarations. Returns
    /// `{"name": value}` for flags that appeared — booleans as true/false,
    /// strings as their value (null when a string flag is bare or followed
    /// by another flag). The argv is not modified; an extension still
    /// decides what reaches the next stage.
    fn parse_flags(&self, argv: &[String]) -> serde_json::Value {
        let mut parsed = serde_json::Map::new();
        let mut i = 0;
        while i < argv.len() {
            let arg = &argv[i];
            if arg == "--" {
                break;
            }
            // A separated string value (`--name value`) is claimed by at
            // most one declaration, whichever matches first; a later
            // ClaimsNext match on the same arg (e.g. two extensions
            // declaring the same flag) records nothing rather than
            // overwriting it.
            let mut consumed_value = false;
            let next = argv.get(i + 1).map(String::as_str);
            for ext in &self.extensions {
                for flag in &ext.manifest.flags {
                    match Self::match_flag(flag, arg, next) {
                        None => {}
                        Some(FlagMatch::Definite { name, value }) => {
                            parsed.insert(name, value.into_json());
                        }
                        Some(FlagMatch::ClaimsNext { name, next }) if !consumed_value => {
                            parsed.insert(name, serde_json::json!(next));
                            consumed_value = true;
                        }
                        Some(FlagMatch::ClaimsNext { .. }) => {}
                    }
                }
            }
            i += 1 + usize::from(consumed_value); // + skip a claimed value slot
        }
        serde_json::Value::Object(parsed)
    }

    /// Remove typed extension flags before e parses subcommands and the initial
    /// prompt. Startup hooks still receive raw argv and may rewrite it first;
    /// this final pass prevents a tool-only string flag's separated value from
    /// becoming an accidental user message.
    fn strip_typed_flags(&self, argv: Vec<String>) -> Vec<String> {
        let mut kept = Vec::with_capacity(argv.len());
        let mut i = 0;
        while i < argv.len() {
            let arg = &argv[i];
            if arg == "--" {
                kept.extend(argv[i..].iter().cloned());
                break;
            }

            let mut matched = false;
            let mut consumes_next = false;
            let next = argv.get(i + 1).map(String::as_str);
            for ext in &self.extensions {
                for flag in &ext.manifest.flags {
                    match Self::match_flag(flag, arg, next) {
                        None => {}
                        Some(FlagMatch::Definite { .. }) => matched = true,
                        Some(FlagMatch::ClaimsNext { .. }) => {
                            matched = true;
                            consumes_next = true;
                        }
                    }
                }
            }

            if !matched {
                kept.push(arg.clone());
            }
            i += 1 + usize::from(consumes_next);
        }
        kept
    }

    /// Chain startup-capable extensions over raw argv. Unlike runtime hooks,
    /// an explicit startup-hook failure is fatal because silently treating a
    /// consumed branch name as a prompt is unsafe and surprising. Parsed
    /// flag values (from every extension's typed flag declarations) ride
    /// along as `flags` so extensions read `--name=value` / `--name` without
    /// hand-scanning argv.
    pub async fn startup(&self, mut argv: Vec<String>) -> Result<StartupAction, String> {
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string();
        let parsed = self.parse_flags(&argv);
        for ext in &self.extensions {
            if !ext.manifest.hooks.iter().any(|hook| hook == "startup") {
                continue;
            }
            let value = self
                .request(
                    ext,
                    "hook.startup",
                    json!({ "cwd": cwd, "argv": argv.clone(), "flags": parsed }),
                    HOOK_TIMEOUT,
                )
                .await
                .map_err(|reason| format!("extension {} startup: {reason}", ext.manifest.name))?;
            let result: StartupResult = serde_json::from_value(value).map_err(|error| {
                format!(
                    "extension {} startup: bad result: {error}",
                    ext.manifest.name
                )
            })?;
            if let Some(next) = result.argv {
                argv = next;
            }
            for (name, value) in result.env {
                if name.is_empty()
                    || name.contains('=')
                    || name.contains('\0')
                    || value.as_deref().is_some_and(|value| value.contains('\0'))
                {
                    return Err(format!(
                        "extension {} startup: invalid environment entry",
                        ext.manifest.name
                    ));
                }
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
            if let Some(request) = result.relaunch {
                if request.cwd.trim().is_empty() {
                    return Err(format!(
                        "extension {} startup: relaunch cwd is empty",
                        ext.manifest.name
                    ));
                }
                return Ok(StartupAction::Relaunch { argv, request });
            }
        }
        Ok(StartupAction::Continue(self.strip_typed_flags(argv)))
    }

    /// Built-in schemas with extension tools merged in. An extension tool with
    /// a built-in's name replaces it — extensions may override built-ins.
    pub fn merged_tool_schemas(&self) -> Vec<Value> {
        let mut schemas = crate::core::tools::schemas();
        for ext in &self.extensions {
            for tool in &ext.manifest.tools {
                schemas.retain(|s| s["function"]["name"].as_str() != Some(tool.name.as_str()));
                schemas.push(json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": if tool.parameters.is_object() {
                            tool.parameters.clone()
                        } else {
                            json!({"type": "object", "properties": {}})
                        },
                    }
                }));
            }
        }
        schemas
    }

    pub fn owns_tool(&self, name: &str) -> bool {
        self.extensions
            .iter()
            .any(|e| e.manifest.tools.iter().any(|t| t.name == name))
    }

    /// `(name, description)` of every extension command, for the / picker.
    pub fn commands(&self) -> Vec<(String, String)> {
        self.extensions
            .iter()
            .flat_map(|e| {
                e.manifest
                    .commands
                    .iter()
                    .map(|c| (c.name.clone(), c.description.clone()))
            })
            .collect()
    }

    /// The declared argument hint of an extension command, if any.
    pub fn command_hint(&self, name: &str) -> Option<String> {
        self.extensions
            .iter()
            .flat_map(|e| e.manifest.commands.iter())
            .find(|c| c.name == name)
            .and_then(|c| c.arguments.clone())
    }

    /// Whether an extension offers argument completions for `name`.
    pub fn has_completions(&self, name: &str) -> bool {
        self.extensions
            .iter()
            .flat_map(|e| e.manifest.commands.iter())
            .any(|c| c.name == name && c.completions)
    }

    /// Ask the owning extension what could follow `/name prefix`. Empty on
    /// any failure: completions are a convenience, never a gate.
    pub async fn complete_command(&self, name: &str, prefix: &str) -> Vec<Completion> {
        let Some(ext) = self.extensions.iter().find(|e| {
            e.manifest
                .commands
                .iter()
                .any(|c| c.name == name && c.completions)
        }) else {
            return Vec::new();
        };
        match self
            .request(
                ext,
                "command.complete",
                json!({"name": name, "prefix": prefix}),
                COMPLETE_TIMEOUT,
            )
            .await
        {
            Ok(value) => serde_json::from_value::<Completions>(value)
                .map(|c| c.items)
                .unwrap_or_default()
                .into_iter()
                .filter(|c| !c.value.is_empty())
                .take(50)
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// `(name, description)` of every extension flag, for `--help`/`/help`.
    /// Typed flags are parsed and removed after startup hooks have seen raw
    /// argv; display-only declarations remain the hook's responsibility.
    pub fn flags(&self) -> Vec<(String, String)> {
        self.extensions
            .iter()
            .flat_map(|e| {
                e.manifest
                    .flags
                    .iter()
                    .map(|f| (f.help_token(), f.description.clone()))
            })
            .collect()
    }

    /// Declared shortcuts as `(chord, description)`, normalized lowercase.
    pub fn shortcuts(&self) -> Vec<(String, String)> {
        self.extensions
            .iter()
            .flat_map(|e| {
                e.manifest
                    .shortcuts
                    .iter()
                    .map(|s| (normalize_chord(&s.key), s.description.clone()))
            })
            .collect()
    }

    pub fn has_shortcut(&self, chord: &str) -> bool {
        let chord = normalize_chord(chord);
        self.extensions.iter().any(|e| {
            e.manifest
                .shortcuts
                .iter()
                .any(|s| normalize_chord(&s.key) == chord)
        })
    }

    /// Run the extension that owns `chord`; answered like a command.
    pub async fn run_shortcut(&self, chord: &str) -> CommandResult {
        let chord = normalize_chord(chord);
        let Some(ext) = self.extensions.iter().find(|e| {
            e.manifest
                .shortcuts
                .iter()
                .any(|s| normalize_chord(&s.key) == chord)
        }) else {
            return CommandResult::default();
        };
        match self
            .request(ext, "shortcut", json!({"key": chord}), COMMAND_TIMEOUT)
            .await
        {
            Ok(value) => serde_json::from_value(value).unwrap_or_else(|_| CommandResult {
                notice: Some(format!("{chord}: bad result")),
                ..Default::default()
            }),
            Err(reason) => CommandResult {
                notice: Some(format!("{chord}: {reason}")),
                ..Default::default()
            },
        }
    }

    /// The declared transcript grammar for an extension tool, if any.
    pub fn tool_label(&self, name: &str) -> Option<ToolLabel> {
        self.extensions
            .iter()
            .flat_map(|e| e.manifest.tools.iter())
            .find(|t| t.name == name)
            .and_then(|t| t.label.clone())
    }

    pub fn has_command(&self, name: &str) -> bool {
        self.extensions
            .iter()
            .any(|e| e.manifest.commands.iter().any(|c| c.name == name))
    }

    pub async fn call_tool(&self, name: &str, arguments: &str) -> ToolResult {
        let (progress, updates) = mpsc::channel(1);
        // This compatibility wrapper deliberately discards progress. Drop
        // the receiver before dispatch so a chatty streaming extension sees
        // a closed channel instead of filling an unpolled buffer and
        // deadlocking before its final response.
        drop(updates);
        self.call_tool_streaming(name, arguments, progress).await
    }

    /// Run an extension tool while routing the additive `tool.update`
    /// capability for this request into `progress`. Extensions that do not
    /// implement it remain compatible: they simply send no updates.
    pub async fn call_tool_streaming(
        &self,
        name: &str,
        arguments: &str,
        progress: mpsc::Sender<ToolProgress>,
    ) -> ToolResult {
        let Some(ext) = self
            .extensions
            .iter()
            .find(|e| e.manifest.tools.iter().any(|t| t.name == name))
        else {
            return ToolResult {
                content: format!("no extension owns tool {name}"),
                is_error: true,
                ..Default::default()
            };
        };
        let args: Value =
            serde_json::from_str(arguments).unwrap_or(Value::String(arguments.into()));
        match self
            .request_with_progress(
                ext,
                "tool_call",
                json!({"name": name, "arguments": args}),
                TOOL_TIMEOUT,
                Some(progress),
            )
            .await
        {
            Ok(value) => serde_json::from_value(value).unwrap_or_else(|_| ToolResult {
                content: format!("{name}: bad result"),
                is_error: true,
                ..Default::default()
            }),
            Err(reason) => ToolResult {
                content: format!("{name}: {reason}"),
                is_error: true,
                ..Default::default()
            },
        }
    }

    pub async fn run_command(&self, name: &str, args: &str) -> CommandResult {
        let Some(ext) = self
            .extensions
            .iter()
            .find(|e| e.manifest.commands.iter().any(|c| c.name == name))
        else {
            return CommandResult {
                notice: Some(format!("no extension owns /{name}")),
                ..Default::default()
            };
        };
        match self
            .request(
                ext,
                "command",
                json!({"name": name, "args": args}),
                COMMAND_TIMEOUT,
            )
            .await
        {
            Ok(value) => serde_json::from_value(value).unwrap_or_else(|_| CommandResult {
                notice: Some(format!("/{name}: bad result")),
                ..Default::default()
            }),
            Err(reason) => CommandResult {
                notice: Some(format!("/{name}: {reason}")),
                ..Default::default()
            },
        }
    }

    /// Ask every extension with the `tool_call` hook. The first explicit block
    /// wins; transport failures and timeouts allow (fail open).
    pub async fn hook_tool_call(&self, name: &str, arguments: &str) -> Option<String> {
        let args: Value =
            serde_json::from_str(arguments).unwrap_or(Value::String(arguments.into()));
        for ext in &self.extensions {
            if !ext.manifest.hooks.iter().any(|h| h == "tool_call") {
                continue;
            }
            if let Ok(value) = self
                .request(
                    ext,
                    "hook.tool_call",
                    json!({"name": name, "arguments": args}),
                    HOOK_TIMEOUT,
                )
                .await
            {
                let verdict: HookVerdict = serde_json::from_value(value).unwrap_or_default();
                if verdict.block {
                    return Some(
                        verdict
                            .reason
                            .unwrap_or_else(|| format!("blocked by {}", ext.manifest.name)),
                    );
                }
            }
        }
        None
    }

    /// Whether any extension listens for input — the app skips the hook
    /// round-trip entirely when none does.
    pub fn has_input_hook(&self) -> bool {
        self.extensions
            .iter()
            .any(|e| e.manifest.hooks.iter().any(|h| h == "input"))
    }

    /// Ask every extension with the `input` hook. The first extension to
    /// consume or replace a line wins; transport failures and timeouts allow
    /// (fail open — a slow extension never eats a user's message). An
    /// allowing extension may still attach a `notice`; those accumulate,
    /// one per line, onto whichever verdict is finally returned.
    pub async fn hook_input(&self, text: &str) -> InputVerdict {
        // Fast path: no extension listens at all.
        if !self
            .extensions
            .iter()
            .any(|e| e.manifest.hooks.iter().any(|h| h == "input"))
        {
            return InputVerdict::default();
        }
        let mut notices: Vec<String> = Vec::new();
        for ext in &self.extensions {
            if !ext.manifest.hooks.iter().any(|h| h == "input") {
                continue;
            }
            if let Ok(value) = self
                .request(ext, "hook.input", json!({"text": text}), HOOK_TIMEOUT)
                .await
            {
                let mut verdict: InputVerdict = serde_json::from_value(value).unwrap_or_default();
                notices.extend(verdict.notice.take().filter(|n| !n.trim().is_empty()));
                if verdict.consume || verdict.replace.as_deref().is_some_and(|r| !r.is_empty()) {
                    verdict.notice = join_notices(notices);
                    return verdict;
                }
            }
        }
        InputVerdict {
            notice: join_notices(notices),
            ..InputVerdict::default()
        }
    }

    /// Ask every extension with the `before_turn` hook, in order, what to
    /// add to this turn: system-prompt paragraphs and conversation
    /// messages. Failures contribute nothing (fail open).
    pub async fn hook_before_turn(&self, prompt: &str) -> BeforeTurn {
        let mut out = BeforeTurn::default();
        for ext in &self.extensions {
            if !ext.manifest.hooks.iter().any(|h| h == "before_turn") {
                continue;
            }
            if let Ok(value) = self
                .request(
                    ext,
                    "hook.before_turn",
                    json!({"prompt": prompt}),
                    HOOK_TIMEOUT,
                )
                .await
            {
                let result: BeforeTurnResult = serde_json::from_value(value).unwrap_or_default();
                if let Some(suffix) = result.system_suffix.filter(|s| !s.trim().is_empty()) {
                    out.system_suffixes.push(suffix);
                }
                if let Some(message) = result.message.filter(|m| !m.content.trim().is_empty()) {
                    out.messages.push(message);
                }
            }
        }
        out
    }

    /// Let every extension with the `tool_result` hook rewrite what the
    /// model reads, in order — each sees the previous one's text. None when
    /// nothing changed.
    pub async fn hook_tool_result(
        &self,
        name: &str,
        content: &str,
        is_error: bool,
    ) -> Option<String> {
        let mut current: Option<String> = None;
        for ext in &self.extensions {
            if !ext.manifest.hooks.iter().any(|h| h == "tool_result") {
                continue;
            }
            let text = current.as_deref().unwrap_or(content);
            if let Ok(value) = self
                .request(
                    ext,
                    "hook.tool_result",
                    json!({"name": name, "content": text, "is_error": is_error}),
                    HOOK_TIMEOUT,
                )
                .await
            {
                let patch: ToolResultPatch = serde_json::from_value(value).unwrap_or_default();
                if let Some(replacement) = patch.content {
                    current = Some(replacement);
                }
            }
        }
        current
    }

    /// Whether any extension asked to render `subject` — `tool:<name>` or
    /// `assistant` — through its `render` hook.
    pub fn renders(&self, subject: &str) -> bool {
        self.extensions
            .iter()
            .any(|e| Self::wants_render(&e.manifest, subject))
    }

    fn wants_render(manifest: &Manifest, subject: &str) -> bool {
        manifest.hooks.iter().any(|h| h == "render")
            && manifest.renders.iter().any(|r| {
                r == subject || (r == "tool:*" && subject.starts_with("tool:")) || r == "*"
            })
    }

    /// Ask every extension that renders `subject`, in order, for the body
    /// to show instead of `content`; each sees the previous answer. None
    /// when nobody changed anything. `kind` is `tool` or `assistant`,
    /// `name` the tool (empty for a reply).
    pub async fn hook_render(&self, subject: &str, name: &str, content: &str) -> Option<Show> {
        let mut current: Option<Show> = None;
        let kind = subject.split(':').next().unwrap_or(subject);
        for ext in &self.extensions {
            if !Self::wants_render(&ext.manifest, subject) {
                continue;
            }
            let text = current.as_ref().map(|s| s.body.as_str()).unwrap_or(content);
            if let Ok(value) = self
                .request(
                    ext,
                    "hook.render",
                    json!({"kind": kind, "name": name, "content": text}),
                    HOOK_TIMEOUT,
                )
                .await
            {
                let result: RenderResult = serde_json::from_value(value).unwrap_or_default();
                if let Some(body) = result.body {
                    current = Some(Show {
                        title: String::new(),
                        body,
                        format: result.format,
                    });
                }
            }
        }
        current
    }

    /// Let every extension with the `compact_summary` hook edit the summary
    /// about to replace the conversation, in order. None when unchanged.
    pub async fn hook_compact_summary(&self, summary: &str) -> Option<String> {
        let mut current: Option<String> = None;
        for ext in &self.extensions {
            if !ext.manifest.hooks.iter().any(|h| h == "compact_summary") {
                continue;
            }
            let text = current.as_deref().unwrap_or(summary);
            if let Ok(value) = self
                .request(
                    ext,
                    "hook.compact_summary",
                    json!({"summary": text}),
                    HOOK_TIMEOUT,
                )
                .await
            {
                let result: CompactSummaryResult =
                    serde_json::from_value(value).unwrap_or_default();
                if let Some(replacement) = result.summary.filter(|s| !s.trim().is_empty()) {
                    current = Some(replacement);
                }
            }
        }
        current
    }

    /// Whether any extension declared one of these hooks — callers skip the
    /// round trip (and its bookkeeping) entirely when none did.
    pub fn has_hook(&self, hook: &str) -> bool {
        self.extensions
            .iter()
            .any(|e| e.manifest.hooks.iter().any(|h| h == hook))
    }

    /// Fire-and-forget lifecycle event to every subscribed extension. A
    /// version-1 manifest (no `events`) receives `turn_end` alone. try_send:
    /// a child that stopped reading stdin gets its queue dropped, never our
    /// loop.
    pub async fn event(&self, name: &str, params: Value) {
        let line =
            json!({"method": "event", "params": {"name": name, "extra": params}}).to_string();
        for ext in &self.extensions {
            let subscribed = match &ext.manifest.events {
                None => name == "turn_end",
                Some(events) => events.iter().any(|e| e == name),
            };
            if subscribed {
                let _ = ext.writer.try_send(line.clone());
            }
        }
    }

    /// A notification to one extension by name (`ui.key` for an
    /// interactive panel). Unknown names and full queues are dropped.
    pub fn notify_extension(&self, extension: &str, method: &str, params: Value) {
        if let Some(ext) = self
            .extensions
            .iter()
            .find(|e| e.manifest.name == extension)
        {
            let line = json!({"method": method, "params": params}).to_string();
            let _ = ext.writer.try_send(line);
        }
    }

    /// Graceful shutdown: a notification, a beat, then the processes die
    /// and are reaped. try_send throughout — quitting must never block on
    /// a wedged child.
    pub async fn shutdown(&self) {
        let line = json!({"method": "shutdown"}).to_string();
        for ext in &self.extensions {
            // Retire before asking: an exit we requested is not news for
            // the transcript, and no new request starts on a leaving child.
            ext.link.retire();
            let _ = ext.writer.try_send(line.clone());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        futures::future::join_all(self.extensions.iter().map(|ext| ext.link.reap())).await;
    }

    async fn request(
        &self,
        ext: &Extension,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.request_with_progress(ext, method, params, timeout, None)
            .await
    }

    async fn request_with_progress(
        &self,
        ext: &Extension,
        method: &str,
        params: Value,
        timeout: Duration,
        progress: Option<mpsc::Sender<ToolProgress>>,
    ) -> Result<Value, String> {
        if !ext.link.alive.load(Ordering::SeqCst) {
            return Err("extension exited".into());
        }
        let id = self.ids.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        ext.link
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        if let Some(progress) = progress {
            ext.link
                .progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id, progress);
        }
        // Whatever ends this call — response, timeout, or the caller
        // dropping the future on Esc — the pending entry goes with it: a
        // stale sender must not linger until the extension answers.
        let _guard = PendingGuard {
            link: ext.link.clone(),
            id,
        };
        // Close the race with the stdout reader ending between the first
        // liveness check and inserting this request into the pending map.
        if !ext.link.alive.load(Ordering::SeqCst) {
            return Err("extension exited".into());
        }
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        // The whole exchange shares one budget — including the enqueue: an
        // extension that stops reading stdin fills the pipe and the channel,
        // and an unbounded send here would hang past every timeout.
        match tokio::time::timeout(timeout, async {
            if ext.writer.send(line).await.is_err() {
                return Err("extension exited".to_string());
            }
            match rx.await {
                Ok(result) => result,
                Err(_) => Err("extension exited".into()),
            }
        })
        .await
        {
            Ok(result) => result,
            Err(_) => Err("timed out".into()),
        }
    }
}

/// One transcript notice from several extensions' `hook.input` notices, or
/// none when nobody said anything.
fn join_notices(notices: Vec<String>) -> Option<String> {
    (!notices.is_empty()).then(|| notices.join("\n"))
}

/// Removes a pending-map entry when its request ends by any path, including
/// the caller dropping the request future.
struct PendingGuard {
    link: Arc<Link>,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.link
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        self.link
            .progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// Extensions under `~/.e/extensions/`, then each installed package's
/// `extensions/` in settings order. A top-level executable is one extension.
/// A subdirectory can bundle an entry point with helper files. Entry-point
/// selection checks the first `index.*` executable in path order, a file
/// matching the directory name, then a sole executable.
fn discover() -> Vec<PathBuf> {
    let mut paths = scan(&home::extensions_dir());
    for (dir, filter) in crate::core::resources::packages::dirs("extensions") {
        // A package's filter names the top-level file or bundle directory,
        // never the entry point inside a bundle.
        paths.extend(scan(&dir).into_iter().filter(|path| {
            let top = path
                .strip_prefix(&dir)
                .ok()
                .and_then(|rel| rel.components().next())
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            filter.allows("extensions", &top)
        }));
    }
    paths
}

/// One extensions directory, sorted so launch order is stable.
fn scan(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(entry_point) = directory_entry_point(&path) {
                paths.push(entry_point);
            }
        } else if path.is_file() && is_executable(&path) {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

/// The executable a directory extension runs. Node (and every other language's
/// relative imports) resolve against this file's own directory, so a bundled
/// `./scaffold.mjs` beside it resolves regardless of the process cwd.
fn directory_entry_point(dir: &std::path::Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_string_lossy().into_owned();
    let mut execs: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_executable(p))
        .collect();
    // read_dir order is filesystem-dependent. Sorting makes ambiguous bundles
    // stable while preserving the documented entry-point precedence.
    execs.sort();
    let by_stem = |stem: &str| {
        execs
            .iter()
            .find(|p| p.file_stem().is_some_and(|s| s == stem))
            .cloned()
    };
    by_stem("index")
        .or_else(|| by_stem(&name))
        .or_else(|| (execs.len() == 1).then(|| execs[0].clone()))
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
#[cfg(not(unix))]
fn is_executable(_path: &std::path::Path) -> bool {
    true
}

/// Hand one extension request to the surface owner, or answer it at once
/// when there is none, the owner is gone, or the extension already has
/// [`MAX_INFLIGHT_REQUESTS`] unanswered. The reply, whenever it comes,
/// goes back down the extension's stdin with the extension's own id.
fn forward(
    name: &Arc<std::sync::OnceLock<String>>,
    requests: &Option<mpsc::Sender<HostRequest>>,
    inflight: &Arc<std::sync::atomic::AtomicUsize>,
    writer: &mpsc::Sender<String>,
    id: Value,
    method: String,
    params: Value,
) {
    fn answer(id: &Value, result: Result<Value, String>) -> String {
        match result {
            Ok(value) => json!({"id": id, "result": value}),
            Err(error) => json!({"id": id, "error": error}),
        }
        .to_string()
    }
    // An immediate error is still a reply the extension waits for: it
    // queues for the writer like any other rather than being dropped when
    // the channel is momentarily full.
    fn refuse(writer: &mpsc::Sender<String>, id: &Value, error: String) {
        let line = answer(id, Err(error));
        let writer = writer.clone();
        tokio::spawn(async move {
            let _ = writer.send(line).await;
        });
    }
    let Some(requests) = requests else {
        refuse(writer, &id, "no ui".into());
        return;
    };
    if inflight.load(Ordering::SeqCst) >= MAX_INFLIGHT_REQUESTS {
        refuse(
            writer,
            &id,
            format!("too many requests in flight (limit {MAX_INFLIGHT_REQUESTS})"),
        );
        return;
    }
    let (tx, rx) = oneshot::channel();
    let request = HostRequest {
        extension: name.get().cloned().unwrap_or_default(),
        method,
        params,
        reply: Some(tx),
    };
    if requests.try_send(request).is_err() {
        refuse(writer, &id, "ui unavailable".into());
        return;
    }
    inflight.fetch_add(1, Ordering::SeqCst);
    let inflight = inflight.clone();
    let writer = writer.clone();
    tokio::spawn(async move {
        let result = rx.await.unwrap_or_else(|_| Err("request dropped".into()));
        inflight.fetch_sub(1, Ordering::SeqCst);
        let _ = writer.send(answer(&id, result)).await;
    });
}

async fn spawn(
    path: &PathBuf,
    cwd: &Path,
    notices: mpsc::Sender<String>,
    startup_registry: Option<&Arc<Mutex<Vec<Arc<Link>>>>>,
    requests: Option<mpsc::Sender<HostRequest>>,
) -> Result<Extension, String> {
    let mut child = tokio::process::Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(cwd)
        // Every exit path reaps through `Link::reap`; this is the backstop
        // for a child that outlives the reap timeout and lands in tokio's
        // orphan queue instead.
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to start: {e}"))?;

    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take();
    let source = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "extension".into());

    if let Some(stderr) = stderr {
        let notices = notices.clone();
        let source = source.clone();
        // stderr is diagnostics, not protocol: an over-long line is reported
        // and its remainder dropped, then reading goes on. The pipe must
        // stay open as long as the child lives — closing it would turn the
        // child's next stderr write into SIGPIPE/EPIPE and kill it.
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            loop {
                match read_bounded_line(&mut reader, MAX_EXTENSION_LINE_BYTES).await {
                    Ok(Some(line)) if !line.trim().is_empty() => {
                        let _ = notices.try_send(format!("extension {source}: {line}"));
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(error) => {
                        let _ = notices.try_send(format!("extension {source}: {error}"));
                        // Only the byte cap leaves the stream mid-line; a
                        // UTF-8 failure has already consumed its line, and
                        // resyncing there would eat the next one too.
                        let mid_line = error
                            .get_ref()
                            .is_none_or(|inner| !inner.is::<std::string::FromUtf8Error>());
                        if mid_line && discard_line(&mut reader).await.is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    let link = Arc::new(Link {
        alive: AtomicBool::new(true),
        pending: Mutex::new(HashMap::new()),
        progress: Mutex::new(HashMap::new()),
        child: tokio::sync::Mutex::new(Some(child)),
        exit_notice: Mutex::new((None, false)),
        notices,
    });
    // Register before the handshake begins. If the startup future is
    // cancelled while we await `initialize`, the guard still finds this
    // child and kills it.
    if let Some(registry) = startup_registry {
        registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(link.clone());
    }

    // Writer task: serialized line output.
    let (writer, mut writer_rx) = mpsc::channel::<String>(64);
    let link_writer = link.clone();
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(line) = writer_rx.recv().await {
            let written: std::io::Result<()> = async {
                stdin.write_all(line.as_bytes()).await?;
                stdin.write_all(b"\n").await?;
                stdin.flush().await
            }
            .await;
            if written.is_err() {
                link_writer.exited();
                break;
            }
        }
    });

    // The manifest name, learned from the manifest response below so the
    // requests an extension sends right behind its manifest carry it.
    let name: Arc<std::sync::OnceLock<String>> = Arc::new(std::sync::OnceLock::new());
    let ui = requests.is_some();

    // Reader task: route responses to pending waiters, notifies to the app,
    // and the extension's own requests to the surface owner.
    let link_reader = link.clone();
    let name_reader = name.clone();
    let writer_reader = writer.clone();
    let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut early: Vec<(Value, String, Value)> = Vec::new();
        while let Ok(Some(line)) = read_bounded_line(&mut reader, MAX_EXTENSION_LINE_BYTES).await {
            match protocol::parse_incoming(&line) {
                Some(Incoming::Request { id, method, params }) => {
                    if name_reader.get().is_none() {
                        // Sent on the heels of the manifest, before its
                        // name is known here: held, in order, until the
                        // manifest response below names the extension.
                        early.push((id, method, params));
                        continue;
                    }
                    forward(
                        &name_reader,
                        &requests,
                        &inflight,
                        &writer_reader,
                        id,
                        method,
                        params,
                    );
                }
                Some(Incoming::Response { id, result }) => {
                    // The first response an extension ever sends is its
                    // manifest; learning the name here (rather than after
                    // the handshake task parses it) lets requests sent
                    // right behind the manifest carry the right name.
                    if name_reader.get().is_none() {
                        if let Some(name) = result
                            .as_ref()
                            .ok()
                            .and_then(|v| v.get("name"))
                            .and_then(Value::as_str)
                            .filter(|n| !n.is_empty())
                        {
                            let _ = name_reader.set(name.to_string());
                        }
                    }
                    if let Some(tx) = link_reader
                        .pending
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id)
                    {
                        let _ = tx.send(result);
                    }
                    if name_reader.get().is_some() {
                        for (id, method, params) in early.drain(..) {
                            forward(
                                &name_reader,
                                &requests,
                                &inflight,
                                &writer_reader,
                                id,
                                method,
                                params,
                            );
                        }
                    }
                }
                Some(Incoming::Notify { message }) => {
                    // Notices are best-effort UI output. A full transcript
                    // channel must never hold up response dispatch.
                    let _ = link_reader.notices.try_send(message);
                }
                Some(Incoming::ToolUpdate { id, stream, chunk }) => {
                    let target = link_reader
                        .progress
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&id)
                        .cloned();
                    if let Some(tx) = target {
                        // Backpressure keeps progress ordered ahead of the
                        // response line that follows it on extension stdout.
                        let _ = tx.send(ToolProgress { stream, chunk }).await;
                    }
                }
                None => {}
            }
        }
        // stdout is done: the process exited, closed its end, or broke the
        // protocol past the line cap. It is finished either way — fail the
        // waiters, say so, and reap it.
        link_reader.exited();
        link_reader.reap().await;
    });

    // Handshake.
    let ext = Extension {
        manifest: Manifest::default(),
        writer,
        link: link.clone(),
    };
    let host_shim = ExtensionHost {
        extensions: Vec::new(),
        ids: AtomicU64::new(1_000_000),
    };
    let init = json!({
        "protocol": protocol::PROTOCOL_VERSION,
        "capabilities": protocol::CAPABILITIES,
        // Whether `ui.*` requests can reach a person; false under `e rpc`.
        "ui": ui,
        "e_version": crate::VERSION,
        "cwd": cwd.display().to_string(),
        // Namespaced extension config from ~/.e/settings.json:
        // {"extensions":{"<name>":{…}}} — each extension reads its own key.
        "extensions_config": crate::core::config::settings::extensions_config(),
    });
    let handshake = async {
        let value = host_shim
            .request(&ext, "initialize", init, INIT_TIMEOUT)
            .await
            .map_err(|e| format!("initialize {e}"))?;
        let manifest: Manifest =
            serde_json::from_value(value).map_err(|e| format!("bad manifest: {e}"))?;
        if manifest.name.is_empty() {
            return Err("manifest has no name".into());
        }
        Ok::<Manifest, String>(manifest)
    };
    let manifest = match handshake.await {
        Ok(manifest) => manifest,
        Err(reason) => {
            link.reap().await;
            return Err(reason);
        }
    };
    link.install_exit_notice(exit_notice(&source, &manifest));
    let _ = name.set(manifest.name.clone());
    Ok(Extension { manifest, ..ext })
}

/// Chords e keeps for itself: interrupt and quit, the viewer and model
/// pickers, the external editor, clipboard paste and copy, the scoped-models
/// save, the terminal's own suspend and clear. Everything else
/// with a ctrl or alt modifier is an extension's to declare; bare keys and
/// shift-only chords never are, since they are how text gets typed.
const RESERVED_CHORDS: &[&str] = &[
    "ctrl+c",
    "ctrl+d",
    "ctrl+g",
    "ctrl+i",
    "ctrl+j",
    "ctrl+l",
    "ctrl+m",
    "ctrl+o",
    "ctrl+p",
    "ctrl+shift+p",
    "ctrl+s",
    "ctrl+v",
    "ctrl+shift+v",
    "ctrl+x",
    "ctrl+z",
];

/// Whether a normalized chord may be declared as an extension shortcut.
pub fn shortcut_allowed(chord: &str) -> bool {
    !chord.is_empty()
        && (chord.starts_with("ctrl+") || chord.starts_with("alt+"))
        && !RESERVED_CHORDS.contains(&chord)
}

/// The keybindings grammar's canonical chord, shared with the composer's
/// own overrides so `Ctrl+Shift+G` and `shift+ctrl+g` are one key. A
/// modifier with no key is nothing to bind.
pub fn normalize_chord(chord: &str) -> String {
    let normalized = crate::core::config::keybindings::normalize_chord(chord);
    let key = normalized.rsplit('+').next().unwrap_or("");
    if key.is_empty() && !normalized.ends_with("++") {
        return String::new();
    }
    normalized
}

/// The transcript line for an extension that dies mid-session. A guard
/// (`tool_call`/`input` hooks) that is gone deserves a louder line: the
/// hooks fail open, so its protection has silently lapsed.
fn exit_notice(source: &str, manifest: &Manifest) -> String {
    let guards: Vec<&str> = manifest
        .hooks
        .iter()
        .map(String::as_str)
        .filter(|hook| matches!(*hook, "tool_call" | "input"))
        .collect();
    if guards.is_empty() {
        format!("extension {source}: exited")
    } else {
        format!(
            "extension {source}: exited — its {} hook no longer applies",
            guards.join(" and ")
        )
    }
}

/// Read one line without allowing an unbounded peer to grow memory
/// indefinitely — a misbehaving extension on the other flavor of this call,
/// or (via the `pub` re-export) an RPC client sending an unterminated or
/// giant line. The newline is consumed but not returned.
///
/// Errors the instant the cap is crossed, before a newline is even in
/// view — deliberately, not just as a memory bound: a still-growing line
/// with no newline yet (a firehose, or a client that never terminates one)
/// must be cut off promptly rather than read forever looking for a
/// newline that may never come. The stream is left mid-line, never
/// resynced: protocol readers treat the error as fatal to their loop, and
/// the one diagnostics reader (extension stderr) resyncs itself with
/// `discard_line`.
pub async fn read_bounded_line<R>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(None);
            }
            break;
        }

        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if line.len().saturating_add(newline) > max_bytes {
                // Leave the newline in place so the stream is mid-line
                // here exactly as in the no-newline-yet case below.
                reader.consume(newline);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("line exceeded {max_bytes} bytes"),
                ));
            }
            line.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            break;
        }

        let count = available.len();
        if line.len().saturating_add(count) > max_bytes {
            reader.consume(count);
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("line exceeded {max_bytes} bytes"),
            ));
        }
        line.extend_from_slice(available);
        reader.consume(count);
    }

    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// Skip to just past the next newline (or EOF): the resync after
/// `read_bounded_line` hits its cap on a stream where dropping the rest of
/// the line is the right call. Reads and discards without buffering, so a
/// firehose costs nothing but time.
async fn discard_line<R>(reader: &mut R) -> std::io::Result<()>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(());
        }
        match available.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                reader.consume(newline + 1);
                return Ok(());
            }
            None => {
                let count = available.len();
                reader.consume(count);
            }
        }
    }
}

#[cfg(test)]
mod tests {

    /// EOF before or after manifest installation announces one unexpected exit.
    #[test]
    fn exit_notice_survives_eof_before_handshake_continuation() {
        for early in [false, true] {
            let (notices, mut rx) = tokio::sync::mpsc::channel(4);
            let link = super::Link {
                alive: std::sync::atomic::AtomicBool::new(true),
                pending: std::sync::Mutex::new(std::collections::HashMap::new()),
                progress: std::sync::Mutex::new(std::collections::HashMap::new()),
                child: tokio::sync::Mutex::new(None),
                exit_notice: std::sync::Mutex::new((None, false)),
                notices,
            };
            if early {
                link.exited();
            }
            link.install_exit_notice("guard exited; hooks fail open".into());
            link.exited();
            link.exited();
            assert_eq!(rx.try_recv().unwrap(), "guard exited; hooks fail open");
            assert!(rx.try_recv().is_err());
        }
    }

    // The entry-point tests set file modes, so they are unix-only. Nested so
    // the module opens with a bare `#[cfg(test)]` (the guard's prod/test split
    // keys on that) without mixing an inner `#![cfg]` on the same module.
    #[cfg(unix)]
    mod unix {
        use super::super::directory_entry_point;
        use std::os::unix::fs::PermissionsExt as _;

        fn write(dir: &std::path::Path, name: &str, executable: bool) {
            let path = dir.join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            let mode = if executable { 0o755 } else { 0o644 };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        fn tmp(label: &str) -> std::path::PathBuf {
            let dir = std::env::temp_dir().join(format!(
                "e-entrypoint-{label}-{}-{}",
                std::process::id(),
                uuid::Uuid::now_v7()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        #[test]
        fn index_wins_and_the_non_executable_scaffold_is_skipped() {
            let dir = tmp("index");
            write(&dir, "index.mjs", true);
            write(&dir, "index.sh", true); // lexical tie-break after the stem rule
            write(&dir, "scaffold.mjs", false); // library, not an entry point
            write(&dir, "helper.mjs", true); // another executable, but index wins
            assert_eq!(directory_entry_point(&dir), Some(dir.join("index.mjs")));
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_file_named_for_the_bundle_is_the_entry_point() {
            let dir = tmp("subagent");
            // The bundle directory is named "e-entrypoint-subagent-…"; use a file
            // whose stem matches the directory name exactly.
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            write(&dir, &format!("{name}.mjs"), true);
            write(&dir, "scaffold.mjs", false);
            assert_eq!(
                directory_entry_point(&dir),
                Some(dir.join(format!("{name}.mjs")))
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn a_lone_executable_is_the_entry_point() {
            let dir = tmp("lone");
            write(&dir, "whatever.sh", true);
            write(&dir, "data.json", false);
            assert_eq!(directory_entry_point(&dir), Some(dir.join("whatever.sh")));
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn no_executable_means_no_entry_point() {
            let dir = tmp("empty");
            write(&dir, "readme.md", false);
            assert_eq!(directory_entry_point(&dir), None);
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
