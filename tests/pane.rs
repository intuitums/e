//! The side pane end to end: an extension opens one over the real
//! protocol, e paints it beside the conversation on a real pty, Esc closes
//! it, and the terminal comes back from the alternate screen.

mod common;

use std::process::Command;

use common::{env_lock, Home};

/// An extension whose `/pane` command opens a two-section pane.
const PANE_EXTENSION: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\),"method".*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"id":%s,"result":{"name":"pane","version":"1","commands":[{"name":"pane","description":"open the pane"}]}}\n' "$id" ;;
    *'"method":"command"'*)
      printf '{"id":"p1","method":"ui.pane","params":{"id":"demo","title":"Demo pane","side":"right","sections":[{"kind":"list","id":"files","items":[{"id":"a","label":"alpha.rs","detail":"+1 -1"},{"id":"b","label":"beta.rs","detail":"new"}]},{"kind":"diff","id":"patch","body":"--- a/alpha.rs\\n+++ b/alpha.rs\\n@@ -1 +1 @@\\n-old line\\n+new line\\n"}]}}\n'
      printf '{"id":%s,"result":{}}\n' "$id" ;;
    *'"method":"shutdown"'*) exit 0 ;;
  esac
done
"#;

#[test]
fn a_pane_opens_beside_the_conversation_and_esc_restores_the_screen() {
    let _lock = env_lock();
    common::clear_env_keys();
    let home = Home::new("pty-pane");
    home.write(
        "models.json",
        r#"{"providers":{"mock":{"base_url":"http://127.0.0.1:1","catalog":"none","models":["audit"]}}}"#,
    );
    home.auth(r#"{"mock":{"key":"synthetic"}}"#);
    home.write("settings.json", r#"{"auto_update":"off"}"#);
    let extensions = home.dir.join("extensions");
    std::fs::create_dir_all(&extensions).unwrap();
    let script = extensions.join("pane.sh");
    std::fs::write(&script, PANE_EXTENSION).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let workspace = home.dir.join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let workspace = workspace.canonicalize().unwrap();
    home.write(
        "trust.json",
        serde_json::to_vec(&serde_json::json!({
            (workspace.to_str().unwrap()): {"trusted": true}
        }))
        .unwrap(),
    );

    let capture = home.dir.join("pane.raw");
    // Open the pane, then Esc twice: back to the first section, then close.
    let output = Command::new("python3")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ptycap.py"))
        .arg(&capture)
        .args(["120", "30", "4.0", "5"])
        .arg(env!("CARGO_BIN_EXE_e"))
        .args(["--no-save", "--no-tools", "--model", "mock/audit"])
        .current_dir(&workspace)
        .env("E_HOME", &home.dir)
        .env("CAP_PROMPT", "/pane")
        .env(
            "CAP_STEPS",
            r#"[[6.5,"\u001b[B"],[7.0,"\u001b"],[7.5,"\u001b"]]"#,
        )
        .env("CAP_EXIT", "\u{3}\u{3}")
        .env("CAP_EXIT_WAIT", "2")
        .env_remove("CAP_WAIT_FOR")
        .env_remove("CAP_RESIZE_AFTER")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = std::fs::read(&capture).unwrap();
    let text = String::from_utf8_lossy(&raw);
    // The pane painted on the alternate screen, beside the conversation.
    let plain = e::core::tools::strip_ansi(&text);
    let entered = text
        .find("\x1b[?1049h")
        .unwrap_or_else(|| panic!("the pane uses the alternate screen; saw:\n{plain}"));
    let painted = &text[entered..];
    // Content checks read past the colour codes the row grammar paints.
    let shown = e::core::tools::strip_ansi(painted);
    assert!(shown.contains("Demo pane"), "the pane title was painted");
    assert!(shown.contains("alpha.rs") && shown.contains("beta.rs"));
    assert!(
        shown.contains("- old line") && shown.contains("+ new line"),
        "{shown}"
    );
    assert!(
        shown.contains(" │ "),
        "the divider between conversation and pane"
    );
    assert!(shown.contains("Esc back"), "the pane's hint row");
    // Esc closed it and the main screen came back before exit.
    let left = painted
        .find("\x1b[?1049l")
        .expect("Esc leaves the alternate screen");
    assert!(
        painted[left..].contains("\x1b[?2004l"),
        "the session then exited normally"
    );
}
