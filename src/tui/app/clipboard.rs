//! Read image attachments from the desktop clipboard without a resident helper.

use crate::core::providers::ImageInput;

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
        set AppleScript's text item delimiters to linefeed
        return paths as text
    on error
        return ""
    end try
end run
"#;
    let output = std::process::Command::new("osascript")
        .args(["-e", SCRIPT])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let paths: Vec<String> = text
        .lines()
        .map(str::trim)
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

    let root = crate::core::config::home::home();
    std::fs::create_dir_all(&root).map_err(|error| format!("clipboard: {error}"))?;
    let id = uuid::Uuid::now_v7();
    let raw = root.join(format!(".clipboard-{id}.image"));
    let png = root.join(format!(".clipboard-{id}.png"));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&raw)
        .map_err(|error| format!("clipboard: {error}"))?;

    let result = (|| {
        let output = std::process::Command::new("osascript")
            .args(["-e", SCRIPT])
            .arg(&raw)
            .output()
            .map_err(|error| format!("clipboard: {error}"))?;
        if !output.status.success() {
            return Err("clipboard does not contain an image".into());
        }
        match ImageInput::from_path(&raw) {
            Ok(image) => Ok(image),
            Err(_) => {
                let converted = std::process::Command::new("sips")
                    .args(["-s", "format", "png"])
                    .arg(&raw)
                    .arg("--out")
                    .arg(&png)
                    .output()
                    .map_err(|error| format!("clipboard: {error}"))?;
                if !converted.status.success() {
                    return Err("clipboard image could not be converted to PNG".into());
                }
                ImageInput::from_path(&png)
            }
        }
    })();
    let _ = std::fs::remove_file(raw);
    let _ = std::fs::remove_file(png);
    result
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
        if let Some(bytes) = command_output(program, &args) {
            if let Ok(text) = String::from_utf8(bytes) {
                let paths = paths_from_uri_list(&text);
                if !paths.is_empty() {
                    if let Ok(images) = ImageInput::from_paths(&paths) {
                        return Ok(images);
                    }
                }
            }
        }
    }
    for mime in ["image/png", "image/jpeg", "image/webp", "image/gif"] {
        if let Some(bytes) = command_output("wl-paste", &["--no-newline", "--type", mime]) {
            if let Ok(image) = ImageInput::from_bytes(bytes) {
                return Ok(vec![image]);
            }
        }
        if let Some(bytes) = command_output("xclip", &["-selection", "clipboard", "-t", mime, "-o"])
        {
            if let Ok(image) = ImageInput::from_bytes(bytes) {
                return Ok(vec![image]);
            }
        }
    }
    Err("clipboard does not contain a supported image".into())
}

#[cfg(target_os = "linux")]
fn command_output(program: &str, args: &[&str]) -> Option<Vec<u8>> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    (output.status.success() && !output.stdout.is_empty()).then_some(output.stdout)
}

#[cfg(target_os = "linux")]
fn paths_from_uri_list(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.strip_prefix("file://"))
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
                "# copied files\nfile:///tmp/one%20shot.png\nfile:///tmp/two.jpg\n"
            ),
            ["/tmp/one shot.png", "/tmp/two.jpg"]
        );
    }
}
