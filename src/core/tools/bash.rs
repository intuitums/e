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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
/// Finished jobs remain queryable briefly, but an autonomous session must not
/// retain every completed process forever.
const BACKGROUND_FINISHED_RETAIN: usize = 64;
const BACKGROUND_PROCESS_LIMIT: usize = 128;
static BACKGROUND_FINISHED_SEQUENCE: AtomicU64 = AtomicU64::new(1);

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
    finished_sequence: AtomicU64,
}

/// Live background processes, keyed by handle. Process-lifetime only — like
/// everything else in this tool, nothing here survives past the e process
/// itself; there is no daemon and no persistence to reload on restart.
static BACKGROUND: Mutex<Option<HashMap<String, Arc<BackgroundProcess>>>> = Mutex::new(None);

fn prune_background(map: &mut HashMap<String, Arc<BackgroundProcess>>) {
    let mut finished: Vec<(String, u64)> = map
        .iter()
        .filter_map(|(id, process)| {
            let sequence = process.finished_sequence.load(Ordering::Relaxed);
            (sequence > 0).then(|| (id.clone(), sequence))
        })
        .collect();
    finished.sort_by_key(|(_, sequence)| *sequence);
    let excess = finished.len().saturating_sub(BACKGROUND_FINISHED_RETAIN);
    for (id, _) in finished.into_iter().take(excess) {
        map.remove(&id);
    }
}

fn register_background(id: String, process: Arc<BackgroundProcess>) -> bool {
    let mut guard = BACKGROUND.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    prune_background(map);
    if map.len() >= BACKGROUND_PROCESS_LIMIT {
        return false;
    }
    map.insert(id, process);
    true
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
        finished_sequence: AtomicU64::new(0),
    });
    if !register_background(id.clone(), process.clone()) {
        kill_group(pid);
        let _ = child.wait();
        return failure("bash: background process limit reached; check or stop existing handles");
    }

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
    process.finished_sequence.store(
        BACKGROUND_FINISHED_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        Ordering::Relaxed,
    );
    if let Some(map) = BACKGROUND
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        prune_background(map);
    }
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

    // The loop only exits once try_wait() reported an exit; if that
    // invariant somehow broke, "killed" is the honest reading — the child's
    // fate is unknown — and never a panic.
    // The loop only exits once try_wait() reported an exit; if that
    // invariant somehow broke, an unknown failure is the honest reading —
    // never a panic.
    let exit_code = status.as_ref().and_then(|s| s.code());
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
    } else if exit_code == Some(0) {
        (ToolOutcome::Completed, "done".to_string())
    } else {
        (
            ToolOutcome::Failed,
            format!("exit {}", exit_code.unwrap_or(-1)),
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
                // valid_up_to() proves the prefix decodes; lossy is byte-
                // identical there and degrades instead of panicking if a
                // future refactor breaks that proof.
                out.push_str(&String::from_utf8_lossy(&carry[..valid]));
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

    fn finished(sequence: u64) -> Arc<BackgroundProcess> {
        Arc::new(BackgroundProcess {
            pid: 0,
            command: String::new(),
            output: Mutex::new(Vec::new()),
            total_bytes: Mutex::new(0),
            exit: Mutex::new(Some(ExitOutcome::Exited(0))),
            finished_sequence: AtomicU64::new(sequence),
        })
    }

    #[test]
    fn finished_background_records_are_bounded() {
        let mut processes = HashMap::new();
        for sequence in 1..=(BACKGROUND_FINISHED_RETAIN as u64 + 2) {
            processes.insert(sequence.to_string(), finished(sequence));
        }
        prune_background(&mut processes);
        assert_eq!(processes.len(), BACKGROUND_FINISHED_RETAIN);
        assert!(!processes.contains_key("1"));
        assert!(processes.contains_key(&(BACKGROUND_FINISHED_RETAIN as u64 + 2).to_string()));
    }
}
