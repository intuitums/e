//! The extension host: discovers executables in `~/.e/extensions/`, keeps one
//! long-lived process per extension, and routes tools, commands, hooks, and
//! events over the line protocol.
//!
//! Failure posture: an extension that won't start or answer is skipped or
//! timed out and reported — never a reason the harness can't run. Hooks fail
//! open (a broken gatekeeper doesn't brick the agent); a block only happens on
//! an explicit verdict.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use super::protocol::{self, CommandResult, HookVerdict, Incoming, Manifest, ToolResult};
use crate::core::home;

const INIT_TIMEOUT: Duration = Duration::from_secs(5);
const HOOK_TIMEOUT: Duration = Duration::from_secs(5);
const TOOL_TIMEOUT: Duration = Duration::from_secs(300);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

struct Extension {
    manifest: Manifest,
    /// Outgoing lines to the process's stdin.
    writer: mpsc::Sender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    child: Mutex<Option<tokio::process::Child>>,
}

pub struct ExtensionHost {
    extensions: Vec<Extension>,
    ids: AtomicU64,
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
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let _ = notices.send(format!("extension {name}: {reason}")).await;
                }
            }
        }
        Arc::new(ExtensionHost { extensions, ids: AtomicU64::new(1) })
    }

    /// An empty host, for sessions with no extensions (and for tests).
    pub fn empty() -> Arc<ExtensionHost> {
        Arc::new(ExtensionHost { extensions: Vec::new(), ids: AtomicU64::new(1) })
    }

    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty()
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
        self.extensions.iter().any(|e| e.manifest.tools.iter().any(|t| t.name == name))
    }

    /// `(name, description)` of every extension command, for the / picker.
    pub fn commands(&self) -> Vec<(String, String)> {
        self.extensions
            .iter()
            .flat_map(|e| e.manifest.commands.iter().map(|c| (c.name.clone(), c.description.clone())))
            .collect()
    }

    pub fn has_command(&self, name: &str) -> bool {
        self.extensions.iter().any(|e| e.manifest.commands.iter().any(|c| c.name == name))
    }

    pub async fn call_tool(&self, name: &str, arguments: &str) -> ToolResult {
        let Some(ext) = self.extensions.iter().find(|e| e.manifest.tools.iter().any(|t| t.name == name)) else {
            return ToolResult { content: format!("no extension owns tool {name}"), is_error: true };
        };
        let args: Value = serde_json::from_str(arguments).unwrap_or(Value::String(arguments.into()));
        match self
            .request(ext, "tool_call", json!({"name": name, "arguments": args}), TOOL_TIMEOUT)
            .await
        {
            Ok(value) => serde_json::from_value(value).unwrap_or_default(),
            Err(reason) => ToolResult { content: format!("{name}: {reason}"), is_error: true },
        }
    }

    pub async fn run_command(&self, name: &str, args: &str) -> CommandResult {
        let Some(ext) = self.extensions.iter().find(|e| e.manifest.commands.iter().any(|c| c.name == name)) else {
            return CommandResult { notice: Some(format!("no extension owns /{name}")), prompt: None };
        };
        match self
            .request(ext, "command", json!({"name": name, "args": args}), COMMAND_TIMEOUT)
            .await
        {
            Ok(value) => serde_json::from_value(value).unwrap_or_default(),
            Err(reason) => CommandResult { notice: Some(format!("/{name}: {reason}")), prompt: None },
        }
    }

    /// Ask every extension with the `tool_call` hook. The first explicit block
    /// wins; transport failures and timeouts allow (fail open).
    pub async fn hook_tool_call(&self, name: &str, arguments: &str) -> Option<String> {
        let args: Value = serde_json::from_str(arguments).unwrap_or(Value::String(arguments.into()));
        for ext in &self.extensions {
            if !ext.manifest.hooks.iter().any(|h| h == "tool_call") {
                continue;
            }
            if let Ok(value) = self
                .request(ext, "hook.tool_call", json!({"name": name, "arguments": args}), HOOK_TIMEOUT)
                .await
            {
                let verdict: HookVerdict = serde_json::from_value(value).unwrap_or_default();
                if verdict.block {
                    return Some(verdict.reason.unwrap_or_else(|| format!("blocked by {}", ext.manifest.name)));
                }
            }
        }
        None
    }

    /// Fire-and-forget lifecycle event to every extension.
    pub async fn event(&self, name: &str, params: Value) {
        let line = json!({"method": "event", "params": {"name": name, "extra": params}}).to_string();
        for ext in &self.extensions {
            let _ = ext.writer.send(line.clone()).await;
        }
    }

    /// Graceful shutdown: a notification, a beat, then the processes die.
    pub async fn shutdown(&self) {
        let line = json!({"method": "shutdown"}).to_string();
        for ext in &self.extensions {
            let _ = ext.writer.send(line.clone()).await;
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
        let id = self.ids.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        ext.pending.lock().unwrap().insert(id, tx);
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        ext.writer.send(line).await.map_err(|_| "extension exited".to_string())?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("extension exited".into()),
            Err(_) => {
                ext.pending.lock().unwrap().remove(&id);
                Err("timed out".into())
            }
        }
    }
}

/// Executable files directly under `~/.e/extensions/`.
fn discover() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home::extensions_dir()) else { return Vec::new() };
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
    std::fs::metadata(path).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}
#[cfg(not(unix))]
fn is_executable(_path: &std::path::Path) -> bool {
    true
}

async fn spawn(path: &PathBuf, notices: mpsc::Sender<String>) -> Result<Extension, String> {
    let mut child = tokio::process::Command::new(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .current_dir(std::env::current_dir().unwrap_or_default())
        .spawn()
        .map_err(|e| format!("failed to start: {e}"))?;

    let stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;

    // Writer task: serialized line output.
    let (writer, mut writer_rx) = mpsc::channel::<String>(64);
    tokio::spawn(async move {
        let mut stdin = stdin;
        while let Some(line) = writer_rx.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if stdin.write_all(b"\n").await.is_err() {
                break;
            }
            let _ = stdin.flush().await;
        }
    });

    // Reader task: route responses to pending waiters, notifies to the app.
    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let pending_reader = pending.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match protocol::parse_incoming(&line) {
                Some(Incoming::Response { id, result }) => {
                    if let Some(tx) = pending_reader.lock().unwrap().remove(&id) {
                        let _ = tx.send(result);
                    }
                }
                Some(Incoming::Notify { message }) => {
                    let _ = notices.send(message).await;
                }
                None => {}
            }
        }
    });

    // Handshake.
    let ext = Extension { manifest: Manifest::default(), writer, pending, child: Mutex::new(Some(child)) };
    let host_shim = ExtensionHost { extensions: Vec::new(), ids: AtomicU64::new(1_000_000) };
    let init = json!({
        "protocol": protocol::PROTOCOL_VERSION,
        "e_version": crate::VERSION,
        "cwd": std::env::current_dir().unwrap_or_default().display().to_string(),
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
