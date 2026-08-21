//! Sessions persist and resume: write a conversation, list it, load it back.

use e::core::provider::ChatMessage;
use e::core::session::{self, Session};

#[test]
fn session_round_trips_and_lists() {
    let home = std::env::temp_dir().join(format!("e-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("E_HOME", &home);

    let cwd = std::env::temp_dir().join("e-proj");
    std::fs::create_dir_all(&cwd).unwrap();

    let mut s = Session::create(&cwd, "opencode-go/deepseek-v4-flash").unwrap();
    s.append(&ChatMessage::user("count the files here please"));
    s.append(&ChatMessage::assistant("There are three.", Vec::new()));
    let path = s.path().to_path_buf();
    drop(s);

    let loaded = Session::load(&path).unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].role, "user");
    assert_eq!(loaded[1].content, "There are three.");

    let listed = session::list(&cwd);
    assert_eq!(listed.len(), 1);
    // Title = first line, eight words.
    assert_eq!(listed[0].title, "count the files here please");
    assert_eq!(listed[0].message_count, 2);

    assert_eq!(session::most_recent(&cwd), Some(path));
}
