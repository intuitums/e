//! Repo-local resources: a trusted directory's `.e/skills/` and
//! `.e/prompts/` load beside the global ones; an untrusted directory's stay
//! out; on a name clash the repo's own resource wins.

use std::sync::Mutex;

// E_HOME is process-global; serialize the tests that set it.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A temp e-home plus a temp repo, wired up for one test.
struct Fixtures {
    home: std::path::PathBuf,
    repo: std::path::PathBuf,
}

impl Drop for Fixtures {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
        let _ = std::fs::remove_dir_all(&self.repo);
    }
}

fn fixtures() -> Fixtures {
    let id = format!(
        "e-repo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    );
    let base = std::env::temp_dir().join(id);
    let home = base.join("home");
    let repo = base.join("repo");
    std::fs::create_dir_all(home.join("skills")).unwrap();
    std::fs::create_dir_all(home.join("prompts")).unwrap();
    std::fs::create_dir_all(repo.join(".e")).unwrap();
    std::env::set_var("E_HOME", &home);
    Fixtures { home, repo }
}

fn write_skill(dir: &std::path::Path, name: &str, description: &str) {
    let dir = dir.join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nbody of {name}"),
    )
    .unwrap();
}

fn write_prompt(dir: &std::path::Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir.join("prompts")).unwrap();
    std::fs::write(dir.join("prompts").join(format!("{name}.md")), body).unwrap();
}

#[test]
fn untrusted_repo_resources_stay_out() {
    let _guard = ENV_LOCK.lock().unwrap();
    let f = fixtures();
    write_skill(&f.home, "global", "the global one");
    write_prompt(&f.home, "greet", "hello global");
    write_skill(&repo_e(&f), "local", "the local one");
    write_prompt(&repo_e(&f), "bye", "goodbye local");

    let skills = e::core::resources::skills::list(&f.repo);
    assert_eq!(
        skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["global"]
    );
    let prompts = e::core::resources::prompts::list(&f.repo);
    assert_eq!(
        prompts.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["greet"]
    );
}

#[test]
fn trusted_repo_adds_its_own_and_shadows_on_name_clash() {
    let _guard = ENV_LOCK.lock().unwrap();
    let f = fixtures();
    write_skill(&f.home, "release", "global release");
    write_skill(&f.home, "other", "other");
    write_skill(&repo_e(&f), "release", "repo release");
    write_prompt(&f.home, "review", "review global");
    write_prompt(&repo_e(&f), "review", "review repo");
    write_prompt(&repo_e(&f), "ship", "ship repo");
    e::core::config::trust::set(&f.repo, true).unwrap();

    let skills = e::core::resources::skills::list(&f.repo);
    let names: Vec<_> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["other", "release"]);
    let release = skills.iter().find(|s| s.name == "release").unwrap();
    assert_eq!(release.description, "repo release");

    let prompts = e::core::resources::prompts::list(&f.repo);
    let names: Vec<_> = prompts.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["review", "ship"]);
    let review = e::core::resources::prompts::find("review", &f.repo).unwrap();
    assert_eq!(review.content, "review repo");

    // The skill tool resolves through the same merge.
    let out = e::core::tools::run("skill", r#"{"name":"release"}"#, &f.repo);
    assert!(!out.is_error());
    assert_eq!(out.summary, "release");
}

#[test]
fn catalog_reflects_the_merge() {
    let _guard = ENV_LOCK.lock().unwrap();
    let f = fixtures();
    write_skill(&repo_e(&f), "only-local", "described");
    e::core::config::trust::set(&f.repo, true).unwrap();

    let catalog = e::core::agent::context::system_prompt(&f.repo);
    assert!(catalog.contains("- only-local: described"));
}

/// The repo's resource root: `<repo>/.e`.
fn repo_e(f: &Fixtures) -> std::path::PathBuf {
    f.repo.join(".e")
}
