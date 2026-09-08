//! Read image attachments from the desktop clipboard without a resident helper.
//!
//! Every subprocess run is bounded twice — a wall-clock deadline and a
//! stdout cap — so a hung or flooding clipboard owner (or a PATH-replaced
//! helper) can neither stall a ctrl+v forever nor balloon memory: the run
//! is killed and the read surfaces as a notice.

use crate::core::providers::{ImageInput, MAX_IMAGE_BYTES};

/// One clipboard read may take this long before its helpers are killed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Bounded helpers are looked up at their system paths, not via `PATH`.
#[cfg(target_os = "macos")]
const OSASCRIPT: &str = "/usr/bin/osascript";
#[cfg(target_os = "macos")]
const SIPS: &str = "/usr/bin/sips";

/// Run a helper, bounded. `Ok` carries the exit status and the capped
/// stdout; `Err` is a spawn failure (`Missing` — try the next helper) or a
/// timeout / over-cap read (give up, the payload is unusable either way).
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) enum RunError {
    Missing,
    Failed(String),
}

struct RunOutput {
    success: bool,
    stdout: Vec<u8>,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run(program: &str, args: &[&str]) -> Result<RunOutput, RunError> {
    use std::io::Read as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // Its own process group, so the kill below reaches any forked
        // descendant still holding the pipe (the bash tool's pattern).
        .process_group(0)
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                RunError::Missing
            } else {
                RunError::Failed(format!("{program}: {error}"))
            }
        })?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(RunError::Failed(format!("{program}: no stdout pipe")));
    };

    // Read (bounded) on a helper thread so the deadline can fire even when
    // the child never closes its pipe; the pipe dies with the kill below.
    // A second channel carries the reader's exit, so a pathological process
    // that escaped the group and still holds the pipe can only ever cost a
    // bounded wait — never a hang, and never a bricked clipboard.
    let (sender, receiver) = std::sync::mpsc::channel();
    let (done_sender, done_receiver) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    buffer.extend_from_slice(&chunk[..read]);
                    if buffer.len() as u64 > MAX_IMAGE_BYTES {
                        break;
                    }
                }
            }
        }
        let _ = sender.send(buffer);
        let _ = done_sender.send(());
    });

    let stdout = match receiver.recv_timeout(READ_TIMEOUT) {
        Ok(buffer) => buffer,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            crate::core::tools::kill_group(child.id());
            let _ = child.wait();
            await_reader(&done_receiver);
            return Err(RunError::Failed(format!(
                "{program} did not finish within {}s",
                READ_TIMEOUT.as_secs()
            )));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Vec::new(),
    };
    if stdout.len() as u64 > MAX_IMAGE_BYTES {
        // The reader stopped draining, so the child may be blocked on a
        // full pipe and never exit on its own — kill the group first.
        crate::core::tools::kill_group(child.id());
        let _ = child.wait();
        let _ = reader.join();
        return Err(RunError::Failed(format!(
            "{program} output exceeded the {} MiB image limit",
            MAX_IMAGE_BYTES / (1024 * 1024)
        )));
    }
    // Give the helper a bounded chance to exit on its own; kill the whole
    // group while the child is unreaped either way — its pid is still the
    // valid group id then, and cannot yet have been reused.
    let deadline = std::time::Instant::now() + READ_TIMEOUT;
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                crate::core::tools::kill_group(child.id());
                let _ = child.wait();
                await_reader(&done_receiver);
                return Err(RunError::Failed(format!(
                    "{program} did not finish within {}s",
                    READ_TIMEOUT.as_secs()
                )));
            }
        }
    };
    // The leader has exited, but a forked descendant may still hold the
    // pipe and block the reader — take the group down before waiting for
    // the reader's exit.
    crate::core::tools::kill_group(child.id());
    let _ = child.wait();
    await_reader(&done_receiver);
    let _ = reader.join();
    Ok(RunOutput { success, stdout })
}

/// Wait briefly for the reader thread to finish. A process that escaped
/// the group and still holds the pipe would block a plain join forever;
/// after the bounded wait the thread is left to die whenever the pipe
/// finally closes, and the read reports its result regardless.
fn await_reader(done: &std::sync::mpsc::Receiver<()>) {
    let _ = done.recv_timeout(std::time::Duration::from_secs(2));
}

/// Read one clipboard payload. File-copy clipboards may contain several image
/// paths; bitmap clipboards produce one encoded image.
pub(super) fn images() -> Result<Vec<ImageInput>, String> {
    platform_images()
}

#[cfg(target_os = "macos")]
fn platform_images() -> Result<Vec<ImageInput>, String> {
    if let Some(images) = macos_file_images() {
        return Ok(images);
    }
    macos_bitmap().map(|image| vec![image])
}

#[cfg(target_os = "macos")]
fn macos_file_images() -> Option<Vec<ImageInput>> {
    const SCRIPT: &str = r#"
on run
    try
        set clipboardItems to the clipboard as list
        set paths to {}
        repeat with clipboardItem in clipboardItems
            set end of paths to POSIX path of clipboardItem
        end repeat
        set AppleScript's text item delimiters to (character id 0)
        return paths as text
    on error
        return ""
    end try
end run
"#;
    let output = run(OSASCRIPT, &["-e", SCRIPT]).ok()?;
    if !output.success {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    // NUL-delimited — a NUL cannot occur in a POSIX path, so a filename
    // containing a newline survives intact. Trim nothing: a path is data.
    let paths: Vec<String> = text
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(String::from)
        .collect();
    if paths.is_empty() {
        return None;
    }
    ImageInput::from_paths(&paths).ok()
}

#[cfg(target_os = "macos")]
fn macos_bitmap() -> Result<ImageInput, String> {
    use std::os::unix::fs::OpenOptionsExt as _;

    const SCRIPT: &str = r#"
on run argv
    set outputPath to item 1 of argv
    try
        set imageData to the clipboard as «class PNGf»
    on error
        try
            set imageData to the clipboard as TIFF picture
        on error
            error "clipboard does not contain an image"
        end try
    end try
    set outputFile to open for access POSIX file outputPath with write permission
    try
        set eof outputFile to 0
        write imageData to outputFile
        close access outputFile
    on error messageText
        try
            close access outputFile
        end try
        error messageText
    end try
end run
"#;

    // The system temp dir, not the e home: the export is transient and is
    // removed below, and osascript's write itself cannot be size-capped —
    // the bound is enforced when the file is read back.
    let dir = std::env::temp_dir();
    let id = uuid::Uuid::now_v7();
    let raw = dir.join(format!(".e-clipboard-{id}.image"));
    let png = dir.join(format!(".e-clipboard-{id}.png"));
    let (Some(raw), Some(png)) = (raw.to_str(), png.to_str()) else {
        return Err("clipboard: temp path is not valid unicode".into());
    };
    let raw_path = std::path::PathBuf::from(raw);
    let png_path = std::path::PathBuf::from(png);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&raw_path)
        .map_err(|error| format!("clipboard: {error}"))?;

    let result = (|| {
        let output = run(OSASCRIPT, &["-e", SCRIPT, raw]).map_err(unusable)?;
        if !output.success {
            return Err("clipboard does not contain an image".into());
        }
        match ImageInput::from_path(&raw_path) {
            Ok(image) => Ok(image),
            Err(_) => {
                let converted =
                    run(SIPS, &["-s", "format", "png", raw, "--out", png]).map_err(unusable)?;
                if !converted.success {
                    return Err("clipboard image could not be converted to PNG".into());
                }
                ImageInput::from_path(&png_path)
            }
        }
    })();
    let _ = std::fs::remove_file(&raw_path);
    let _ = std::fs::remove_file(&png_path);
    result
}

#[cfg(target_os = "macos")]
fn unusable(error: RunError) -> String {
    match error {
        RunError::Missing => "osascript is not available".into(),
        RunError::Failed(message) => message,
    }
}

#[cfg(target_os = "linux")]
fn platform_images() -> Result<Vec<ImageInput>, String> {
    for (program, args) in [
        ("wl-paste", vec!["--no-newline", "--type", "text/uri-list"]),
        (
            "xclip",
            vec!["-selection", "clipboard", "-t", "text/uri-list", "-o"],
        ),
    ] {
        match command_output(program, &args) {
            Ok(Some(bytes)) => {
                if let Ok(text) = String::from_utf8(bytes) {
                    let paths = paths_from_uri_list(&text);
                    if !paths.is_empty() {
                        if let Ok(images) = ImageInput::from_paths(&paths) {
                            return Ok(images);
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(error) => return Err(error),
        }
    }
    for mime in ["image/png", "image/jpeg", "image/webp", "image/gif"] {
        for program in ["wl-paste", "xclip"] {
            let args: Vec<&str> = if program == "wl-paste" {
                vec!["--no-newline", "--type", mime]
            } else {
                vec!["-selection", "clipboard", "-t", mime, "-o"]
            };
            match command_output(program, &args) {
                Ok(Some(bytes)) => {
                    if let Ok(image) = ImageInput::from_bytes(bytes) {
                        return Ok(vec![image]);
                    }
                }
                Ok(None) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Err("clipboard does not contain a supported image".into())
}

/// `Ok(None)`: the helper is absent or reported nothing — try the next one.
/// `Err`: the read failed for a reason worth reporting (timeout, over-cap).
#[cfg(target_os = "linux")]
fn command_output(program: &str, args: &[&str]) -> Result<Option<Vec<u8>>, String> {
    match run(program, args) {
        Ok(output) if output.success && !output.stdout.is_empty() => Ok(Some(output.stdout)),
        Ok(_) => Ok(None),
        Err(RunError::Missing) => Ok(None),
        Err(RunError::Failed(message)) => Err(message),
    }
}

#[cfg(target_os = "linux")]
fn paths_from_uri_list(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let rest = line.strip_prefix("file://")?;
            // `file://localhost/…` means the local host; an empty authority
            // is already the leading slash. Remote authorities stay put.
            let path = match rest.strip_prefix("localhost/") {
                Some(local) => local,
                None => rest,
            };
            (!path.is_empty()).then_some(path)
        })
        .filter_map(percent_decode)
        .collect()
}

#[cfg(target_os = "linux")]
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let encoded = bytes.get(index + 1..index + 3)?;
            let text = std::str::from_utf8(encoded).ok()?;
            out.push(u8::from_str_radix(text, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_images() -> Result<Vec<ImageInput>, String> {
    Err("clipboard images are not supported on this platform".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn uri_lists_decode_multiple_file_paths() {
        assert_eq!(
            super::paths_from_uri_list(
                "# copied files\nfile:///tmp/one%20shot.png\nfile://localhost/tmp/two.jpg\n"
            ),
            ["/tmp/one shot.png", "/tmp/two.jpg"]
        );
    }

    #[test]
    fn a_missing_helper_tries_the_next_one() {
        assert_eq!(
            super::command_output("e-definitely-not-installed", &[]),
            Ok(None)
        );
    }
}
