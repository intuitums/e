//! `docs/` is the single source for three readers — GitHub, the website, and the
//! binary's `e docs` — so its shape is a contract: complete front matter the
//! website can parse, unique topic names, and relative links that resolve.
//! `docs/README.md` is the guide for whoever edits this folder.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn docs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("docs")
}

/// Every guide: `docs/<group>/*.md`, excluding each folder's README.md.
fn guides() -> Vec<PathBuf> {
    let mut found = Vec::new();
    for group in std::fs::read_dir(docs()).unwrap() {
        let group = group.unwrap().path();
        if !group.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(&group).unwrap() {
            let file = file.unwrap().path();
            if file.extension().is_some_and(|ext| ext == "md")
                && file.file_name().is_some_and(|name| name != "README.md")
            {
                found.push(file);
            }
        }
    }
    assert!(
        !found.is_empty(),
        "no guides found under {}",
        docs().display()
    );
    found
}

/// The front matter block, or None when the file does not open with one.
fn front_matter(text: &str) -> Option<Vec<(String, String)>> {
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut fields = Vec::new();
    for line in lines {
        if line.trim_end() == "---" {
            return Some(fields);
        }
        let (key, value) = line.split_once(':')?;
        fields.push((key.trim().to_string(), value.trim().to_string()));
    }
    None
}

#[test]
fn every_guide_has_complete_front_matter() {
    for path in guides() {
        let text = std::fs::read_to_string(&path).unwrap();
        let fields =
            front_matter(&text).unwrap_or_else(|| panic!("{} has no front matter", path.display()));
        for key in ["title", "description", "order"] {
            let value = fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
                .unwrap_or_else(|| panic!("{} is missing `{key}`", path.display()));
            assert!(!value.is_empty(), "{} has an empty `{key}`", path.display());
        }
        let order = fields
            .iter()
            .find(|(name, _)| name == "order")
            .map(|(_, value)| value.clone())
            .unwrap();
        assert!(
            order.parse::<u32>().is_ok(),
            "{} has a non-numeric order `{order}`",
            path.display()
        );
        let unknown: Vec<&String> = fields
            .iter()
            .map(|(name, _)| name)
            .filter(|name| !["title", "description", "order"].contains(&name.as_str()))
            .collect();
        assert!(
            unknown.is_empty(),
            "{} has unknown front matter keys {unknown:?}",
            path.display()
        );
    }
}

#[test]
fn every_group_declares_its_title_and_order() {
    let mut orders = BTreeSet::new();
    for entry in std::fs::read_dir(docs()).unwrap() {
        let group = entry.unwrap().path();
        if !group.is_dir() {
            continue;
        }
        let readme = group.join("README.md");
        let text = std::fs::read_to_string(&readme)
            .unwrap_or_else(|_| panic!("{} has no README.md", group.display()));
        let fields = front_matter(&text)
            .unwrap_or_else(|| panic!("{} has no front matter", readme.display()));
        for key in ["title", "description", "order"] {
            let value = fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
                .unwrap_or_else(|| panic!("{} is missing `{key}`", readme.display()));
            assert!(
                !value.is_empty(),
                "{} has an empty `{key}`",
                readme.display()
            );
        }
        let order: u32 = fields
            .iter()
            .find(|(name, _)| name == "order")
            .unwrap()
            .1
            .parse()
            .unwrap_or_else(|_| panic!("{} has a non-numeric order", readme.display()));
        assert!(orders.insert(order), "two groups share order {order}");
    }
    assert!(
        orders.len() >= 4,
        "expected the four groups, found {}",
        orders.len()
    );
}

#[test]
fn topics_are_unique_and_served_without_front_matter() {
    let mut stems = BTreeSet::new();
    for path in guides() {
        let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(stems.insert(stem.clone()), "two guides share `{stem}`");
        let body = e::core::resources::docs::body(&stem)
            .unwrap_or_else(|| panic!("e docs does not serve `{stem}`"));
        assert!(
            !body.starts_with("---") && !body.is_empty(),
            "`{stem}` reaches the terminal with front matter or empty"
        );
    }
    // The folder's own guide is for the repository, not a topic.
    assert!(e::core::resources::docs::body("README").is_none());
    assert!(e::core::resources::docs::body("readme").is_none());
}

#[test]
fn every_relative_link_resolves() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = walk(&docs());
    files.extend(walk(&manifest.join("contributing")));
    // A reader starts at the repository root, so its guides are checked too.
    for name in [
        "README.md",
        "AGENTS.md",
        "CLAUDE.md",
        "CONTRIBUTING.md",
        "SECURITY.md",
    ] {
        let path = manifest.join(name);
        if path.is_file() {
            files.push(path);
        }
    }

    let mut checked = 0;
    for path in &files {
        let text = std::fs::read_to_string(path).unwrap();
        for target in targets(&text) {
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
                || target.starts_with('#')
            {
                continue;
            }
            let file = target.split('#').next().unwrap();
            if file.is_empty() {
                continue;
            }
            let resolved = path.parent().unwrap().join(file);
            assert!(
                resolved.exists(),
                "{} links to `{target}`, which does not resolve",
                path.display()
            );
            checked += 1;
        }
    }
    // The walk must see the guides and the folder READMEs, or a broken link in
    // one of them would pass unnoticed.
    assert!(
        files.len() >= guides().len() + 2,
        "the walk missed files under docs/"
    );
    assert!(
        checked > 20,
        "only {checked} links checked; did the walk run?"
    );
}

/// Every markdown file under a folder, including each folder's README.md.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                found.push(path);
            }
        }
    }
    found
}

/// Inline markdown link and image targets.
fn targets(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("](") {
        rest = &rest[at + 2..];
        let Some(end) = rest.find(')') else { break };
        let target = rest[..end].trim();
        if !target.contains(' ') {
            found.push(target.to_string());
        }
        rest = &rest[end..];
    }
    found
}
