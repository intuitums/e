//! Self-update. Pins: version comparison, the checksum gate (a bad sum
//! refuses to install), and the full install path against a fake release —
//! download, verify, unpack, atomic swap.

use std::io::{Read, Write};
use std::net::TcpListener;

#[test]
fn version_comparison() {
    use e::core::update::is_newer;
    assert!(is_newer("v0.4.1", "0.4.0"));
    assert!(is_newer("1.0.0", "0.9.9"));
    assert!(!is_newer("v0.4.0", "0.4.0"));
    assert!(!is_newer("0.3.9", "0.4.0"));
    assert!(!is_newer("garbage", "0.4.0"));
    // A code-named (non-SemVer) build is never newer than a release, so it
    // never auto-updates over itself.
    assert!(!is_newer("v9.9.9", "dogfood"));
}

fn serve_release(files: Vec<(String, Vec<u8>)>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        for _ in 0..files.len() {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            let body = files
                .iter()
                .find(|(name, _)| path.ends_with(name.as_str()))
                .map(|(_, b)| b.clone())
                .unwrap_or_default();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.write_all(&body);
        }
    });
    (format!("http://127.0.0.1:{port}/releases"), handle)
}

fn fake_release(binary_contents: &str, poison_checksum: bool) -> (Vec<(String, Vec<u8>)>, String) {
    let target = e::core::update::target();
    let dir = std::env::temp_dir().join(format!(
        "e-update-fixture-{}-{}",
        std::process::id(),
        poison_checksum
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("e"), binary_contents).unwrap();
    let tar = dir.join("e.tar.gz");
    assert!(std::process::Command::new("tar")
        .arg("czf")
        .arg(&tar)
        .arg("-C")
        .arg(&dir)
        .arg("e")
        .status()
        .unwrap()
        .success());
    let tarball = std::fs::read(&tar).unwrap();
    let sum = {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&tarball);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let sum = if poison_checksum { "0".repeat(64) } else { sum };
    let name = format!("e-{target}.tar.gz");
    let files = vec![
        (name.clone(), tarball),
        (
            "checksums.txt".into(),
            format!("{sum}  {name}\n").into_bytes(),
        ),
    ];
    let _ = std::fs::remove_dir_all(&dir);
    (files, binary_contents.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn install_swaps_the_binary_atomically() {
    let (files, contents) = fake_release("#!/bin/sh\necho new-e\n", false);
    let (base, server) = serve_release(files);
    let dest_dir = std::env::temp_dir().join(format!("e-update-dest-{}", std::process::id()));
    std::fs::create_dir_all(&dest_dir).unwrap();
    let dest = dest_dir.join("e");
    std::fs::write(&dest, "old-binary").unwrap();

    let version = e::core::update::install_from(&base, "v9.9.9", &dest)
        .await
        .unwrap();
    assert_eq!(version, "9.9.9");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), contents);
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(&dest_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn poisoned_checksum_refuses_to_install() {
    let (files, _) = fake_release("evil\n", true);
    let (base, server) = serve_release(files);
    let dest_dir = std::env::temp_dir().join(format!("e-update-poison-{}", std::process::id()));
    std::fs::create_dir_all(&dest_dir).unwrap();
    let dest = dest_dir.join("e");
    std::fs::write(&dest, "old-binary").unwrap();

    let err = e::core::update::install_from(&base, "v9.9.9", &dest)
        .await
        .unwrap_err();
    assert!(err.contains("checksum mismatch"));
    // The running binary is untouched.
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old-binary");
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(&dest_dir);
}
