//! Agent personas: trust-scoped discovery, and a persona applied end to end
//! through `e rpc`.

use std::process::{Command, Stdio};

mod common;

use common::{env_lock, request_json, serve_sse, Home};
use std::io::Write as _;

/// Global personas always load; a repo's own `.e/agents/` only when the
/// directory is trusted, and a repo persona shadows a global one by name.
#[test]
fn discovery_is_trust_scoped_and_project_shadows_global() {
    let _lock = env_lock();
    let home = Home::new("agents-scope");
    std::fs::create_dir_all(home.dir.join("agents")).unwrap();
    std::fs::write(
        home.dir.join("agents").join("scout.md"),
        "---\ndescription: global scout\n---\nglobal body\n",
    )
    .unwrap();

    let project = home.dir.join("project");
    std::fs::create_dir_all(project.join(".e").join("agents")).unwrap();
    std::fs::write(
        project.join(".e").join("agents").join("scout.md"),
        "---\ndescription: project scout\n---\nproject body\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".e").join("agents").join("local.md"),
        "---\ndescription: project only\n---\nlocal body\n",
    )
    .unwrap();

    // Untrusted: only the global persona is visible.
    let untrusted = e::core::resources::agents::list(&project);
    assert_eq!(untrusted.len(), 1);
    assert_eq!(untrusted[0].description, "global scout");

    // Trusted: the project's own personas load, and its `scout` shadows the
    // global one of the same name.
    e::core::config::trust::set(&project, true).unwrap();
    let trusted = e::core::resources::agents::list(&project);
    let scout = trusted.iter().find(|a| a.name == "scout").unwrap();
    assert_eq!(scout.description, "project scout");
    assert!(trusted.iter().any(|a| a.name == "local"));
    assert_eq!(trusted.len(), 2);
}

/// A named persona's system prompt reaches the provider request: the delegated
/// turn is steered by the persona body, not the default base instructions.
#[test]
fn a_named_persona_shapes_the_delegated_turn() {
    let _lock = env_lock();
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
                data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n\
                data: [DONE]\n\n";
    let (port, server) = serve_sse(&[body]);
    let home = Home::new("agents-rpc");
    std::fs::create_dir_all(home.dir.join("agents")).unwrap();
    home.write(
        "agents/scout.md",
        "---\ndescription: recon\ntools: read, grep\n---\nYou are the scout persona for this test.\n",
    );
    home.write(
        "models.json",
        format!(
            r#"{{"providers":{{"mock":{{"base_url":"http://127.0.0.1:{port}","api":"completions","models":["test"]}}}}}}"#
        ),
    );
    home.auth(r#"{"format_version":1,"mock":{"key":"k"}}"#);

    let mut child = Command::new(env!("CARGO_BIN_EXE_e"))
        .args(["--no-extensions", "rpc"])
        .env("E_HOME", &home.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"id\":1,\"prompt\":\"go\",\"agent\":\"scout\"}\n")
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let captured = server.join().unwrap().remove(0);
    let request = request_json(&captured);
    let system = request["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["role"] == "system"))
        .and_then(|m| m["content"].as_str())
        .unwrap_or_default();
    assert!(
        system.contains("You are the scout persona for this test."),
        "system prompt missing the persona body: {system}"
    );
}
