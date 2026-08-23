//! Sessions persist and resume: write a conversation, list it, load it back.

use std::sync::Mutex;

use e::core::agent::Agent;
use e::core::provider::catalog::{Api, Model};
use e::core::provider::ChatMessage;
use e::core::session::{self, Session};

// E_HOME is process-global, so tests that replace it must not overlap.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn session_round_trips_and_lists() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("e-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    let cwd = std::env::temp_dir().join("e-proj");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut s = Session::create(&cwd, "opencode-go/deepseek-v4-flash").unwrap();
    s.append(&ChatMessage::user("count the files here please"));
    s.append(&ChatMessage::assistant("There are three.", Vec::new()));
    s.append(&ChatMessage::tool_result_with_meta(
        "call-1",
        "line one\nline two",
        e::core::tools::ToolOutcome::Failed,
        "exit 7",
    ));
    let path = s.path().to_path_buf();
    drop(s);

    let loaded = Session::load(&path).unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[0].role, "user");
    assert_eq!(loaded[1].content, "There are three.");
    let meta = loaded[2]
        .tool_meta
        .as_ref()
        .expect("tool metadata persisted");
    assert_eq!(meta.outcome, e::core::tools::ToolOutcome::Failed);
    assert_eq!(meta.summary, "exit 7");

    let listed = session::list(&cwd);
    assert_eq!(listed.len(), 1);
    // Title = first line, eight words.
    assert_eq!(listed[0].title, "count the files here please");
    assert_eq!(listed[0].message_count, 3);

    assert_eq!(session::most_recent(&cwd), Some(path));
}

#[test]
fn opening_e_does_not_count_as_a_session() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "e-empty-session-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    let model = Model {
        provider: "test".into(),
        id: "model".into(),
        base_url: "http://127.0.0.1:1".into(),
        api: Api::Completions,
        efforts: Vec::new(),
        context_window: 1_000,
    };
    let (_agent, _events) = Agent::new(model);
    assert!(
        session::list(&std::env::current_dir().unwrap()).is_empty(),
        "constructing the app must not create a session"
    );

    let cwd = home.join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();

    let session = Session::create(&cwd, "test/model").unwrap();
    let path = session.path().to_path_buf();
    drop(session);
    assert!(path.exists(), "fixture should contain a header-only file");
    assert!(session::list(&cwd).is_empty());
    assert_eq!(session::most_recent(&cwd), None);

    let mut session = Session::reopen(&path).unwrap();
    session.append(&ChatMessage::assistant("unsolicited", Vec::new()));
    drop(session);
    assert!(session::list(&cwd).is_empty());

    let mut session = Session::reopen(&path).unwrap();
    session.append(&ChatMessage::user("now this is a session"));
    drop(session);
    let listed = session::list(&cwd);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "now this is a session");

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn old_tool_messages_without_metadata_still_load() {
    let old = r#"{"role":"tool","content":"ok","tool_call_id":"c1"}"#;
    let message: ChatMessage = serde_json::from_str(old).unwrap();
    assert_eq!(message.role, "tool");
    assert!(message.tool_meta.is_none());
}

#[test]
fn path_separator_and_hyphen_do_not_collide() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "e-session-collision-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    let root = home.join("workspaces");
    let first = root.join("alpha").join("beta-gamma");
    let second = root.join("alpha-beta").join("gamma");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();

    let mut saved = Session::create(&first, "test/model").unwrap();
    saved.append(&ChatMessage::user("first workspace only"));
    drop(saved);

    assert_eq!(session::list(&first).len(), 1);
    assert!(session::list(&second).is_empty());
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn legacy_session_directories_are_filtered_by_header_cwd() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "e-session-legacy-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    let root = home.join("workspaces");
    let first = root.join("alpha").join("beta-gamma");
    let second = root.join("alpha-beta").join("gamma");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let first = first.canonicalize().unwrap();
    let second = second.canonicalize().unwrap();
    let legacy = format!("-{}-", first.to_string_lossy().replace('/', "-"));
    assert_eq!(
        legacy,
        format!("-{}-", second.to_string_lossy().replace('/', "-"))
    );
    let dir = home.join("sessions").join(legacy);
    std::fs::create_dir_all(&dir).unwrap();

    for (name, cwd, message) in [
        ("first", &first, "belongs to first"),
        ("second", &second, "belongs to second"),
    ] {
        let contents = format!(
            "{}\n{}\n",
            serde_json::json!({
                "type": "session", "id": name, "cwd": cwd,
                "created": 1, "model": "test/model"
            }),
            serde_json::json!({
                "type": "message", "message": ChatMessage::user(message)
            })
        );
        std::fs::write(dir.join(format!("{name}.jsonl")), contents).unwrap();
    }

    let first_list = session::list(&first);
    assert_eq!(first_list.len(), 1);
    assert_eq!(first_list[0].title, "belongs to first");
    let second_list = session::list(&second);
    assert_eq!(second_list.len(), 1);
    assert_eq!(second_list[0].title, "belongs to second");
    let _ = std::fs::remove_dir_all(home);
}

// Linux filesystems accept arbitrary non-NUL filename bytes. macOS rejects
// this deliberately invalid UTF-8 test fixture with EILSEQ.
#[cfg(target_os = "linux")]
#[test]
fn session_keys_preserve_non_utf8_path_bytes() {
    use std::os::unix::ffi::OsStringExt;

    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "e-session-non-utf8-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    let cwd = home
        .join("workspaces")
        .join(std::ffi::OsString::from_vec(b"project-\xff".to_vec()));
    std::fs::create_dir_all(&cwd).unwrap();
    let mut saved = Session::create(&cwd, "test/model").unwrap();
    saved.append(&ChatMessage::user("non utf8 workspace"));
    drop(saved);

    assert_eq!(session::list(&cwd).len(), 1);
    let _ = std::fs::remove_dir_all(home);
}
