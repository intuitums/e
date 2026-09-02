//! Shared fixtures for the integration tests.
#![allow(dead_code)] // each test crate takes a subset of the helpers
//!
//! Each `tests/*.rs` crate `mod common;`s this directory. `E_HOME` is
//! process-global, so anything that writes it takes `env_lock()` first —
//! including across `#[tokio::test]` awaits (each test has its own runtime).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread::JoinHandle;

use e::core::providers::catalog::{Api, Model, Thinking};

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Hold this for the whole test whenever `E_HOME` or a provider `key_env`
/// must stay ours. A prior panic must not cascade as `PoisonError`.
pub fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Registry env keys leak a developer's real sign-in into signed-out
/// assertions. Clear every declared `key_env` for this process.
pub fn clear_env_keys() {
    for provider in e::core::providers::registry::all() {
        if let Some(env) = &provider.auth.key_env {
            std::env::remove_var(env);
        }
    }
}

/// A unique `E_HOME` that is removed when dropped.
pub struct Home {
    pub dir: PathBuf,
}

impl Home {
    pub fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "e-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("E_HOME", &dir);
        Home { dir }
    }

    pub fn write(&self, name: &str, contents: impl AsRef<[u8]>) {
        std::fs::write(self.dir.join(name), contents).unwrap();
    }

    pub fn auth(&self, json: &str) {
        self.write("auth.json", json);
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// HTTP response wrapping an SSE body.
pub fn sse_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

/// Accept one connection per response, capture each request, write the
/// canned reply. The join handle yields the captured requests in order.
pub fn serve_raw(responses: Vec<String>) -> (u16, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let mut captured = Vec::new();
        for response in responses {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 262144];
            let n = sock.read(&mut buf).unwrap();
            captured.push(String::from_utf8_lossy(&buf[..n]).into_owned());
            sock.write_all(response.as_bytes()).unwrap();
        }
        captured
    });
    (port, handle)
}

pub fn serve_sse(bodies: &[&str]) -> (u16, JoinHandle<Vec<String>>) {
    serve_raw(bodies.iter().copied().map(sse_response).collect())
}

pub fn test_model(provider: &str, port: u16, api: Api) -> Model {
    Model {
        provider: provider.into(),
        id: "test".into(),
        base_url: format!("http://127.0.0.1:{port}"),
        api,
        catalog: e::core::providers::registry::CatalogStrategy::Openai,
        responses_mount: e::core::providers::registry::ResponsesMount::Platform,
        provider_supports_tools: true,
        provider_image_input: false,
        effort: vec!["low".into(), "medium".into(), "high".into()],
        thinking: Thinking::Manual,
        context_window: 200_000,
        max_output: None,
        supports_tools: true,
        image_input: false,
        pricing: None,
    }
}

/// The OpenAI-shaped tool schema the dialects convert (or pass through).
pub fn read_tool() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "read",
            "description": "read a file",
            "parameters": {"type": "object", "properties": {}}
        }
    })
}

pub fn request_json(sent: &str) -> serde_json::Value {
    serde_json::from_str(sent.split("\r\n\r\n").nth(1).expect("request has a body"))
        .expect("request body is JSON")
}
