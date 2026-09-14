//! Packages: a git repository (or local directory) shaped like `~/.e/` —
//! `extensions/`, `skills/`, `prompts/`, `themes/` — installs under
//! `~/.e/packages/<host>/<path>`, is recorded in settings, and feeds every
//! loader after the home's own resources. Settings are the source of truth:
//! a deleted clone is reported at startup and restored by `e install`.
//!
//! These tests drive real `git` against a throwaway repository, the way a
//! user's install does.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{env_lock, serve_raw_bytes, Home};
use e::core::resources::packages::{self, Source, Status};

/// Package installs are async (release downloads); the git paths are
/// synchronous underneath, so a current-thread runtime is enough.
fn block<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// A committed package repository with one resource of every kind, tagged
/// `v1`, plus a second commit adding `prompts/more.md` on `main`.
struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(label: &str) -> Repo {
        let dir = std::env::temp_dir().join(format!(
            "e-pkg-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        write(
            &dir.join("skills/hello/SKILL.md"),
            "---\nname: hello\ndescription: from a package\n---\nbody\n",
        );
        write(
            &dir.join("prompts/hi.md"),
            "---\ndescription: say hi\n---\nhi $1\n",
        );
        write(
            &dir.join("themes/pkgtheme.json"),
            r#"{"name":"pkgtheme","vars":{},"colors":{}}"#,
        );
        write(
            &dir.join("extensions/pkgext.sh"),
            "#!/bin/sh\nwhile IFS= read -r line; do\n\
             id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9][0-9]*\\).*/\\1/p')\n\
             case \"$line\" in\n\
             *initialize*) printf '{\"id\":%s,\"result\":{\"name\":\"pkgext\",\"version\":\"1\",\"commands\":[{\"name\":\"pkg\",\"description\":\"from pkg\"}]}}\\n' \"$id\" ;;\n\
             *shutdown*) exit 0 ;;\n\
             esac\ndone\n",
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.join("extensions/pkgext.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let repo = Repo { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.commit("one of each");
        repo.git(&["tag", "v1"]);
        write(&repo.dir.join("prompts/more.md"), "more\n");
        repo.commit("more");
        repo
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .current_dir(&self.dir)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// The `git:file://…` source for this repository, optionally pinned.
    fn source(&self, rev: Option<&str>) -> String {
        let base = format!("git:file://{}", self.dir.display());
        match rev {
            Some(rev) => format!("{base}@{rev}"),
            None => base,
        }
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn settings_packages(home: &Home) -> Vec<String> {
    let text = std::fs::read_to_string(home.dir.join("settings.json")).unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    json["packages"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn install_clones_under_the_managed_root_and_every_loader_sees_the_package() {
    let _lock = env_lock();
    let home = Home::new("pkg-install");
    let repo = Repo::new("install");
    let cwd = std::env::temp_dir();

    let (root, counts) = block(packages::install(&repo.source(Some("v1")))).unwrap();
    assert!(root.starts_with(home.dir.join("packages").join("file")));
    assert_eq!(counts, [1, 1, 1, 1]);
    assert_eq!(settings_packages(&home), vec![repo.source(Some("v1"))]);
    assert!(
        !root.join("prompts/more.md").exists(),
        "pinned at v1, before the second commit"
    );

    let skills = e::core::resources::skills::list(&cwd);
    let hello = skills.iter().find(|s| s.name == "hello").unwrap();
    assert_eq!(hello.description, "from a package");
    assert!(hello.dir.starts_with(&root));
    assert!(packages::is_packaged(&hello.dir));

    let hi = e::core::resources::prompts::find("hi", &cwd).unwrap();
    assert_eq!(hi.content, "hi $1");

    assert!(e::core::config::settings::theme_names().contains(&"pkgtheme".to_string()));
    assert!(e::tui::theme::load_user("pkgtheme").is_some());

    let (notices, _rx) = tokio::sync::mpsc::channel(16);
    let host = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(e::core::extensions::ExtensionHost::start(notices, None));
    assert!(
        host.has_command("pkg"),
        "the package's extension is launched"
    );
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(host.shutdown());
}

#[test]
fn the_home_shadows_a_package_resource_of_the_same_name() {
    let _lock = env_lock();
    let home = Home::new("pkg-shadow");
    let repo = Repo::new("shadow");
    let cwd = std::env::temp_dir();
    block(packages::install(&repo.source(Some("v1")))).unwrap();
    write(
        &home.dir.join("skills/hello/SKILL.md"),
        "---\nname: hello\ndescription: from the home\n---\nbody\n",
    );
    write(&home.dir.join("prompts/hi.md"), "home hi\n");

    let skills = e::core::resources::skills::list(&cwd);
    let hellos: Vec<_> = skills.iter().filter(|s| s.name == "hello").collect();
    assert_eq!(hellos.len(), 1);
    assert_eq!(hellos[0].description, "from the home");
    assert_eq!(
        e::core::resources::prompts::find("hi", &cwd)
            .unwrap()
            .content,
        "home hi"
    );
}

#[test]
fn a_missing_clone_is_reported_at_startup_and_restored_by_install_all() {
    let _lock = env_lock();
    let home = Home::new("pkg-missing");
    let repo = Repo::new("missing");
    let (root, _) = block(packages::install(&repo.source(Some("v1")))).unwrap();
    std::fs::remove_dir_all(&root).unwrap();

    assert_eq!(packages::missing(), vec![repo.source(Some("v1"))]);
    assert!(
        packages::dirs("skills").is_empty(),
        "nothing loads from a missing clone"
    );
    let (notices, mut rx) = tokio::sync::mpsc::channel(16);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let host = runtime.block_on(e::core::extensions::ExtensionHost::start(notices, None));
    let notice = rx.try_recv().unwrap();
    assert!(
        notice.starts_with("package git:file://")
            && notice.ends_with("not installed — run `e install`"),
        "{notice}"
    );
    runtime.block_on(host.shutdown());

    // Startup never cloned; `e install` with no source does.
    assert!(!root.exists());
    let results = block(packages::install_all());
    assert_eq!(results.len(), 1);
    assert!(results[0].as_ref().unwrap().ends_with(": installed"));
    assert!(root.join("skills/hello/SKILL.md").is_file());
    assert!(matches!(
        packages::list()[0].status,
        Status::Installed { .. }
    ));
    drop(home);
}

#[test]
fn reinstalling_moves_the_pin_and_unpinning_follows_the_default_branch() {
    let _lock = env_lock();
    let home = Home::new("pkg-pin");
    let repo = Repo::new("pin");
    let (root, _) = block(packages::install(&repo.source(Some("v1")))).unwrap();
    assert!(!root.join("prompts/more.md").exists());

    // Same package, new ref: one entry, moved — not a duplicate.
    let (same_root, counts) = block(packages::install(&repo.source(None))).unwrap();
    assert_eq!(same_root, root);
    assert_eq!(settings_packages(&home), vec![repo.source(None)]);
    assert_eq!(counts[2], 2, "the tip of main carries both prompts");

    // Unpinned, `e install` fast-forwards to new commits.
    write(&repo.dir.join("prompts/third.md"), "third\n");
    repo.commit("third");
    let results = block(packages::install_all());
    assert!(results[0].as_ref().unwrap().ends_with(": up to date"));
    assert!(root.join("prompts/third.md").is_file());
}

#[test]
fn remove_deletes_a_managed_clone_but_leaves_a_local_directory_alone() {
    let _lock = env_lock();
    let home = Home::new("pkg-remove");
    let repo = Repo::new("remove");
    let (root, _) = block(packages::install(&repo.source(Some("v1")))).unwrap();
    // Identity ignores the ref: removing by the bare source finds the entry.
    packages::remove(&repo.source(None)).unwrap();
    assert!(!root.exists());
    assert!(settings_packages(&home).is_empty());
    assert!(
        std::fs::read_dir(home.dir.join("packages"))
            .unwrap()
            .next()
            .is_none(),
        "empty host and user directories are pruned"
    );
    assert!(
        packages::remove(&repo.source(None)).is_err(),
        "not installed twice"
    );

    // A relative local source is recorded as it resolved, so it loads from
    // any later working directory and can be removed by either spelling.
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.dir.parent().unwrap()).unwrap();
    let relative = format!("./{}", repo.dir.file_name().unwrap().to_string_lossy());
    block(packages::install(&relative)).unwrap();
    std::env::set_current_dir(&previous).unwrap();
    let recorded = settings_packages(&home);
    assert_eq!(recorded.len(), 1);
    assert!(
        std::path::Path::new(&recorded[0]).is_absolute(),
        "{recorded:?}"
    );
    assert_eq!(packages::roots(), vec![repo.dir.canonicalize().unwrap()]);
    packages::remove(&recorded[0]).unwrap();
    assert!(settings_packages(&home).is_empty());

    // A local path is referenced in place, so removal only forgets it.
    let local = repo.dir.to_string_lossy().into_owned();
    let (root, counts) = block(packages::install(&local)).unwrap();
    assert_eq!(root, repo.dir.canonicalize().unwrap());
    assert_eq!(counts, [1, 1, 2, 1]);
    packages::remove(&local).unwrap();
    assert!(repo.dir.join("skills/hello/SKILL.md").is_file());
    assert!(settings_packages(&home).is_empty());
}

#[test]
fn a_source_that_is_not_a_package_is_refused_before_anything_is_written() {
    let _lock = env_lock();
    let home = Home::new("pkg-refuse");
    assert!(block(packages::install("intuitums/e-diff")).is_err());
    assert!(block(packages::install("--upload-pack=touch")).is_err());
    assert!(block(packages::install("/definitely/not/a/directory")).is_err());
    assert!(!home.dir.join("settings.json").exists());
    assert!(!home.dir.join("packages").exists());
}

/// A release asset served locally: the gzip tarball the workflow publishes
/// plus a matching checksums.txt.
fn release_server(name: &str) -> (u16, std::thread::JoinHandle<Vec<String>>, String) {
    let target = e::core::update::target().expect("a release target for this machine");
    let dir = std::env::temp_dir().join(format!("e-release-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(name),
        "#!/bin/sh\nread -r line; printf '{\"id\":1000000,\"result\":{\"name\":\"tool\"}}\n'\n",
    )
    .unwrap();
    let status = std::process::Command::new("tar")
        .args(["czf", "asset.tar.gz", name])
        .current_dir(&dir)
        .status()
        .unwrap();
    assert!(status.success());
    let tar = std::fs::read(dir.join("asset.tar.gz")).unwrap();
    let sum = {
        use sha2::Digest;
        sha2::Sha256::digest(&tar)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let asset = format!("{name}-{target}.tar.gz");
    let http = |body: &[u8]| {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    };
    let sums = format!("{sum}  {asset}\n");
    let (port, server) = serve_raw_bytes(vec![http(&tar), http(sums.as_bytes())]);
    let _ = std::fs::remove_dir_all(&dir);
    (port, server, asset)
}

#[test]
fn a_release_package_installs_its_executable_under_extensions() {
    let _lock = env_lock();
    let home = Home::new("pkg-release");
    let (port, server, asset) = release_server("tool");
    let source = Source::parse("release:intuitums/e/tool@v9").unwrap();
    let base = format!("http://127.0.0.1:{port}");
    block(packages::install_release_from(&source, &base, &base)).unwrap();
    let requests = server.join().unwrap();
    assert!(requests[0].contains(&format!("GET /download/v9/{asset}")));
    assert!(requests[1].contains("GET /download/v9/checksums.txt"));

    let root = home.dir.join("packages/releases/intuitums/e/tool");
    assert_eq!(source.root(), root);
    let binary = root.join("extensions/tool");
    assert!(binary.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            std::fs::metadata(&binary).unwrap().permissions().mode() & 0o111,
            0
        );
    }
    assert_eq!(
        e::core::update::installed_release_tag(&root).as_deref(),
        Some("v9")
    );
    assert!(!root.join(".staging-v9").exists(), "staging is cleaned up");
    // Pinned at the installed tag: a second install touches no network.
    block(packages::install_release_from(
        &source,
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
    ))
    .unwrap();

    // The parse grammar and identity.
    assert_eq!(
        Source::parse("release:Intuitums/E/tool@v9")
            .unwrap()
            .identity(),
        Source::parse("release:intuitums/e/tool")
            .unwrap()
            .identity()
    );
    assert!(Source::parse("release:intuitums/e").is_err());
    assert!(Source::parse("release:intuitums/../e/tool").is_err());
    assert!(Source::parse("release:intuitums/e/tool@-x").is_err());
}

#[test]
fn a_trusted_repository_lists_its_own_packages_and_once_roots_are_forgotten() {
    let _lock = env_lock();
    let home = Home::new("pkg-project");
    let repo = Repo::new("project");
    let ws = home.dir.join("ws");
    std::fs::create_dir_all(ws.join(".e")).unwrap();
    std::fs::write(
        ws.join(".e/packages"),
        format!(
            "# team packages\n{}\n\n{}\n",
            repo.source(Some("v1")),
            repo.dir.display()
        ),
    )
    .unwrap();
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(&ws).unwrap();

    assert!(
        packages::configured().is_empty(),
        "untrusted: the list is ignored"
    );
    e::core::config::trust::set(&ws, true).unwrap();
    // The local directory line is not honoured: trust must not run code in
    // place. The git source is.
    let listed: Vec<String> = packages::configured()
        .into_iter()
        .map(|e| e.source)
        .collect();
    assert_eq!(listed, vec![repo.source(Some("v1"))]);
    assert_eq!(packages::missing(), vec![repo.source(Some("v1"))]);
    // `e install` installs the project's packages into the user's roots.
    let results = block(packages::install_all());
    assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
    assert_eq!(packages::roots().len(), 1);
    // Settings stay the user's: nothing from the project list was written.
    assert!(settings_packages(&home).is_empty());

    // `--package`: a local directory joins the roots in place; a clone is
    // temporary and removed by forget_once.
    let extra = home.dir.join("extra");
    std::fs::create_dir_all(extra.join("prompts")).unwrap();
    let root = block(packages::use_once(&extra.to_string_lossy())).unwrap();
    assert_eq!(root, extra.canonicalize().unwrap());
    let cloned = block(packages::use_once(&repo.source(None))).unwrap();
    assert!(cloned.join("skills/hello/SKILL.md").is_file());
    assert_eq!(packages::roots().len(), 3);
    packages::forget_once();
    assert!(!cloned.exists(), "the temporary clone is gone");
    assert!(extra.is_dir(), "the local directory is not ours to delete");
    assert_eq!(packages::roots().len(), 1);
    std::env::set_current_dir(previous).unwrap();
}

/// A registry for one package: the packument at `/<name>`, the tarball at
/// `/<name>/-/<name>-<version>.tgz`, served until the handle is dropped.
/// npm talks to it through `npm_config_registry`.
struct Registry {
    port: u16,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Registry {
    fn serve(name: &str, version: &str, tarball: Vec<u8>) -> Registry {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let tarball_path = format!(
            "/{name}/-/{}-{version}.tgz",
            name.rsplit('/').next().unwrap()
        );
        let packument = serde_json::json!({
            "name": name,
            "dist-tags": {"latest": version},
            "versions": {version: {
                "name": name, "version": version,
                "dist": {"tarball": format!("http://127.0.0.1:{port}{tarball_path}")}
            }}
        })
        .to_string();
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (seen, halt, name) = (requests.clone(), stop.clone(), name.to_string());
        std::thread::spawn(move || loop {
            if halt.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let Ok((mut sock, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(10));
                continue;
            };
            sock.set_nonblocking(false).unwrap();
            let mut buf = vec![0u8; 65536];
            let n = sock.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            seen.lock().unwrap().push(path.clone());
            let encoded = name.replace('/', "%2f");
            let (status, kind, body): (&str, &str, Vec<u8>) = if path == tarball_path {
                ("200 OK", "application/octet-stream", tarball.clone())
            } else if path == format!("/{name}") || path == format!("/{encoded}") {
                ("200 OK", "application/json", packument.clone().into_bytes())
            } else {
                (
                    "404 Not Found",
                    "application/json",
                    b"{\"error\":\"not found\"}".to_vec(),
                )
            };
            let _ = write!(
                sock,
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(&body);
        });
        Registry {
            port,
            requests,
            stop,
        }
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `package/…` tarball of a one-prompt, one-extension package, as `npm
/// publish` would produce it.
fn package_tarball(label: &str, version: &str) -> Vec<u8> {
    let stage = std::env::temp_dir().join(format!("e-npm-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    let root = stage.join("package");
    write(
        &root.join("package.json"),
        &format!(r#"{{"name":"e-npm-{label}","version":"{version}","keywords":["e-package"]}}"#),
    );
    write(
        &root.join("prompts/npmhi.md"),
        "---\ndescription: from npm\n---\nhi\n",
    );
    write(&root.join("extensions/npmext.sh"), "#!/bin/sh\nexit 0\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            root.join("extensions/npmext.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let out = stage.join("pkg.tgz");
    let status = Command::new("tar")
        // macOS tar would add `._` resource forks; none of those.
        .env("COPYFILE_DISABLE", "1")
        .args([
            "-czf",
            &out.to_string_lossy(),
            "-C",
            &stage.to_string_lossy(),
            "package",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    let bytes = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_dir_all(&stage);
    bytes
}

/// Requires `npm` on PATH; the registry is the test's own.
#[test]
fn an_npm_package_installs_without_scripts_updates_and_removes() {
    if Command::new("npm").arg("--version").output().is_err() {
        eprintln!("npm not available; skipping");
        return;
    }
    let _lock = env_lock();
    let home = Home::new("pkg-npm");
    let cache = home.dir.join("npm-cache");
    let registry = Registry::serve("e-npm-one", "1.0.0", package_tarball("one", "1.0.0"));
    std::env::set_var(
        "npm_config_registry",
        format!("http://127.0.0.1:{}/", registry.port),
    );
    std::env::set_var("npm_config_cache", &cache);

    let (root, counts) = block(packages::install("npm:e-npm-one")).unwrap();
    assert_eq!(
        root,
        packages::npm_prefix()
            .join("node_modules")
            .join("e-npm-one")
    );
    assert_eq!(
        counts,
        [1, 0, 1, 0],
        "the extension and the prompt are seen"
    );
    assert!(root.join("prompts/npmhi.md").is_file());
    assert_eq!(settings_packages(&home), vec!["npm:e-npm-one".to_string()]);
    assert!(
        packages::npm_prefix().join("package.json").is_file(),
        "the prefix is a project of e's own"
    );
    let prompts = e::core::resources::prompts::list(&home.dir);
    assert!(prompts.iter().any(|p| p.name == "npmhi"));

    // A newer version on the registry: `e install` brings it current.
    drop(registry);
    let registry = Registry::serve("e-npm-one", "1.1.0", package_tarball("one", "1.1.0"));
    std::env::set_var(
        "npm_config_registry",
        format!("http://127.0.0.1:{}/", registry.port),
    );
    let results = block(packages::install_all());
    assert_eq!(
        results,
        vec![Ok("npm:e-npm-one: updated 1.0.0 → 1.1.0".to_string())]
    );
    assert!(
        registry
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|p| !p.contains("audit")),
        "no audit calls: {:?}",
        registry.requests.lock().unwrap()
    );

    // A pinned spec moves the entry, not duplicates it; remove deletes the
    // install and the entry.
    block(packages::install("npm:e-npm-one@1.1.0")).unwrap();
    assert_eq!(
        settings_packages(&home),
        vec!["npm:e-npm-one@1.1.0".to_string()]
    );
    packages::remove("npm:e-npm-one").unwrap();
    assert!(!root.exists(), "npm uninstall removed it");
    assert!(settings_packages(&home).is_empty());
    std::env::remove_var("npm_config_registry");
    std::env::remove_var("npm_config_cache");
}

#[test]
fn a_filtered_entry_loads_only_what_it_names_and_survives_a_reinstall() {
    let _lock = env_lock();
    let home = Home::new("pkg-filter");
    let repo = Repo::new("filter");
    block(packages::install(&repo.source(Some("v1")))).unwrap();
    // Hand-edit settings the way a user would: an object entry with filters.
    let entry = serde_json::json!({
        "source": repo.source(Some("v1")),
        "prompts": ["!prompts/hi.md"],
        "skills": ["skills/nothing-*"],
    });
    e::core::config::settings::set_array("packages", vec![entry]).unwrap();
    let prompts = e::core::resources::prompts::list(&home.dir);
    assert!(!prompts.iter().any(|p| p.name == "hi"), "excluded prompt");
    let skills = e::core::resources::skills::list(&home.dir);
    assert!(!skills.iter().any(|s| s.name == "hello"), "not included");
    assert!(
        e::core::config::settings::theme_names().contains(&"pkgtheme".to_string()),
        "an unfiltered kind loads"
    );
    // Moving the pin keeps the filters.
    block(packages::install(&repo.source(None))).unwrap();
    let saved = e::core::config::settings::get_array("packages").unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0]["source"], repo.source(None));
    assert_eq!(saved[0]["prompts"], serde_json::json!(["!prompts/hi.md"]));
}

#[test]
fn init_starts_a_package_that_loads_in_place() {
    let _lock = env_lock();
    let home = Home::new("pkg-init");
    let dir = home.dir.join("my-pack");
    let written = packages::init(&dir).unwrap();
    assert!(written.iter().any(|p| p.ends_with("extensions/hello.mjs")));
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap()).unwrap();
    assert_eq!(manifest["name"], "my-pack");
    assert_eq!(manifest["keywords"], serde_json::json!(["e-package"]));
    assert_eq!(
        packages::counts(&dir)[0],
        2,
        "two executables in extensions/"
    );
    assert!(
        packages::init(&dir).is_err(),
        "never over an existing package"
    );
}

/// The released shape of the `packages` list: strings, and objects with a
/// `source` and per-kind filters, side by side. A future e keeps reading it.
#[test]
fn the_settings_fixture_with_filtered_entries_still_reads() {
    let _lock = env_lock();
    let home = Home::new("pkg-fixture");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/config/settings-v1-packages.json");
    std::fs::copy(fixture, home.dir.join("settings.json")).unwrap();
    let entries = packages::settings_entries();
    let sources: Vec<&str> = entries.iter().map(|e| e.source.as_str()).collect();
    assert_eq!(
        sources,
        [
            "npm:@fschrhunt1/e-diff",
            "git:github.com/fschrhunt/e-diff@v2",
            "npm:@team/e-tools@1.4.0",
            "/Users/me/src/local-pack"
        ]
    );
    assert!(entries[0].filter.is_empty());
    assert!(!entries[2].filter.allows("extensions", "legacy.mjs"));
    assert!(entries[2].filter.allows("extensions", "diff.mjs"));
    assert!(!entries[2].filter.allows("prompts", "other.md"));
    for entry in &entries {
        assert!(Source::parse(&entry.source).is_ok(), "{}", entry.source);
    }
}
