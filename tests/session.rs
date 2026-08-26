//! Sessions persist and resume: write a conversation, list it, load it back.

use std::sync::Mutex;

use e::core::agent::Agent;
use e::core::providers::catalog::{Api, Model};
use e::core::providers::ChatMessage;
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
    s.append(&ChatMessage::user("count the files here please"))
        .unwrap();
    s.append(&ChatMessage::assistant("There are three.", Vec::new()))
        .unwrap();
    s.append(&ChatMessage::tool_result_with_meta(
        "call-1",
        "line one\nline two",
        e::core::tools::ToolOutcome::Failed,
        "exit 7",
    ))
    .unwrap();
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
fn session_name_sets_reads_and_clears() {
    let _lock = ENV_LOCK.lock().unwrap();
    let model = Model {
        provider: "test".into(),
        id: "model".into(),
        base_url: "http://127.0.0.1:1".into(),
        api: Api::Completions,
        efforts: Vec::new(),
        thinking: e::core::providers::catalog::Thinking::Manual,
        context_window: 1_000,
        max_output: None,
    };
    let (agent, _events) = Agent::new(model);

    assert_eq!(agent.session_name(), None);
    agent.set_session_name("my-session".into());
    assert_eq!(agent.session_name().as_deref(), Some("my-session"));
    agent.clear_session_name();
    assert_eq!(agent.session_name(), None);
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
        thinking: e::core::providers::catalog::Thinking::Manual,
        context_window: 1_000,
        max_output: None,
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
    session
        .append(&ChatMessage::assistant("unsolicited", Vec::new()))
        .unwrap();
    drop(session);
    assert!(session::list(&cwd).is_empty());

    let mut session = Session::reopen(&path).unwrap();
    session
        .append(&ChatMessage::user("now this is a session"))
        .unwrap();
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
    saved
        .append(&ChatMessage::user("first workspace only"))
        .unwrap();
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
    saved
        .append(&ChatMessage::user("non utf8 workspace"))
        .unwrap();
    drop(saved);

    assert_eq!(session::list(&cwd).len(), 1);
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_session_open_in_one_place_cannot_be_appended_to_from_another() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "e-session-lock-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    let cwd = home.join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut owner = Session::create(&cwd, "test/model").unwrap();
    owner.append(&ChatMessage::user("owned here")).unwrap();
    let path = owner.path().to_path_buf();

    let second = Session::reopen(&path).err().unwrap();
    assert_eq!(second.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(
        second.to_string().contains("already active"),
        "the error must name the conflict: {second}"
    );

    // Releasing the first Session releases the lock.
    drop(owner);
    let mut resumed = Session::reopen(&path).unwrap();
    resumed.append(&ChatMessage::user("back in")).unwrap();
    drop(resumed);
    assert_eq!(Session::load(&path).unwrap().len(), 2);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_stale_lock_from_a_crashed_e_is_stolen_not_worshipped() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "e-session-stale-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    let cwd = home.join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();

    let s = Session::create(&cwd, "test/model").unwrap();
    let path = s.path().to_path_buf();
    let lock_path = path.with_extension("lock");
    assert!(lock_path.exists());

    // A crashed writer leaves its PID behind; that process is gone now if
    // we write one that cannot exist. The lock must yield.
    std::fs::write(&lock_path, b"4194304\n").unwrap();
    assert!(std::path::Path::new(&lock_path).exists());
    let _ = Session::reopen(&path).unwrap();

    // An empty or unparseable lock (crashed between create and PID write)
    // must not wedge the session shut either.
    std::fs::write(&lock_path, b"").unwrap();
    let _ = Session::reopen(&path).unwrap();

    drop(s);

    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn a_corrupted_record_is_surfaced_not_silently_dropped() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "e-session-corrupt-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    let cwd = home.join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut s = Session::create(&cwd, "test/model").unwrap();
    s.append(&ChatMessage::user("first")).unwrap();
    s.append(&ChatMessage::assistant("second", Vec::new()))
        .unwrap();
    let path = s.path().to_path_buf();
    drop(s);

    // Interleave two half-written records the way unlocked writers would.
    let raw = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = raw.lines().map(String::from).collect();
    // Interleave two half-written records the way unlocked writers would:
    // one shared line holding both JSON objects, then the orphaned tail.
    let record = std::mem::take(&mut lines[2]);
    let (head, tail) = record.split_at(20);
    lines[1].push_str(head);
    lines.insert(2, tail.to_string());
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let err = Session::load(&path).err().unwrap();
    assert!(
        err.to_string().contains("corrupt session record"),
        "load must report corruption: {err}"
    );
    // And the file no longer presents itself as a clean resumable session.
    assert!(session::list(&cwd).is_empty());

    let _ = std::fs::remove_dir_all(home);
}

/// A crash mid-append leaves a torn final line — the most common artifact an
/// append-only log ever shows. That must cost one record, not the session;
/// interior corruption (the test above) stays fatal.
#[test]
fn a_torn_final_line_costs_the_record_not_the_session() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "e-session-torn-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);
    let cwd = home.join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut s = Session::create(&cwd, "test/model").unwrap();
    s.append(&ChatMessage::user("first")).unwrap();
    s.append(&ChatMessage::assistant("second", Vec::new()))
        .unwrap();
    let path = s.path().to_path_buf();
    drop(s);

    // The crash: a record cut mid-write at the end of the file.
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("{\"type\":\"message\",\"message\":{\"role\":\"user\",\"con");
    std::fs::write(&path, raw).unwrap();

    let messages = Session::load(&path).unwrap();
    assert_eq!(messages.len(), 2, "the complete records survive");
    assert_eq!(messages[1].content, "second");

    let _ = std::fs::remove_dir_all(home);
}

/// A crash between a tool call and its result leaves a dangling tool_use
/// every dialect rejects on replay; load repairs the tail with an honest
/// synthetic result instead of handing the agent an unreplayable history.
#[test]
fn a_dangling_tool_call_is_repaired_on_load() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!(
        "e-session-dangling-{}-{}",
        std::process::id(),
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);
    let cwd = home.join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut s = Session::create(&cwd, "test/model").unwrap();
    s.append(&ChatMessage::user("task")).unwrap();
    s.append(&ChatMessage::assistant(
        "working",
        vec![e::core::providers::ToolCall {
            id: "call-1".into(),
            name: "bash".into(),
            arguments: "{}".into(),
            signature: None,
        }],
    ))
    .unwrap();
    let path = s.path().to_path_buf();
    drop(s);

    let messages = Session::load(&path).unwrap();
    let last = messages.last().unwrap();
    assert_eq!(last.role, "tool", "a synthetic result closes the batch");
    assert_eq!(last.tool_call_id.as_deref(), Some("call-1"));
    assert!(last.content.contains("not executed"));

    let _ = std::fs::remove_dir_all(home);
}

/// A session that cannot be created must say so — once per failure episode,
/// not per message, and not never.
#[tokio::test(flavor = "multi_thread")]
async fn persistence_failure_warns_once_not_silently() {
    let _lock = ENV_LOCK.lock().unwrap();
    // E_HOME pointing at a regular file makes every session create fail.
    let blocked = std::env::temp_dir().join(format!("e-blocked-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&blocked);
    let _ = std::fs::remove_file(&blocked);
    std::fs::write(&blocked, "not a directory").unwrap();
    std::env::set_var("E_HOME", &blocked);

    let model = Model {
        provider: "mock".into(),
        id: "m".into(),
        base_url: "http://127.0.0.1:1".into(),
        api: Api::Completions,
        efforts: Vec::new(),
        thinking: e::core::providers::catalog::Thinking::Manual,
        context_window: 200_000,
        max_output: None,
    };
    let (agent, mut rx) = Agent::new(model);
    agent.record_user("first".into());
    agent.record_user("second".into());

    let mut warnings = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, e::core::agent::SessionEvent::Warning(_)) {
            warnings += 1;
        }
    }
    assert_eq!(warnings, 1, "one warning per failure episode");
    // The in-memory conversation still advanced.
    assert_eq!(agent.history_snapshot().len(), 2);
    let _ = std::fs::remove_file(&blocked);
}

/// Every message becomes its own tree node, chained by id/parent onto
/// whatever was last written — a plain session with no rewind is a straight
/// line, exactly what `load` still sees.
#[test]
fn nodes_chain_linearly_when_nothing_rewound() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("e-tree-linear-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);
    let cwd = std::env::temp_dir().join("e-tree-linear-proj");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut s = Session::create(&cwd, "test/model").unwrap();
    s.append(&ChatMessage::user("first")).unwrap();
    s.append(&ChatMessage::assistant("reply", Vec::new()))
        .unwrap();
    let path = s.path().to_path_buf();
    drop(s);

    let nodes = Session::nodes(&path).unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(nodes[0].parent.is_none(), "the first message is a root");
    assert_eq!(nodes[1].parent.as_deref(), Some(nodes[0].id.as_str()));
    // Every id is distinct and non-empty.
    assert_ne!(nodes[0].id, nodes[1].id);
    assert!(!nodes[0].id.is_empty() && !nodes[1].id.is_empty());

    let _ = std::fs::remove_dir_all(&home);
}

/// `set_head` is the whole rewind mechanism: point the next append at an
/// earlier node and the file grows a second branch instead of extending the
/// abandoned tail — which survives untouched, still reachable through
/// `nodes`.
#[test]
fn set_head_grows_a_branch_without_touching_the_old_tail() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("e-tree-branch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);
    let cwd = std::env::temp_dir().join("e-tree-branch-proj");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut s = Session::create(&cwd, "test/model").unwrap();
    s.append(&ChatMessage::user("root")).unwrap();
    s.append(&ChatMessage::assistant("branch A reply", Vec::new()))
        .unwrap();
    let path = s.path().to_path_buf();

    let root_id = Session::nodes(&path).unwrap()[0].id.clone();
    s.set_head(Some(root_id.clone()));
    s.append(&ChatMessage::user("branch B")).unwrap();
    s.append(&ChatMessage::assistant("branch B reply", Vec::new()))
        .unwrap();
    drop(s);

    let nodes = Session::nodes(&path).unwrap();
    assert_eq!(nodes.len(), 4, "both branches persist in the one file");
    let children_of_root = nodes
        .iter()
        .filter(|n| n.parent.as_deref() == Some(root_id.as_str()))
        .count();
    assert_eq!(
        children_of_root, 2,
        "root now has two children — a branch point"
    );
    // Plain load() still walks the file top to bottom: both branches, in
    // append order, exactly as an append-only reader always saw it.
    let loaded = Session::load(&path).unwrap();
    assert_eq!(loaded.len(), 4);
    assert_eq!(loaded[2].content, "branch B");

    let _ = std::fs::remove_dir_all(&home);
}

/// Reopening a session must pick up the true tip of whatever branch was
/// active when it was last closed — not the file's root — so a resumed
/// session keeps growing that branch instead of starting a second root
/// beside it.
#[test]
fn reopen_continues_the_branch_that_was_active() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("e-tree-reopen-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);
    let cwd = std::env::temp_dir().join("e-tree-reopen-proj");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut s = Session::create(&cwd, "test/model").unwrap();
    s.append(&ChatMessage::user("root")).unwrap();
    let root_id = Session::nodes(s.path()).unwrap()[0].id.clone();
    s.set_head(Some(root_id.clone()));
    s.append(&ChatMessage::user("chosen branch")).unwrap();
    let path = s.path().to_path_buf();
    drop(s);

    let mut resumed = Session::reopen(&path).unwrap();
    resumed
        .append(&ChatMessage::assistant("continues here", Vec::new()))
        .unwrap();
    drop(resumed);

    let nodes = Session::nodes(&path).unwrap();
    let tail = nodes.last().unwrap();
    assert_eq!(tail.message.content, "continues here");
    let parent = nodes
        .iter()
        .find(|n| n.id == tail.parent.clone().unwrap())
        .unwrap();
    assert_eq!(
        parent.message.content, "chosen branch",
        "reopen must resume onto the branch that was active, not the root"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// A record written before branching existed carries no id or parent.
/// `nodes` must still produce a usable, linearly-chained tree for it so an
/// old session resumes onto its real tail instead of silently starting a
/// second root.
#[test]
fn legacy_records_synthesize_a_linear_chain() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("e-tree-legacy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);
    let cwd = std::env::temp_dir().join("e-tree-legacy-proj");
    std::fs::create_dir_all(&cwd).unwrap();

    // Hand-write a pre-branching-format log: no id/parent on the records.
    let s = Session::create(&cwd, "test/model").unwrap();
    let path = s.path().to_path_buf();
    drop(s);
    let legacy = format!(
        "{}\n{}\n{}\n",
        serde_json::json!({"type":"session","id":"x","cwd":cwd.to_string_lossy(),"created":1,"model":"test/model"}),
        serde_json::json!({"type":"message","message":ChatMessage::user("legacy first")}),
        serde_json::json!({"type":"message","message":ChatMessage::assistant("legacy reply", Vec::<e::core::providers::ToolCall>::new())}),
    );
    std::fs::write(&path, legacy).unwrap();

    let nodes = Session::nodes(&path).unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(nodes[0].parent.is_none());
    assert_eq!(nodes[1].parent.as_deref(), Some(nodes[0].id.as_str()));

    // A session reopened from a legacy tail keeps growing that same line.
    let mut resumed = Session::reopen(&path).unwrap();
    resumed
        .append(&ChatMessage::user("new turn after legacy tail"))
        .unwrap();
    drop(resumed);
    let nodes = Session::nodes(&path).unwrap();
    assert_eq!(nodes.len(), 3);
    assert_eq!(nodes[2].parent.as_deref(), Some(nodes[1].id.as_str()));

    let _ = std::fs::remove_dir_all(&home);
}

/// The persisted name is part of session identity: it must be readable back
/// for resume, and the latest entry wins.
#[test]
fn the_latest_persisted_name_is_readable_for_resume() {
    let _lock = ENV_LOCK.lock().unwrap();
    let home = std::env::temp_dir().join(format!("e-name-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);
    let cwd = std::env::temp_dir().join("e-name-proj");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut s = Session::create(&cwd, "mock/m").unwrap();
    s.append(&ChatMessage::user("hello")).unwrap();
    s.set_name("alpha").unwrap();
    s.set_name("beta").unwrap();
    let path = s.path().to_path_buf();
    drop(s);

    assert_eq!(session::name_of(&path).as_deref(), Some("beta"));
    let _ = std::fs::remove_dir_all(&home);
}
