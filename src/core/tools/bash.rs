//! The bash tool: spawn a shell command and stream its captured pipes.
//!
//! This remains spawn-and-capture, not a terminal daemon. A wall-clock timeout
//! or turn cancellation kills the command's process group. Pipe readers feed
//! one tagged queue so display and retained output keep observed ordering.

use serde_json::{json, Value};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{schema_object, OutputStream, ToolOutcome, ToolOutput};

pub fn schema() -> Value {
    schema_object(
        "bash",
        "Run a shell command in the workspace root and return its combined output. Each call is a fresh shell: cd, environment variables, and background processes do not persist between calls. Output keeps the most recent 32KB when longer.",
        json!({
            "command": {"type": "string"},
            "timeout": {"type": "integer", "description": "Seconds before the command is killed (default 120)"}
        }),
        &["command"],
    )
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
    let Some(command) = args["command"].as_str() else {
        return failure("bash: missing command");
    };
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
