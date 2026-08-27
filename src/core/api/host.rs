//! The extension host: discovers executables in `~/.e/extensions/`, keeps one
//! long-lived process per extension, and routes tools, commands, hooks, and
//! events over the line protocol.
//!
//! Failure posture: discovery and runtime hooks fail open and are reported.
//! An extension that advertises a startup hook owns startup argument handling,
//! so its explicit failure is fatal rather than leaking consumed arguments
//! into the user's prompt.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::protocol::{
    self, CommandResult, HookVerdict, Incoming, InputVerdict, Manifest, Relaunch, StartupResult,
    ToolResult,
};
use crate::core::config::home;

const INIT_TIMEOUT: Duration = Duration::from_secs(5);
const HOOK_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_TIMEOUT: Duration = Duration::from_secs(300);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_EXTENSION_LINE_BYTES: usize = 1024 * 1024;

/// Requests awaiting a response, keyed by wire id.
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
type ProgressMap = Arc<Mutex<HashMap<u64, mpsc::Sender<ToolProgress>>>>;

#[derive(Clone, Debug)]
pub struct ToolProgress {
    pub stream: crate::core::tools::OutputStream,
    pub chunk: String,
}

struct Extension {
    manifest: Manifest,
    /// Outgoing lines to the process's stdin.
    writer: mpsc::Sender<String>,
    pending: PendingMap,
    progress: ProgressMap,
    /// False once either process pipe proves the extension has exited.
    /// Pending requests fail immediately and new ones are refused.
    alive: Arc<AtomicBool>,
    child: Mutex<Option<tokio::process::Child>>,
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

impl ExtensionHost {
    /// Discover and start every extension. `notices` receives extension
    /// `notify` messages and startup diagnostics for the transcript.
    pub async fn start(notices: mpsc::Sender<String>) -> Arc<ExtensionHost> {
        let mut extensions = Vec::new();
        for path in discover() {
            match spawn(&path, notices.clone()).await {
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
        let host = Arc::new(ExtensionHost {
            extensions,
            ids: AtomicU64::new(1),
        });
        // Every extension that declares typed flags gets them now, before any
        // startup hook — a tool-only extension can read its flags anytime, not
        // just during startup. `flags` is a notification (no reply expected).
        let argv: Vec<String> = std::env::args().skip(1).collect();
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
            // Match this arg against every declared flag. A separated string
            // value (`--name value`) is consumed at most once, whichever
            // declaration matches first.
            let mut consumed_value = false;
            let mut match_flag = |parsed: &mut serde_json::Map<String, serde_json::Value>| {
                for ext in &self.extensions {
                    for flag in &ext.manifest.flags {
                        let Some(long) = flag.long_form() else {
                            continue; // display-only name
                        };
                        if flag.flag_type == "string" {
                            if let Some(value) = arg.strip_prefix(&format!("{long}=")) {
                                parsed.insert(flag.name.clone(), serde_json::json!(value));
                            } else if &long == arg {
                                // `--name value`, or null when the next
                                // token is another flag or absent.
                                match argv.get(i + 1) {
                                    Some(next) if next != "-" && !next.starts_with('-') => {
                                        if !consumed_value {
                                            parsed
                                                .insert(flag.name.clone(), serde_json::json!(next));
                                            consumed_value = true;
                                        }
                                    }
                                    _ => {
                                        parsed.insert(flag.name.clone(), serde_json::Value::Null);
                                    }
                                }
                            }
                        } else if &long == arg {
                            parsed.insert(flag.name.clone(), serde_json::json!(true));
                        } else if let Some(value) = arg.strip_prefix(&format!("{long}=")) {
                            let on = matches!(
                                value.to_ascii_lowercase().as_str(),
                                "1" | "true" | "yes" | "on"
                            );
                            parsed.insert(flag.name.clone(), serde_json::json!(on));
                        } else if let Some(rest) = arg.strip_prefix("--no-") {
                            if rest == flag.name {
                                parsed.insert(flag.name.clone(), serde_json::json!(false));
                            }
                        }
                    }
                }
            };
            match_flag(&mut parsed);
            if consumed_value {
                i += 1; // skip the separated value slot
            }
            i += 1;
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
            for ext in &self.extensions {
                for flag in &ext.manifest.flags {
                    let Some(long) = flag.long_form() else {
                        continue;
                    };
                    if flag.flag_type == "string" {
                        if arg == &long {
                            matched = true;
                            consumes_next = argv
                                .get(i + 1)
                                .is_some_and(|next| next != "-" && !next.starts_with('-'));
                        } else if arg.strip_prefix(&format!("{long}=")).is_some() {
                            matched = true;
                        }
                    } else if arg == &long
                        || arg.strip_prefix(&format!("{long}=")).is_some()
                        || arg
                            .strip_prefix("--no-")
                            .is_some_and(|rest| rest == flag.name)
                    {
                        matched = true;
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
                session_name: None,
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
                session_name: None,
            }),
            Err(reason) => ToolResult {
                content: format!("{name}: {reason}"),
                is_error: true,
                session_name: None,
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
                prompt: None,
                session_name: None,
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
                prompt: None,
                session_name: None,
            }),
            Err(reason) => CommandResult {
                notice: Some(format!("/{name}: {reason}")),
                prompt: None,
                session_name: None,
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
    /// (fail open — a slow extension never eats a user's message).
    pub async fn hook_input(&self, text: &str) -> InputVerdict {
        // Fast path: no extension listens at all.
        if !self
            .extensions
            .iter()
            .any(|e| e.manifest.hooks.iter().any(|h| h == "input"))
        {
            return InputVerdict::default();
        }
        for ext in &self.extensions {
            if !ext.manifest.hooks.iter().any(|h| h == "input") {
                continue;
            }
            if let Ok(value) = self
                .request(ext, "hook.input", json!({"text": text}), HOOK_TIMEOUT)
                .await
            {
                let verdict: InputVerdict = serde_json::from_value(value).unwrap_or_default();
                if verdict.consume || verdict.replace.as_deref().is_some_and(|r| !r.is_empty()) {
                    return verdict;
                }
            }
        }
        InputVerdict::default()
    }

    /// Fire-and-forget lifecycle event to every extension. try_send: a child
    /// that stopped reading stdin gets its queue dropped, never our loop.
    pub async fn event(&self, name: &str, params: Value) {
        let line =
            json!({"method": "event", "params": {"name": name, "extra": params}}).to_string();
        for ext in &self.extensions {
            let _ = ext.writer.try_send(line.clone());
        }
    }

    /// Graceful shutdown: a notification, a beat, then the processes die.
    /// try_send throughout — quitting must never block on a wedged child.
    pub async fn shutdown(&self) {
        let line = json!({"method": "shutdown"}).to_string();
        for ext in &self.extensions {
            let _ = ext.writer.try_send(line.clone());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        for ext in &self.extensions {
            if let Some(child) = ext.child.lock().unwrap().as_mut() {
                let _ = child.start_kill();
            }
        }
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
        if !ext.alive.load(Ordering::SeqCst) {
            return Err("extension exited".into());
        }
        let id = self.ids.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        ext.pending.lock().unwrap().insert(id, tx);
        if let Some(progress) = progress {
            ext.progress.lock().unwrap().insert(id, progress);
        }
        // Whatever ends this call — response, timeout, or the caller
        // dropping the future on Esc — the pending entry goes with it: a
        // stale sender must not linger until the extension answers.
        let _guard = PendingGuard {
            pending: ext.pending.clone(),
            progress: ext.progress.clone(),
            id,
        };
        // Close the race with the stdout reader ending between the first
        // liveness check and inserting this request into the pending map.
        if !ext.alive.load(Ordering::SeqCst) {
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

/// Removes a pending-map entry when its request ends by any path, including
/// the caller dropping the request future.
struct PendingGuard {
    pending: PendingMap,
    progress: ProgressMap,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
        self.progress.lock().unwrap().remove(&self.id);
    }
}

/// Executable files directly under `~/.e/extensions/`.
fn discover() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home::extensions_dir()) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_executable(p))
        .collect();
    paths.sort();
    paths
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

async fn spawn(path: &PathBuf, notices: mpsc::Sender<String>) -> Result<Extension, String> {
    let mut child = tokio::process::Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(std::env::current_dir().unwrap_or_default())
        // A failed handshake drops the child below; without this, a process
        // that ignores stdin EOF would outlive every `?` early return as an
        // untracked orphan.
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to start: {e}"))?;

    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take();

    if let Some(stderr) = stderr {
        let notices = notices.clone();
        let source = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "extension".into());
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
                        break;
                    }
                }
            }
        });
    }

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let progress: ProgressMap = Arc::new(Mutex::new(HashMap::new()));
    let alive = Arc::new(AtomicBool::new(true));

    // Writer task: serialized line output.
    let (writer, mut writer_rx) = mpsc::channel::<String>(64);
    let pending_writer = pending.clone();
    let progress_writer = progress.clone();
    let alive_writer = alive.clone();
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(line) = writer_rx.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err() {
                alive_writer.store(false, Ordering::SeqCst);
                pending_writer.lock().unwrap().clear();
                progress_writer.lock().unwrap().clear();
                break;
            }
            if stdin.write_all(b"\n").await.is_err() {
                alive_writer.store(false, Ordering::SeqCst);
                pending_writer.lock().unwrap().clear();
                progress_writer.lock().unwrap().clear();
                break;
            }
            if stdin.flush().await.is_err() {
                alive_writer.store(false, Ordering::SeqCst);
                pending_writer.lock().unwrap().clear();
                progress_writer.lock().unwrap().clear();
                break;
            }
        }
    });

    // Reader task: route responses to pending waiters, notifies to the app.
    let pending_reader = pending.clone();
    let progress_reader = progress.clone();
    let alive_reader = alive.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(line)) = read_bounded_line(&mut reader, MAX_EXTENSION_LINE_BYTES).await {
            match protocol::parse_incoming(&line) {
                Some(Incoming::Response { id, result }) => {
                    if let Some(tx) = pending_reader.lock().unwrap().remove(&id) {
                        let _ = tx.send(result);
                    }
                }
                Some(Incoming::Notify { message }) => {
                    // Notices are best-effort UI output. A full transcript
                    // channel must never hold up response dispatch.
                    let _ = notices.try_send(message);
                }
                Some(Incoming::ToolUpdate { id, stream, chunk }) => {
                    let target = progress_reader.lock().unwrap().get(&id).cloned();
                    if let Some(tx) = target {
                        // Backpressure keeps progress ordered ahead of the
                        // response line that follows it on extension stdout.
                        let _ = tx.send(ToolProgress { stream, chunk }).await;
                    }
                }
                None => {}
            }
        }
        alive_reader.store(false, Ordering::SeqCst);
        // Dropping the senders wakes every request through its
        // `Ok(Err(_)) => extension exited` path instead of its long timeout.
        pending_reader.lock().unwrap().clear();
        progress_reader.lock().unwrap().clear();
    });

    // Handshake.
    let ext = Extension {
        manifest: Manifest::default(),
        writer,
        pending,
        progress,
        alive,
        child: Mutex::new(Some(child)),
    };
    let host_shim = ExtensionHost {
        extensions: Vec::new(),
        ids: AtomicU64::new(1_000_000),
    };
    let init = json!({
        "protocol": protocol::PROTOCOL_VERSION,
        "capabilities": ["tool.update"],
        "e_version": crate::VERSION,
        "cwd": std::env::current_dir().unwrap_or_default().display().to_string(),
        // Namespaced extension config from ~/.e/settings.json:
        // {"extensions":{"<name>":{…}}} — each extension reads its own key.
        "extensions_config": crate::core::config::settings::extensions_config(),
    });
    let manifest_value = host_shim
        .request(&ext, "initialize", init, INIT_TIMEOUT)
        .await
        .map_err(|e| format!("initialize {e}"))?;
    let manifest: Manifest =
        serde_json::from_value(manifest_value).map_err(|e| format!("bad manifest: {e}"))?;
    if manifest.name.is_empty() {
        return Err("manifest has no name".into());
    }
    Ok(Extension { manifest, ..ext })
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
/// newline that may never come. Every caller therefore treats this error
/// as fatal to its read loop, never as "skip this line and keep going" —
/// the stream is left mid-line, not resynced to the next one.
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
                reader.consume(newline + 1);
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
