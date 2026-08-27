//! The bash tool: spawn a shell command and stream its captured pipes.
//!
//! This remains spawn-and-capture, not a terminal daemon. A wall-clock timeout
//! or turn cancellation kills the command's process group. Pipe readers feed
//! one tagged queue so display and retained output keep observed ordering.
//!
//! `background: true` is the one exception to "spawn-and-capture, not a
//! daemon": the process outlives the call that started it (though never the
//! e process — no persistence, nothing survives a restart), tracked in
//! `BACKGROUND` by a handle the model checks or kills later. Everything else
//! about it — the process group, the 32KB retained tail, ANSI/carriage-return
//! cleanup — matches the foreground path; only the waiting is removed.

use serde_json::{json, Value};
use std::collections::HashMap;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{schema_object, OutputStream, ToolOutcome, ToolOutput};

pub fn schema() -> Value {
    schema_object(
        "bash",
        "Run a shell command in the workspace root and return its combined output. Each call is a fresh shell: cd, environment variables, and (unless started with `background: true`) background processes do not persist between calls. Output keeps the most recent 32KB when longer.\n\nFor something long-lived (a dev server, a watcher) that would otherwise block the turn: pass `background: true` to start it detached and get a `handle` back immediately, instead of waiting for it to exit. Check on it, or read more of its output, with a later call passing `handle` and no `command`; add `signal: \"kill\"` to stop it. A background process outlives the turn that started it but not the e process — nothing persists across a restart.",
        json!({
            "command": {"type": "string", "description": "The command to run. Omit when checking or killing a background process by `handle`."},
            "timeout": {"type": "integer", "description": "Seconds before the command is killed (default 120). Ignored when starting a background process — it runs until it exits or is killed."},
            "background": {"type": "boolean", "description": "Start `command` detached and return immediately with a `handle`, instead of waiting for it to finish."},
            "handle": {"type": "string", "description": "A background process's handle, from a prior background start. Returns its status and output so far; combine with `signal: \"kill\"` to stop it."},
            "signal": {"type": "string", "enum": ["kill"], "description": "Send with `handle` to kill that background process."}
        }),
        &[],
    )
}

/// Bytes kept per background process, tail-retained like the foreground
/// path's own cap — a runaway server logging forever must not grow forever.
const BACKGROUND_RETAIN_LIMIT: usize = 32 * 1024;

/// Handles retained before registering a new one starts evicting finished
/// ones. Bounds the map over a long session that starts and finishes many
/// background processes — nothing ever removed a completed entry otherwise,
/// so its `Arc`/mutexes (bounded output aside) lived for the rest of the e
/// process. A still-running handle is never evicted, no matter how many
/// there are — only a process whose exit was already recorded is fair game.
const MAX_BACKGROUND_HANDLES: usize = 64;

#[derive(Clone, Copy)]
enum ExitOutcome {
    Exited(i32),
    Killed,
}

struct BackgroundProcess {
    pid: u32,
    command: String,
    output: Mutex<Vec<u8>>,
    total_bytes: Mutex<usize>,
    exit: Mutex<Option<ExitOutcome>>,
    registered_at: Instant,
}

/// Live background processes, keyed by handle. Process-lifetime only — like
/// everything else in this tool, nothing here survives past the e process
/// itself; there is no daemon and no persistence to reload on restart.
static BACKGROUND: Mutex<Option<HashMap<String, Arc<BackgroundProcess>>>> = Mutex::new(None);

fn register_background(id: String, process: Arc<BackgroundProcess>) {
    let mut guard = BACKGROUND.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.len() >= MAX_BACKGROUND_HANDLES {
        let mut exited: Vec<(String, Instant)> = map
            .iter()
            .filter_map(|(id, p)| {
                let finished = p.exit.lock().unwrap_or_else(|e| e.into_inner()).is_some();
                finished.then(|| (id.clone(), p.registered_at))
            })
            .collect();
        exited.sort_by_key(|(_, at)| *at);
        for (id, _) in exited
            .into_iter()
            .take(map.len() + 1 - MAX_BACKGROUND_HANDLES)
        {
            map.remove(&id);
        }
    }
    map.insert(id, process);
}

fn find_background(id: &str) -> Option<Arc<BackgroundProcess>> {
    BACKGROUND
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|map| map.get(id).cloned())
}

/// Start `command` detached and return immediately with a handle. Output
/// keeps accumulating (capped) in the background; nothing here blocks the
/// calling turn.
fn start_background(command: &str, cwd: &Path) -> ToolOutput {
    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => return failure(&format!("bash: {error}")),
    };
    let pid = child.id();
    let id = uuid::Uuid::new_v4().to_string();
    let process = Arc::new(BackgroundProcess {
        pid,
        command: command.to_string(),
        output: Mutex::new(Vec::new()),
        total_bytes: Mutex::new(0),
        exit: Mutex::new(None),
        registered_at: Instant::now(),
    });
    register_background(id.clone(), process.clone());

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = stdout.map(|pipe| {
        let process = process.clone();
        std::thread::spawn(move || drain_into_background(pipe, process))
    });
    let stderr_thread = stderr.map(|pipe| {
        let process = process.clone();
        std::thread::spawn(move || drain_into_background(pipe, process))
    });
    std::thread::spawn(move || reap_background(child, process, stdout_thread, stderr_thread));

    ToolOutput {
        content: format!("started background process {id} (pid {pid}): {command}"),
        outcome: ToolOutcome::Completed,
        summary: format!("background {id}"),
        display: None,
    }
}

/// Drain one pipe into the process's capped, tail-retained buffer. No live
/// callback here — background output is read on demand, not streamed.
fn drain_into_background<R: std::io::Read>(mut pipe: R, process: Arc<BackgroundProcess>) {
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(count) => {
                let mut output = process.output.lock().unwrap_or_else(|e| e.into_inner());
                output.extend_from_slice(&buf[..count]);
                if output.len() > BACKGROUND_RETAIN_LIMIT {
                    let excess = output.len() - BACKGROUND_RETAIN_LIMIT;
                    output.drain(..excess);
                    let orphaned = output
                        .iter()
                        .take_while(|byte| (**byte & 0b1100_0000) == 0b1000_0000)
                        .count();
                    output.drain(..orphaned);
                }
                drop(output);
                *process
                    .total_bytes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) += count;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// Wait for the child so it never becomes a zombie, then record how it
/// ended once both pipes have drained.
fn reap_background(
    mut child: Child,
    process: Arc<BackgroundProcess>,
    stdout_thread: Option<std::thread::JoinHandle<()>>,
    stderr_thread: Option<std::thread::JoinHandle<()>>,
) {
    let status = child.wait();
    if let Some(t) = stdout_thread {
        let _ = t.join();
    }
    if let Some(t) = stderr_thread {
        let _ = t.join();
    }
    let outcome = match status {
        Ok(status) => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                match status.signal() {
                    Some(_) => ExitOutcome::Killed,
                    None => ExitOutcome::Exited(status.code().unwrap_or(-1)),
                }
            }
            #[cfg(not(unix))]
            {
                ExitOutcome::Exited(status.code().unwrap_or(-1))
            }
        }
        Err(_) => ExitOutcome::Exited(-1),
    };
    *process.exit.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
}

/// Check on, read more from, or kill a background process by handle.
fn query_background(id: &str, kill: bool) -> ToolOutput {
    let Some(process) = find_background(id) else {
        return failure(&format!("bash: no background process with handle {id}"));
    };
    if kill {
        kill_group(process.pid);
        // Give the reaper a brief window to observe the exit and record it.
        let deadline = Instant::now() + Duration::from_millis(500);
        while process
            .exit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let retained = process
        .output
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let total_bytes = *process
        .total_bytes
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let exit = *process.exit.lock().unwrap_or_else(|e| e.into_inner());
    let mut combined =
        super::resolve_carriage_returns(&super::strip_ansi(&String::from_utf8_lossy(&retained)))
            .trim_end()
            .to_string();
    if total_bytes > retained.len() {
        combined = format!(
            "… [truncated: {total_bytes} bytes total, showing the last {} — earlier output dropped]\n{combined}",
            retained.len()
        );
    }
    let (outcome, summary, status_line) = match exit {
        None => (
            ToolOutcome::Completed,
            format!("running (pid {})", process.pid),
            format!(
                "[still running — pid {}, command: {}]",
                process.pid, process.command
            ),
        ),
        Some(ExitOutcome::Exited(0)) => (
            ToolOutcome::Completed,
            "exited 0".to_string(),
            "[exited 0]".to_string(),
        ),
        Some(ExitOutcome::Exited(code)) => (
            ToolOutcome::Failed,
            format!("exited {code}"),
            format!("[exited {code}]"),
        ),
        Some(ExitOutcome::Killed) => (
            ToolOutcome::Cancelled,
            "killed".to_string(),
            "[killed]".to_string(),
        ),
    };
    if !combined.is_empty() {
        combined.push('\n');
    }
    combined.push_str(&status_line);
    ToolOutput {
        content: combined,
        outcome,
        summary,
        display: None,
    }
}

/// Kill a process group whose child is its group leader.
fn kill_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        // The child creates this process group in `pre_exec`. ESRCH simply
        // means every member has already exited; falling back to the positive
        // pid after wait risks signaling a newly reused pid.
        let _ = libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    if let Ok(mut child) = Command::new("kill").arg("-9").arg(pid.to_string()).spawn() {
        let _ = child.wait();
    }
}

/// Compatibility entry point for non-streaming callers.
pub fn run(args: &Value, cwd: &Path) -> ToolOutput {
    run_streaming(args, cwd, &AtomicBool::new(false), |_, _| {})
}

/// Run bash and publish decoded stdout/stderr chunks while the process lives.
pub fn run_streaming<F>(
    args: &Value,
    cwd: &Path,
    cancel: &AtomicBool,
    mut on_output: F,
) -> ToolOutput
where
    F: FnMut(OutputStream, &str),
{
    if let Some(handle) = args["handle"].as_str() {
        return query_background(handle, args["signal"].as_str() == Some("kill"));
    }
    let Some(command) = args["command"].as_str() else {
        return failure("bash: missing command (or a background `handle` to check)");
    };
    if args["background"].as_bool().unwrap_or(false) {
        return start_background(command, cwd);
    }
    let timeout = args["timeout"].as_u64().unwrap_or(120).clamp(1, 600);

    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => return failure(&format!("bash: {error}")),
    };

    let (tx, rx) = mpsc::channel::<(OutputStream, Vec<u8>)>();
    let reader_stop = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_reader(
            stdout,
            OutputStream::Stdout,
            tx.clone(),
            reader_stop.clone(),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_reader(
            stderr,
            OutputStream::Stderr,
            tx.clone(),
            reader_stop.clone(),
        ));
    }
    drop(tx);

    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut timed_out = false;
    let mut cancelled = false;
    let mut status = None;
    let mut retained = Vec::<u8>::new();
    let mut total_bytes = 0usize;
    // Per-stream carry for a UTF-8 code point split across pipe reads —
    // decoding each chunk alone turned split points into U+FFFD live.
    let mut carries = [Vec::<u8>::new(), Vec::<u8>::new()];

    while status.is_none() {
        while let Ok((stream, bytes)) = rx.try_recv() {
            retain_and_publish(
                &mut retained,
                &mut total_bytes,
                &mut carries[carry_index(stream)],
                stream,
                &bytes,
                &mut on_output,
            );
        }
        if cancel.load(Ordering::SeqCst) {
            cancelled = true;
            kill_group(child.id());
        } else if Instant::now() >= deadline {
            timed_out = true;
            kill_group(child.id());
        }
        match child.try_wait() {
            Ok(found) => status = found,
            Err(error) => return failure(&format!("bash: {error}")),
        }
        if status.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // A shell can exit while a background descendant still owns its pipe.
    // Background processes are outside this tool's contract, so close the
    // group on natural exit too. Give readers a brief chance to observe EOF,
    // then ask nonblocking readers to stop; never join a thread that is still
    // stuck behind a setsid'd descendant which escaped the group.
    kill_group(child.id());
    let drain_deadline = Instant::now() + Duration::from_millis(100);
    while readers.iter().any(|reader| !reader.is_finished()) && Instant::now() < drain_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    reader_stop.store(true, Ordering::SeqCst);
    let stop_deadline = Instant::now() + Duration::from_millis(100);
    while readers.iter().any(|reader| !reader.is_finished()) && Instant::now() < stop_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    for reader in readers {
        if reader.is_finished() {
            let _ = reader.join();
        }
    }
    while let Ok((stream, bytes)) = rx.try_recv() {
        retain_and_publish(
            &mut retained,
            &mut total_bytes,
            &mut carries[carry_index(stream)],
            stream,
            &bytes,
            &mut on_output,
        );
    }
    // The pipes are closed: whatever the carries still hold is genuinely
    // incomplete output, published lossily rather than dropped.
    for (index, stream) in [OutputStream::Stdout, OutputStream::Stderr]
        .into_iter()
        .enumerate()
    {
        if !carries[index].is_empty() {
            on_output(stream, &String::from_utf8_lossy(&carries[index]));
        }
    }

    let status = status.expect("loop stops only after child exit");
    // The model's copy: decoded, stripped of colour codes and progress-bar
    // rewrites — token noise it should never pay for.
    let mut combined =
        super::resolve_carriage_returns(&super::strip_ansi(&String::from_utf8_lossy(&retained)))
            .trim_end()
            .to_string();
    if total_bytes > retained.len() {
        // The marker leads: a reader of a truncated log needs to know it is
        // mid-stream before line one, not after 32KB.
        combined = format!(
            "… [truncated: {total_bytes} bytes total, showing the last {} — earlier output dropped]\n{combined}",
            retained.len()
        );
    }
    let (outcome, summary) = if cancelled {
        (ToolOutcome::Cancelled, "cancelled".to_string())
    } else if timed_out {
        (ToolOutcome::TimedOut, format!("timeout {timeout}s"))
    } else if status.success() {
        (ToolOutcome::Completed, "done".to_string())
    } else {
        (
            ToolOutcome::Failed,
            format!("exit {}", status.code().unwrap_or(-1)),
        )
    };
    if outcome == ToolOutcome::TimedOut {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&format!("… [killed: exceeded the {timeout}s timeout]"));
    } else if outcome == ToolOutcome::Cancelled {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("… [cancelled]");
    }

    ToolOutput {
        content: combined,
        outcome,
        summary,
        display: None,
    }
}

/// Drain one process pipe and tag every chunk before joining the shared queue.
#[cfg(unix)]
fn spawn_reader<R>(
    pipe: R,
    stream: OutputStream,
    tx: mpsc::Sender<(OutputStream, Vec<u8>)>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()>
where
    R: std::io::Read + AsRawFd + Send + 'static,
{
    unsafe {
        let fd = pipe.as_raw_fd();
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            let _ = libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    spawn_reader_loop(pipe, stream, tx, stop)
}

#[cfg(not(unix))]
fn spawn_reader<R>(
    pipe: R,
    stream: OutputStream,
    tx: mpsc::Sender<(OutputStream, Vec<u8>)>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()>
where
    R: std::io::Read + Send + 'static,
{
    spawn_reader_loop(pipe, stream, tx, stop)
}

fn spawn_reader_loop<R>(
    mut pipe: R,
    stream: OutputStream,
    tx: mpsc::Sender<(OutputStream, Vec<u8>)>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()>
where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if tx.send((stream, buffer[..count].to_vec())).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
        }
    })
}

fn carry_index(stream: OutputStream) -> usize {
    match stream {
        OutputStream::Stdout => 0,
        OutputStream::Stderr => 1,
    }
}

/// Retain a bounded suffix and publish every chunk so pipe draining never
/// depends on the model-output cap. The tail is what is kept: compilers and
/// test runners put the verdict at the end of a long log, so retaining the
/// head handed the model 32KB of passing output and dropped the failure.
/// Publishing goes through the stream's carry so only complete UTF-8 leaves;
/// a split code point waits for its remaining bytes instead of becoming a
/// replacement character.
fn retain_and_publish<F>(
    retained: &mut Vec<u8>,
    total_bytes: &mut usize,
    carry: &mut Vec<u8>,
    stream: OutputStream,
    bytes: &[u8],
    on_output: &mut F,
) where
    F: FnMut(OutputStream, &str),
{
    const RETAIN_LIMIT: usize = 32 * 1024;
    *total_bytes = total_bytes.saturating_add(bytes.len());
    retained.extend_from_slice(bytes);
    if retained.len() > RETAIN_LIMIT {
        let excess = retained.len() - RETAIN_LIMIT;
        retained.drain(..excess);
        // The raw byte cut may land inside a UTF-8 code point. Discard only
        // the orphaned continuation prefix so the retained tail starts at a
        // real boundary and lossy decoding doesn't invent a leading U+FFFD.
        let orphaned = retained
            .iter()
            .take_while(|byte| (**byte & 0b1100_0000) == 0b1000_0000)
            .count();
        retained.drain(..orphaned);
    }
    carry.extend_from_slice(bytes);
    let text = drain_complete_utf8(carry);
    if !text.is_empty() {
        on_output(stream, &text);
    }
}

/// Decode everything decodable, leaving at most an incomplete trailing
/// sequence in the buffer. Interior invalid bytes become U+FFFD — they are
/// genuinely bad, not split.
fn drain_complete_utf8(carry: &mut Vec<u8>) -> String {
    let mut out = String::new();
    loop {
        match std::str::from_utf8(carry) {
            Ok(text) => {
                out.push_str(text);
                carry.clear();
                return out;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(std::str::from_utf8(&carry[..valid]).expect("validated prefix"));
                match e.error_len() {
                    Some(bad) => {
                        out.push('\u{FFFD}');
                        carry.drain(..valid + bad);
                    }
                    None => {
                        // An incomplete sequence at the tail: keep it for
                        // the next chunk.
                        carry.drain(..valid);
                        return out;
                    }
                }
            }
        }
    }
}

fn failure(message: &str) -> ToolOutput {
    ToolOutput {
        content: message.into(),
        outcome: ToolOutcome::Failed,
        summary: "error".into(),
        display: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(exit: Option<ExitOutcome>) -> Arc<BackgroundProcess> {
        Arc::new(BackgroundProcess {
            pid: 0,
            command: "true".into(),
            output: Mutex::new(Vec::new()),
            total_bytes: Mutex::new(0),
            exit: Mutex::new(exit),
            registered_at: Instant::now(),
        })
    }

    /// This module is the only test touching the process-wide `BACKGROUND`
    /// static, so a fresh map at the start of the one test that uses it is
    /// safe rather than racing another test's handles.
    #[test]
    fn registering_past_the_cap_evicts_finished_handles_but_never_a_running_one() {
        *BACKGROUND.lock().unwrap_or_else(|e| e.into_inner()) = None;

        for i in 0..MAX_BACKGROUND_HANDLES {
            register_background(format!("finished-{i}"), fake(Some(ExitOutcome::Exited(0))));
        }
        register_background("still-running".into(), fake(None));
        // One more finished registration should evict the oldest finished
        // entry to make room, not the still-running one.
        register_background("finished-new".into(), fake(Some(ExitOutcome::Exited(0))));

        let guard = BACKGROUND.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.as_ref().expect("populated above");
        assert!(
            map.len() <= MAX_BACKGROUND_HANDLES + 1,
            "map grew unbounded: {} entries",
            map.len()
        );
        assert!(
            map.contains_key("still-running"),
            "a still-running handle must never be evicted"
        );
        assert!(
            !map.contains_key("finished-0"),
            "the oldest finished handle should have been evicted to make room"
        );
        assert!(
            map.contains_key("finished-new"),
            "the newly registered handle must be present"
        );
    }
}
