//! Packages: shareable bundles of extensions, skills, prompt templates, and
//! themes.
//!
//! A package is an npm package, a git repository, or a local directory laid
//! out like `~/.e/` itself — `extensions/`, `skills/`, `prompts/`, `themes/`,
//! any subset, no manifest of e's own. `e install <source>` fetches it under
//! `~/.e/packages/` (`npm/node_modules/<name>` for npm, `<host>/<path>` for
//! git) and records the source in the `packages` list of `settings.json`;
//! every loader then reads each package's directory after `~/.e/`'s own, so
//! a resource in the home shadows a package's, and a trusted repo's `.e/`
//! shadows both. A settings entry may carry per-kind glob filters
//! ([`Filter`]) that leave part of a package unloaded.
//!
//! Settings are the source of truth, not the directory: delete an install
//! and `e install` with no arguments puts it back. Startup never touches the
//! network — a listed package missing on disk is reported in the transcript.
//! git and npm run as subprocesses, always with lifecycle scripts off, so
//! installing needs them on `PATH` and speaks whatever registries and
//! credentials the user's own do.
//!
//! A release package (`release:<owner>/<repo>/<name>[@tag]`) is a compiled
//! extension published as a GitHub release asset, `<name>-<target>.tar.gz`
//! beside a `checksums.txt` — how e's own `packages/` crates reach users.
//! It installs under `~/.e/packages/releases/<owner>/<repo>/<name>` with the
//! executable in `extensions/`, the same shape as every other package.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::config::{home, settings};

/// The resource directories a package may carry, in display order.
pub const KINDS: [&str; 4] = ["extensions", "skills", "prompts", "themes"];

const SETTINGS_KEY: &str = "packages";

/// A parsed package source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An npm package, installed under `~/.e/packages/npm/node_modules/<name>`.
    Npm {
        /// The package name, `@scope/name` included.
        name: String,
        /// A version, range, or dist-tag to pin; `None` follows `latest`.
        version: Option<String>,
    },
    /// A git remote, cloned under `~/.e/packages/<host>/<path>`.
    Git {
        /// The URL handed to `git clone`, ref stripped.
        url: String,
        /// Lowercased host, the first directory under the packages root.
        host: String,
        /// The repository path on that host, `.git` stripped.
        path: String,
        /// A tag, branch, or commit to pin; `None` follows the default branch.
        rev: Option<String>,
    },
    /// A directory on this machine, loaded in place — never copied.
    Local(PathBuf),
    /// A compiled extension from a GitHub release.
    Release {
        owner: String,
        repo: String,
        name: String,
        /// A release tag to pin; `None` follows the latest release.
        tag: Option<String>,
    },
}

impl Source {
    /// Parse a source string, the grammar `e install` accepts:
    ///
    /// - `npm:name[@version]`, `npm:@scope/name[@version]` — from the
    ///   user's npm registry
    /// - `git:host/user/repo[@ref]` — shorthand, cloned over HTTPS
    /// - `git:git@host:user/repo[@ref]` — scp-style SSH
    /// - `https://…`, `ssh://…`, `git://…`, `file://…` — any git URL, with
    ///   or without the `git:` prefix
    /// - `/abs/path`, `./rel`, `../rel`, `~/path` — a local directory
    pub fn parse(spec: &str) -> Result<Source, String> {
        let spec = spec.trim();
        if spec.is_empty() {
            return Err("empty package source".into());
        }
        if spec.starts_with('-') {
            return Err(format!("`{spec}` is not a package source"));
        }
        if let Some(rest) = spec.strip_prefix("npm:") {
            return parse_npm(rest, spec);
        }
        if let Some(rest) = spec.strip_prefix("git:") {
            return parse_git(rest, spec);
        }
        if let Some(rest) = spec.strip_prefix("release:") {
            return parse_release(rest, spec);
        }
        if spec.contains("://") {
            return parse_git(spec, spec);
        }
        let local = spec.starts_with('/')
            || spec.starts_with("./")
            || spec.starts_with("../")
            || spec == "."
            || spec == ".."
            || spec.starts_with("~/");
        if local {
            return Ok(Source::Local(expand_local(spec)));
        }
        Err(format!(
            "`{spec}` is not a package source — use npm:<name>[@version], git:<host>/<user>/<repo>[@ref], a git URL, or a directory path"
        ))
    }

    /// Where the package's files live: the managed clone, or the local
    /// directory itself.
    pub fn root(&self) -> PathBuf {
        match self {
            Source::Npm { name, .. } => npm_prefix().join("node_modules").join(name),
            Source::Git { host, path, .. } => home::packages_dir().join(host).join(path),
            Source::Local(path) => path.clone(),
            Source::Release {
                owner, repo, name, ..
            } => home::packages_dir()
                .join("releases")
                .join(owner)
                .join(repo)
                .join(name),
        }
    }

    /// Two sources name the same package when they differ only in scheme,
    /// credentials, `.git`, case of the host, or the pinned ref.
    pub fn identity(&self) -> String {
        match self {
            Source::Npm { name, .. } => format!("npm:{}", name.to_lowercase()),
            Source::Git { host, path, .. } => format!("{host}/{}", path.to_lowercase()),
            Source::Local(path) => path
                .canonicalize()
                .unwrap_or_else(|_| path.clone())
                .to_string_lossy()
                .into_owned(),
            Source::Release {
                owner, repo, name, ..
            } => format!(
                "release:{}/{}/{}",
                owner.to_lowercase(),
                repo.to_lowercase(),
                name
            ),
        }
    }
}

/// `npm:[@scope/]name[@version]`. Names follow npm's rules closely enough
/// to be safe as a directory and as an argument: lowercase, URL-safe
/// characters, no leading dot or dash, one optional `@scope/`.
fn parse_npm(rest: &str, spec: &str) -> Result<Source, String> {
    let rest = rest.trim();
    let (name, version) = match rest.strip_prefix('@') {
        // A scoped name has its own leading `@`; the version's comes after.
        Some(scoped) => match scoped.split_once('@') {
            Some((name, version)) => (format!("@{name}"), Some(version)),
            None => (format!("@{scoped}"), None),
        },
        None => match rest.split_once('@') {
            Some((name, version)) => (name.to_string(), Some(version)),
            None => (rest.to_string(), None),
        },
    };
    let bare = name.strip_prefix('@').unwrap_or(&name);
    let segments: Vec<&str> = bare.split('/').collect();
    let valid_segment = |s: &str| {
        !s.is_empty()
            && s.len() <= 214
            && !s.starts_with(['.', '-', '_'])
            && s.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')
            })
    };
    let shape_ok = match (name.starts_with('@'), segments.as_slice()) {
        (false, [only]) => valid_segment(only),
        (true, [scope, pkg]) => valid_segment(scope) && valid_segment(pkg),
        _ => false,
    };
    if !shape_ok {
        return Err(format!("`{spec}` is not an npm package name"));
    }
    let version = match version {
        Some("") => return Err(format!("`{spec}` has an empty version")),
        Some(v) if v.starts_with('-') || v.chars().any(char::is_whitespace) => {
            return Err(format!("`{spec}` has an unsafe version"))
        }
        Some(v) => Some(v.to_string()),
        None => None,
    };
    Ok(Source::Npm { name, version })
}

/// `release:<owner>/<repo>/<name>[@tag]`.
fn parse_release(rest: &str, spec: &str) -> Result<Source, String> {
    let (path, tag) = match rest.rsplit_once('@') {
        Some((path, tag)) if !tag.is_empty() => (path, Some(tag.to_string())),
        Some((path, _)) => (path, None),
        None => (rest, None),
    };
    let parts: Vec<&str> = path.split('/').collect();
    let [owner, repo, name] = parts.as_slice() else {
        return Err(format!(
            "`{spec}` is not a release source — use release:<owner>/<repo>/<name>[@tag]"
        ));
    };
    let clean = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            && s != "."
            && s != ".."
    };
    if !clean(owner) || !clean(repo) || !clean(name) {
        return Err(format!("`{spec}` has an unsafe release path"));
    }
    if tag.as_deref().is_some_and(|t| {
        t.starts_with('-') || t.contains(['/', '\\']) || t.contains(char::is_whitespace)
    }) {
        return Err(format!("`{spec}` has an unsafe tag"));
    }
    Ok(Source::Release {
        owner: owner.to_string(),
        repo: repo.to_string(),
        name: name.to_string(),
        tag,
    })
}

/// Install or update a release package from `base` (the releases URL) and
/// `api` (the latest-release endpoint), separated from the GitHub URLs so a
/// test can serve a release. An unpinned package that is already at the
/// latest tag is left alone.
pub async fn install_release_from(source: &Source, base: &str, api: &str) -> Result<(), String> {
    let Source::Release { name, tag, .. } = source else {
        return Ok(());
    };
    let root = source.root();
    let managed = home::packages_dir();
    if !root.starts_with(&managed) || root == managed {
        return Err(format!("refusing to write outside {}", managed.display()));
    }
    let wanted = match tag {
        Some(tag) => tag.clone(),
        None => crate::core::update::latest_tag_from(api)
            .await?
            .ok_or("the repository has no releases")?,
    };
    if crate::core::update::installed_release_tag(&root).as_deref() == Some(wanted.as_str())
        && root.join("extensions").join(name).is_file()
    {
        return Ok(());
    }
    crate::core::update::install_release_package(base, &wanted, name, &root).await
}

async fn install_release(source: &Source) -> Result<(), String> {
    let Source::Release { owner, repo, .. } = source else {
        return Ok(());
    };
    let (base, api) = crate::core::update::github_release_urls(owner, repo);
    install_release_from(source, &base, &api).await
}

fn expand_local(spec: &str) -> PathBuf {
    let path = match spec.strip_prefix("~/") {
        Some(rest) => match home::user_home() {
            Some(user_home) => user_home.join(rest),
            None => PathBuf::from(spec),
        },
        None => PathBuf::from(spec),
    };
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    };
    // The real path, so `./pkg` and a symlinked temp root record the same
    // way they resolve; a path that does not exist yet stays as written.
    path.canonicalize().unwrap_or(path)
}

/// Split the trailing `@ref`, ignoring the `user@` of an SSH authority.
fn split_rev(rest: &str) -> (String, Option<String>) {
    let (scheme, remainder) = match rest.find("://") {
        Some(at) => (&rest[..at + 3], &rest[at + 3..]),
        None => ("", rest),
    };
    // A `user@` (or `user:password@`) authority sits before the path; a ref
    // sits after it. Only an `@` beyond the authority is a ref marker. With
    // a scheme the authority runs to the first `/` — a `:` inside it is a
    // port or a password. Without one (scp form) it ends at the first `:`.
    let authority_end = if scheme.is_empty() {
        remainder.find(['/', ':'])
    } else {
        remainder.find('/')
    }
    .unwrap_or(remainder.len());
    match remainder[authority_end..].rfind('@') {
        Some(at) => {
            let at = authority_end + at;
            let rev = remainder[at + 1..].to_string();
            let base = format!("{scheme}{}", &remainder[..at]);
            if rev.is_empty() {
                (base, None)
            } else {
                (base, Some(rev))
            }
        }
        None => (rest.to_string(), None),
    }
}

fn parse_git(rest: &str, spec: &str) -> Result<Source, String> {
    let (url, rev) = split_rev(rest.trim());
    if let Some(rev) = &rev {
        if rev.starts_with('-') || rev.chars().any(char::is_whitespace) {
            return Err(format!("`{rev}` is not a git ref"));
        }
    }
    let (host_part, path_part, url) = if let Some(at) = url.find("://") {
        // scheme://[user@]host[:port]/path
        let after = &url[at + 3..];
        let slash = after.find('/').unwrap_or(after.len());
        let authority = &after[..slash];
        let host = authority.rsplit('@').next().unwrap_or(authority);
        let host = host.split(':').next().unwrap_or(host);
        (host.to_string(), after[slash..].to_string(), url.clone())
    } else if let Some((authority, path)) = url.split_once(':') {
        // scp-style: [user@]host:path
        if authority.contains('/') {
            return Err(format!("`{spec}` is not a git source"));
        }
        let host = authority.rsplit('@').next().unwrap_or(authority);
        (host.to_string(), path.to_string(), url.clone())
    } else {
        // shorthand host/user/repo
        match url.split_once('/') {
            Some((host, path)) => (
                host.to_string(),
                path.to_string(),
                format!("https://{host}/{path}"),
            ),
            None => return Err(format!("`{spec}` is not a git source")),
        }
    };
    let host = host_part.trim().to_lowercase();
    if host.is_empty() && !url.starts_with("file://") {
        return Err(format!("`{spec}` has no host"));
    }
    let host = if host.is_empty() {
        "file".to_string()
    } else {
        host
    };
    // The host becomes the first directory under the managed root, so it
    // must be a plain name: dots inside are fine, a leading one is not.
    if host.starts_with('.')
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(format!("`{host}` is not a host name"));
    }
    let path = path_part.trim_matches('/');
    let path = path
        .strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/');
    if path.is_empty() {
        return Err(format!("`{spec}` names no repository"));
    }
    for segment in path.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains('\\') {
            return Err(format!("`{spec}` has an unsafe repository path"));
        }
    }
    Ok(Source::Git {
        url,
        host,
        path: path.to_string(),
        rev,
    })
}

/// A configured package, as `e packages` shows it.
pub struct Package {
    /// The settings entry, verbatim.
    pub spec: String,
    pub status: Status,
}

pub enum Status {
    /// On disk; per-kind resource counts in [`KINDS`] order.
    Installed { counts: [usize; 4] },
    /// Listed in settings, absent on disk — `e install` restores it.
    Missing,
    /// The settings entry does not parse.
    Invalid(String),
}

/// Per-kind glob filters on one package: which of its resources load. An
/// empty list for a kind loads everything of that kind. Patterns are
/// relative to the package root (`extensions/legacy.mjs`, `skills/*`); a
/// leading `!` excludes. With only exclusions, everything else loads; with
/// any inclusion, only what an inclusion names — minus the exclusions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// One pattern list per kind, in [`KINDS`] order.
    patterns: [Vec<String>; 4],
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.patterns.iter().all(Vec::is_empty)
    }

    /// Whether `name` — a file or directory directly under the package's
    /// `<kind>/` — loads.
    pub fn allows(&self, kind: &str, name: &str) -> bool {
        let Some(index) = KINDS.iter().position(|k| *k == kind) else {
            return true;
        };
        let patterns = &self.patterns[index];
        if patterns.is_empty() {
            return true;
        }
        let candidate = format!("{kind}/{name}");
        let matches = |pattern: &str| {
            crate::core::tools::glob_regex(pattern).is_ok_and(|re| re.is_match(&candidate))
        };
        let mut included = !patterns.iter().any(|p| !p.starts_with('!'));
        for pattern in patterns {
            match pattern.strip_prefix('!') {
                Some(excluded) if matches(excluded) => return false,
                Some(_) => {}
                None if matches(pattern) => included = true,
                None => {}
            }
        }
        included
    }

    /// The filter's settings form: the kind keys of an object entry.
    fn from_object(object: &serde_json::Map<String, serde_json::Value>) -> Filter {
        let mut filter = Filter::default();
        for (index, kind) in KINDS.iter().enumerate() {
            if let Some(list) = object.get(*kind).and_then(|v| v.as_array()) {
                filter.patterns[index] = list
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect();
            }
        }
        filter
    }
}

/// One `packages` entry of `settings.json`: the source as typed, and any
/// filters. Written back as a plain string when it has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub source: String,
    pub filter: Filter,
}

impl Entry {
    fn plain(source: &str) -> Entry {
        Entry {
            source: source.to_string(),
            filter: Filter::default(),
        }
    }

    fn from_value(value: &serde_json::Value) -> Option<Entry> {
        if let Some(source) = value.as_str() {
            return Some(Entry::plain(source));
        }
        let object = value.as_object()?;
        let source = object.get("source")?.as_str()?;
        Some(Entry {
            source: source.to_string(),
            filter: Filter::from_object(object),
        })
    }

    fn to_value(&self) -> serde_json::Value {
        if self.filter.is_empty() {
            return serde_json::Value::String(self.source.clone());
        }
        let mut object = serde_json::Map::new();
        object.insert("source".into(), self.source.clone().into());
        for (index, kind) in KINDS.iter().enumerate() {
            let patterns = &self.filter.patterns[index];
            if !patterns.is_empty() {
                object.insert((*kind).into(), serde_json::json!(patterns));
            }
        }
        serde_json::Value::Object(object)
    }
}

/// The entries recorded in `settings.json`, in order — what `e install` and
/// `e remove` edit. A malformed entry is dropped here and rewritten away by
/// the next edit.
pub fn settings_entries() -> Vec<Entry> {
    settings::get_array(SETTINGS_KEY)
        .unwrap_or_default()
        .iter()
        .filter_map(Entry::from_value)
        .collect()
}

fn set_entries(entries: &[Entry]) -> std::io::Result<()> {
    settings::set_array(SETTINGS_KEY, entries.iter().map(Entry::to_value).collect())
}

/// A trusted repository's own list: `<cwd>/.e/packages`, one source per
/// line, `#` comments. Shared by the team through the repository; installs
/// land in the user's managed roots like any other package. Only npm, git,
/// and release sources are honoured: a local directory would run in place,
/// and trusting a checkout must not be enough to execute code it carries.
pub fn project_entries(cwd: &Path) -> Vec<String> {
    if !crate::core::config::trust::trusted(cwd) {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(cwd.join(".e").join("packages")) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter(|line| !matches!(Source::parse(line), Ok(Source::Local(_))))
        .map(str::to_string)
        .collect()
}

/// Every configured entry: settings first, then the current directory's
/// project list (a package already in settings is not repeated, so the
/// user's filters on it stand).
pub fn configured() -> Vec<Entry> {
    let mut entries = settings_entries();
    let cwd = std::env::current_dir().unwrap_or_default();
    for source in project_entries(&cwd) {
        let same = |a: &str, b: &str| match (Source::parse(a), Source::parse(b)) {
            (Ok(a), Ok(b)) => a.identity() == b.identity(),
            _ => a == b,
        };
        if !entries.iter().any(|known| same(&known.source, &source)) {
            entries.push(Entry::plain(&source));
        }
    }
    entries
}

/// Project-list packages not on disk — what trusting the directory offers
/// to install.
pub fn project_missing(cwd: &Path) -> Vec<String> {
    project_entries(cwd)
        .into_iter()
        .filter(|spec| Source::parse(spec).is_ok_and(|s| !s.root().is_dir()))
        .collect()
}

/// Roots loaded for this process only (`--package`), kept beside the
/// configured ones and forgotten at exit.
fn once_roots() -> &'static std::sync::Mutex<Vec<PathBuf>> {
    static ROOTS: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> = std::sync::OnceLock::new();
    ROOTS.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Load a package for this run without recording it: a local directory is
/// used in place, a git source is cloned into a temporary directory, a
/// release asset is fetched into one. Returns the root.
pub async fn use_once(spec: &str) -> Result<PathBuf, String> {
    let source = Source::parse(spec)?;
    let root = match &source {
        Source::Local(path) => {
            if !path.is_dir() {
                return Err(format!("{} is not a directory", path.display()));
            }
            path.clone()
        }
        Source::Npm { name, .. } => {
            // A throwaway prefix: the package lands at
            // `<dir>/node_modules/<name>`, and the whole prefix goes at exit.
            let dir = std::env::temp_dir().join(format!(
                "e-package-{}-{}",
                std::process::id(),
                once_roots().lock().unwrap_or_else(|e| e.into_inner()).len()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            npm_install(&dir, &source)?;
            // The loadable root is the package itself, not the prefix;
            // `forget_once` finds the prefix again from the root.
            dir.join("node_modules").join(name)
        }
        Source::Git { url, rev, .. } => {
            let dir = std::env::temp_dir().join(format!(
                "e-package-{}-{}",
                std::process::id(),
                once_roots().lock().unwrap_or_else(|e| e.into_inner()).len()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            git(
                &std::env::temp_dir(),
                &["clone", "--quiet", "--", url, &dir.to_string_lossy()],
            )?;
            if let Some(rev) = rev {
                checkout(&dir, rev)?;
            }
            install_dependencies(&dir)?;
            dir
        }
        Source::Release {
            owner,
            repo,
            name,
            tag,
            ..
        } => {
            let dir = std::env::temp_dir().join(format!(
                "e-package-{}-{}",
                std::process::id(),
                once_roots().lock().unwrap_or_else(|e| e.into_inner()).len()
            ));
            let (base, api) = crate::core::update::github_release_urls(owner, repo);
            let wanted = match tag {
                Some(tag) => tag.clone(),
                None => crate::core::update::latest_tag_from(&api)
                    .await?
                    .ok_or("the repository has no releases")?,
            };
            crate::core::update::install_release_package(&base, &wanted, name, &dir).await?;
            dir
        }
    };
    once_roots()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(root.clone());
    Ok(root)
}

/// Remove the temporary clones `use_once` made. Local directories are
/// untouched. An npm root sits inside its throwaway prefix, so the
/// directory removed is the ancestor this process created under the
/// temporary directory, not necessarily the root itself.
pub fn forget_once() {
    let roots = std::mem::take(&mut *once_roots().lock().unwrap_or_else(|e| e.into_inner()));
    let prefix = format!("e-package-{}-", std::process::id());
    let temp = std::env::temp_dir();
    for root in roots {
        let temporary = root.ancestors().find(|dir| {
            dir.parent() == Some(temp.as_path())
                && dir
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
        });
        if let Some(dir) = temporary {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Every configured package with its on-disk status.
pub fn list() -> Vec<Package> {
    configured()
        .into_iter()
        .map(|entry| {
            let spec = entry.source;
            let status = match Source::parse(&spec) {
                Err(reason) => Status::Invalid(reason),
                Ok(source) => {
                    let root = source.root();
                    if root.is_dir() {
                        Status::Installed {
                            counts: counts(&root),
                        }
                    } else {
                        Status::Missing
                    }
                }
            };
            Package { spec, status }
        })
        .collect()
}

/// The roots of every package present on disk, in settings order, then
/// the project's, then this run's `--package` roots.
pub fn roots() -> Vec<PathBuf> {
    loaded().into_iter().map(|(root, _)| root).collect()
}

/// Every package present on disk with its filter, in settings order, then
/// the one-run roots (unfiltered).
fn loaded() -> Vec<(PathBuf, Filter)> {
    let mut roots: Vec<(PathBuf, Filter)> = configured()
        .into_iter()
        .filter_map(|entry| {
            Source::parse(&entry.source)
                .ok()
                .map(|s| (s.root(), entry.filter))
        })
        .filter(|(root, _)| root.is_dir())
        .collect();
    for root in once_roots()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
    {
        if !roots.iter().any(|(known, _)| known == root) {
            roots.push((root.clone(), Filter::default()));
        }
    }
    roots
}

/// Each installed package's `<kind>/` directory, when it has one, with the
/// filter a loader asks before taking a resource from it.
pub fn dirs(kind: &str) -> Vec<(PathBuf, Filter)> {
    loaded()
        .into_iter()
        .map(|(root, filter)| (root.join(kind), filter))
        .filter(|(dir, _)| dir.is_dir())
        .collect()
}

/// Settings entries that are not on disk, for the startup notice.
pub fn missing() -> Vec<String> {
    list()
        .into_iter()
        .filter(|p| matches!(p.status, Status::Missing))
        .map(|p| p.spec)
        .collect()
}

/// How many resources a package root carries of each kind, in [`KINDS`]
/// order: executables (or bundle directories), `SKILL.md` folders, `.md`
/// files, `.json` files.
pub fn counts(root: &Path) -> [usize; 4] {
    let entries = |kind: &str| -> Vec<PathBuf> {
        std::fs::read_dir(root.join(kind))
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    let extensions = entries("extensions")
        .iter()
        .filter(|p| p.is_dir() || is_executable(p))
        .count();
    let skills = entries("skills")
        .iter()
        .filter(|p| p.join("SKILL.md").is_file())
        .count();
    let has_ext = |p: &PathBuf, ext: &str| p.extension().is_some_and(|x| x == ext);
    let prompts = entries("prompts")
        .iter()
        .filter(|p| has_ext(p, "md"))
        .count();
    let themes = entries("themes")
        .iter()
        .filter(|p| has_ext(p, "json"))
        .count();
    [extensions, skills, prompts, themes]
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file()
        && std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Install one source: clone (or sync an existing clone to its ref) and
/// record it in settings, replacing an entry for the same package at another
/// ref. Returns the root and its resource counts.
pub async fn install(spec: &str) -> Result<(PathBuf, [usize; 4]), String> {
    let source = Source::parse(spec)?;
    match &source {
        Source::Local(path) => {
            if !path.is_dir() {
                return Err(format!("{} is not a directory", path.display()));
            }
        }
        Source::Npm { .. } => npm_install(&npm_prefix(), &source)?,
        Source::Git { .. } => sync(&source)?,
        Source::Release { .. } => install_release(&source).await?,
    }
    let root = source.root();
    if counts(&root).iter().all(|n| *n == 0) {
        eprintln!(
            "note: {} carries no extensions/, skills/, prompts/, or themes/",
            root.display()
        );
    }
    // A local path is recorded as it resolved, so a `./pkg` installed from
    // one directory still loads (and removes) from any other.
    let recorded = match &source {
        Source::Local(path) => path.to_string_lossy().into_owned(),
        _ => spec.trim().to_string(),
    };
    record(&source, &recorded).map_err(|e| format!("could not update settings.json: {e}"))?;
    let counts = counts(&root);
    Ok((root, counts))
}

/// Make disk match settings: clone what is missing, sync every git package
/// to its ref (or the tip of its default branch when unpinned). Returns one
/// line per package for the report; the first failure stops nothing else.
pub async fn install_all() -> Vec<Result<String, String>> {
    let mut results = Vec::new();
    for entry in configured() {
        results.push(install_one(&entry.source).await);
    }
    results
}

/// Install the sources a trusted repository lists that are not on disk —
/// what `/trust` offers. One line per package, like [`install_all`].
pub async fn install_project(cwd: &Path) -> Vec<Result<String, String>> {
    let mut results = Vec::new();
    for spec in project_missing(cwd) {
        results.push(install_one(&spec).await);
    }
    results
}

async fn install_one(spec: &str) -> Result<String, String> {
    let source = Source::parse(spec)?;
    match &source {
        Source::Local(path) if !path.is_dir() => {
            Err(format!("{spec}: {} is not a directory", path.display()))
        }
        Source::Local(_) => Ok(format!("{spec}: in place")),
        Source::Npm { .. } => {
            let before = installed_npm_version(&source.root());
            npm_install(&npm_prefix(), &source).map_err(|e| format!("{spec}: {e}"))?;
            let after = installed_npm_version(&source.root());
            Ok(format!(
                "{spec}: {}",
                match (before, after) {
                    (None, Some(version)) => format!("installed {version}"),
                    (Some(old), Some(new)) if old != new => format!("updated {old} → {new}"),
                    _ => "up to date".to_string(),
                }
            ))
        }
        Source::Git { .. } => {
            let fresh = !source.root().is_dir();
            sync(&source).map_err(|e| format!("{spec}: {e}"))?;
            Ok(format!(
                "{spec}: {}",
                if fresh { "installed" } else { "up to date" }
            ))
        }
        Source::Release { .. } => {
            let before = crate::core::update::installed_release_tag(&source.root());
            install_release(&source)
                .await
                .map_err(|e| format!("{spec}: {e}"))?;
            let after = crate::core::update::installed_release_tag(&source.root());
            Ok(format!(
                "{spec}: {}",
                match (before, after) {
                    (None, Some(tag)) => format!("installed {tag}"),
                    (Some(old), Some(new)) if old != new => format!("updated {old} → {new}"),
                    _ => "up to date".to_string(),
                }
            ))
        }
    }
}

/// Forget a package: drop its settings entry and delete a managed clone. A
/// local directory is left alone — e never owned it.
pub fn remove(spec: &str) -> Result<PathBuf, String> {
    let source = Source::parse(spec)?;
    let identity = source.identity();
    let mut entries = settings_entries();
    // The recorded spec names the directory on disk; the argument only has
    // to name the same package (identity folds case, scheme, and `.git`).
    let mut installed = None;
    entries.retain(|entry| match Source::parse(&entry.source) {
        Ok(recorded) if recorded.identity() == identity => {
            installed = Some(recorded);
            false
        }
        _ => true,
    });
    let Some(source) = installed else {
        return Err(format!("{spec} is not installed"));
    };
    set_entries(&entries).map_err(|e| format!("could not update settings.json: {e}"))?;
    let root = source.root();
    if let Source::Npm { name, .. } = &source {
        if root.is_dir() {
            npm(
                &npm_prefix(),
                &[
                    "uninstall",
                    "--ignore-scripts",
                    "--no-audit",
                    "--no-fund",
                    "--",
                    name,
                ],
            )?;
        }
    } else if matches!(source, Source::Git { .. } | Source::Release { .. }) {
        let managed = home::packages_dir();
        if root.starts_with(&managed) && root != managed && root.exists() {
            std::fs::remove_dir_all(&root)
                .map_err(|e| format!("could not delete {}: {e}", root.display()))?;
            // Empty `<host>/<user>` parents are litter, not state.
            let mut parent = root.parent();
            while let Some(dir) = parent {
                if dir == managed || std::fs::remove_dir(dir).is_err() {
                    break;
                }
                parent = dir.parent();
            }
        }
    }
    Ok(root)
}

/// Append the source as typed, replacing any entry for the same package so
/// `e install …@v2` moves a pin instead of duplicating it. Filters the
/// replaced entry carried stay with it.
fn record(source: &Source, spec: &str) -> std::io::Result<()> {
    let identity = source.identity();
    let mut entries = settings_entries();
    let mut filter = Filter::default();
    entries.retain(|entry| {
        let same = Source::parse(&entry.source).is_ok_and(|s| s.identity() == identity);
        if same {
            filter = entry.filter.clone();
        }
        !same
    });
    entries.push(Entry {
        source: spec.to_string(),
        filter,
    });
    set_entries(&entries)
}

/// Bring a git package's clone to the requested state: a fresh clone when
/// absent, otherwise fetch and check out the pinned ref, or fast-forward the
/// default branch. A failed fresh clone leaves nothing behind.
fn sync(source: &Source) -> Result<(), String> {
    let Source::Git { url, rev, .. } = source else {
        return Ok(());
    };
    let root = source.root();
    let managed = home::packages_dir();
    if !root.starts_with(&managed) || root == managed {
        return Err(format!("refusing to write outside {}", managed.display()));
    }
    if !root.is_dir() {
        let parent = root.parent().unwrap_or(&managed);
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let result = git(
            parent,
            &["clone", "--quiet", "--", url, &root.to_string_lossy()],
        )
        .and_then(|_| match rev {
            Some(rev) => checkout(&root, rev),
            None => Ok(()),
        });
        if let Err(e) = result {
            let _ = std::fs::remove_dir_all(&root);
            return Err(e);
        }
        return install_dependencies(&root);
    }
    git(&root, &["fetch", "--quiet", "--tags", "origin"])?;
    let synced = match rev {
        Some(rev) => checkout(&root, rev),
        None => {
            // A clone that was pinned earlier sits detached; return to the
            // remote's default branch before fast-forwarding.
            if git(&root, &["symbolic-ref", "--quiet", "HEAD"]).is_err() {
                let head = git(
                    &root,
                    &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
                )?;
                let branch = head
                    .trim()
                    .strip_prefix("origin/")
                    .unwrap_or(head.trim())
                    .to_string();
                // A branch name is a ref, not a pathspec, so no `--` here;
                // git itself refuses to create names that start with `-`.
                git(&root, &["checkout", "--quiet", &branch])?;
            }
            git(&root, &["pull", "--quiet", "--ff-only"]).map(|_| ())
        }
    };
    synced.and_then(|()| install_dependencies(&root))
}

/// The npm project every npm package installs into. One `package.json` of
/// e's own marks it, so npm treats it as a project rather than walking up
/// to whatever the user has above `~/.e`.
pub fn npm_prefix() -> PathBuf {
    home::packages_dir().join("npm")
}

/// Install (or bring current) one npm package into the project at `prefix`,
/// creating the project on first use. Lifecycle scripts never run: the
/// package's code runs when e loads it, not when npm unpacks it. An
/// unpinned package asks for `latest`, which is how `e install` updates it.
fn npm_install(prefix: &Path, source: &Source) -> Result<(), String> {
    let Source::Npm { name, version } = source else {
        return Ok(());
    };
    std::fs::create_dir_all(prefix).map_err(|e| e.to_string())?;
    let manifest = prefix.join("package.json");
    if !manifest.is_file() {
        std::fs::write(
            &manifest,
            "{\n  \"name\": \"e-packages\",\n  \"private\": true,\n  \"description\": \"npm packages e installed; edit with `e install` and `e remove`\"\n}\n",
        )
        .map_err(|e| e.to_string())?;
    }
    let spec = format!("{name}@{}", version.as_deref().unwrap_or("latest"));
    npm(
        prefix,
        &[
            "install",
            "--ignore-scripts",
            "--omit=dev",
            "--no-audit",
            "--no-fund",
            "--save-exact",
            "--",
            &spec,
        ],
    )
    .map(|_| ())
}

/// The version an npm package is installed at, from its own `package.json`.
fn installed_npm_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("version")?.as_str().map(str::to_string)
}

/// A git package whose `package.json` declares dependencies gets them
/// installed beside it, scripts off, so an extension that imports a library
/// runs after `e install` the way an npm package's would. A package without
/// a manifest, or without dependencies, is left exactly as cloned.
fn install_dependencies(root: &Path) -> Result<(), String> {
    let manifest = root.join("package.json");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return Ok(());
    };
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let has_dependencies = json
        .get("dependencies")
        .and_then(|d| d.as_object())
        .is_some_and(|d| !d.is_empty());
    if !has_dependencies {
        return Ok(());
    }
    let subcommand = if root.join("package-lock.json").is_file() {
        "ci"
    } else {
        "install"
    };
    npm(
        root,
        &[
            subcommand,
            "--ignore-scripts",
            "--omit=dev",
            "--no-audit",
            "--no-fund",
        ],
    )
    .map(|_| ())
}

/// Run npm in `cwd`, returning stdout; a failure carries npm's own stderr.
/// Every call names its directory, and none may run a package's scripts.
fn npm(cwd: &Path, args: &[&str]) -> Result<String, String> {
    debug_assert!(args.contains(&"--ignore-scripts"));
    let output = Command::new("npm")
        .current_dir(cwd)
        .env("NPM_CONFIG_UPDATE_NOTIFIER", "false")
        .args(args)
        .output()
        .map_err(|e| format!("could not run npm: {e} (npm packages need npm on PATH)"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Err(if stderr.is_empty() {
        format!("npm {} failed", args.first().unwrap_or(&""))
    } else {
        format!("npm {}: {stderr}", args.first().unwrap_or(&""))
    })
}

/// Detach at `rev`: the remote branch of that name first (so a branch pin
/// tracks the fetched tip, not a stale local branch), then the tag or commit.
fn checkout(root: &Path, rev: &str) -> Result<(), String> {
    let remote = format!("origin/{rev}");
    if git(root, &["checkout", "--quiet", "--detach", &remote, "--"]).is_ok() {
        return Ok(());
    }
    git(root, &["checkout", "--quiet", "--detach", rev, "--"]).map(|_| ())
}

/// Run git in `cwd`, returning stdout; a failure carries git's own stderr.
/// Every call names its directory so no command inherits the process cwd —
/// a checkout's own `.git/config` (an `insteadOf` rewrite, say) must not
/// shape a clone e performs. Git never prompts: a source that needs
/// credentials fails instead of hanging the terminal.
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .map_err(|e| format!("could not run git: {e}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    Err(if stderr.is_empty() {
        format!("git {} failed", args.first().unwrap_or(&""))
    } else {
        format!("git {}: {stderr}", args.first().unwrap_or(&""))
    })
}

/// `e packages init <dir>`: a package to start from. One extension on the
/// optional scaffold, the three other directories ready, a `package.json`
/// carrying the `e-package` keyword so `npm publish` lists it in the
/// catalog, and a README that says what to change. Refuses a directory that
/// already has files.
pub fn init(dir: &Path) -> Result<Vec<PathBuf>, String> {
    if dir.is_dir()
        && std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(format!("{} is not empty", dir.display()));
    }
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| parse_npm(n, n).is_ok())
        .map(str::to_string)
        .unwrap_or_else(|| "my-e-package".to_string());
    let files: [(&str, String); 7] = [
        ("extensions/scaffold.mjs", SCAFFOLD.to_string()),
        ("extensions/hello.mjs", HELLO.to_string()),
        ("skills/.keep", String::new()),
        ("prompts/.keep", String::new()),
        ("themes/.keep", String::new()),
        (
            "package.json",
            format!(
                "{{\n  \"name\": \"{name}\",\n  \"version\": \"0.1.0\",\n  \"description\": \"an e package\",\n  \"keywords\": [\"e-package\"],\n  \"license\": \"MIT\",\n  \"files\": [\"extensions\", \"skills\", \"prompts\", \"themes\", \"README.md\"]\n}}\n"
            ),
        ),
        ("README.md", README.replace("{name}", &name)),
    ];
    let mut written = Vec::new();
    for (relative, contents) in files {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
        #[cfg(unix)]
        if relative.starts_with("extensions/") {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
        }
        written.push(path);
    }
    Ok(written)
}

const SCAFFOLD: &str = include_str!("../../../docs/customize/examples/scaffold.mjs");

const HELLO: &str = r#"#!/usr/bin/env node
// hello — the smallest useful extension: one tool the model can call and
// one command the user can run. Rename it, then grow it; the protocol is
// docs/customize/extensions.md, the helper beside this file is optional.
import { connect } from "./scaffold.mjs";

const ext = connect({
  manifest: {
    name: "hello",
    version: "0.1",
    description: "greets, as a tool and as a command",
    tools: [
      {
        name: "hello",
        description: "Greet someone by name.",
        parameters: { type: "object", properties: { name: { type: "string" } } },
      },
    ],
    commands: [{ name: "hello", description: "say hello from this package" }],
  },
  tool({ arguments: { name } }) {
    return { content: `hello, ${name || "world"}` };
  },
  command() {
    return { notice: "hello from your package" };
  },
});

ext.run();
"#;

const README: &str = r#"# {name}

An [e](https://github.com/intuitums/e) package: any subset of `extensions/`,
`skills/`, `prompts/`, and `themes/`, laid out like `~/.e/` itself.

Try it in place while you work on it:

```sh
e --package . 
```

Install it for good:

```sh
e install .
```

Publish it so `e install npm:{name}` works for everyone — the `e-package`
keyword in `package.json` is what lists it in the catalog:

```sh
npm publish
```

Extensions must be executable (`chmod +x extensions/*.mjs`, and commit the
mode). Delete the directories you do not use; the `.keep` files only hold
them in git.
"#;

/// True when a path sits inside the managed packages root — the skills
/// picker uses it to label a skill's scope.
pub fn is_packaged(path: &Path) -> bool {
    path.starts_with(home::packages_dir()) || roots().iter().any(|root| path.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_parts(spec: &str) -> (String, String, String, Option<String>) {
        match Source::parse(spec).unwrap() {
            Source::Git {
                url,
                host,
                path,
                rev,
            } => (url, host, path, rev),
            Source::Local(_) | Source::Release { .. } | Source::Npm { .. } => {
                panic!("{spec} parsed as another kind")
            }
        }
    }

    #[test]
    fn shorthand_clones_over_https_and_splits_the_ref() {
        let (url, host, path, rev) = git_parts("git:github.com/intuitums/e-diff@v1");
        assert_eq!(url, "https://github.com/intuitums/e-diff");
        assert_eq!(host, "github.com");
        assert_eq!(path, "intuitums/e-diff");
        assert_eq!(rev.as_deref(), Some("v1"));
        let (_, _, _, rev) = git_parts("git:github.com/intuitums/e-diff");
        assert_eq!(rev, None);
    }

    #[test]
    fn urls_and_scp_forms_share_one_identity() {
        let specs = [
            "git:github.com/Intuitums/e-diff@v1",
            "https://github.com/intuitums/e-diff.git",
            "git:git@github.com:intuitums/e-diff@main",
            "ssh://git@github.com/intuitums/e-diff",
            "git:ssh://git@github.com:22/intuitums/e-diff@release/1.0",
        ];
        let identities: std::collections::HashSet<String> = specs
            .iter()
            .map(|s| Source::parse(s).unwrap().identity())
            .collect();
        assert_eq!(identities.len(), 1, "{identities:?}");
        let (url, _, _, rev) = git_parts("git:git@github.com:intuitums/e-diff@main");
        assert_eq!(url, "git@github.com:intuitums/e-diff");
        assert_eq!(rev.as_deref(), Some("main"));
        let (_, _, _, rev) = git_parts("git:ssh://git@github.com:22/intuitums/e-diff@release/1.0");
        assert_eq!(rev.as_deref(), Some("release/1.0"));
    }

    #[test]
    fn credentials_in_a_url_authority_are_not_a_ref() {
        // GitLab's deploy-token form: the `@` after the password ends the
        // authority, and the `:` inside the credentials is not the path.
        let (url, host, path, rev) = git_parts("https://oauth2:TOKEN@gitlab.com/group/repo.git");
        assert_eq!(url, "https://oauth2:TOKEN@gitlab.com/group/repo.git");
        assert_eq!(host, "gitlab.com");
        assert_eq!(path, "group/repo");
        assert_eq!(rev, None);
        let (url, _, _, rev) = git_parts("https://oauth2:TOKEN@gitlab.com/group/repo.git@v2");
        assert_eq!(url, "https://oauth2:TOKEN@gitlab.com/group/repo.git");
        assert_eq!(rev.as_deref(), Some("v2"));
    }

    #[test]
    fn managed_root_is_host_then_path_and_never_escapes() {
        let source = Source::parse("git:github.com/intuitums/e-diff").unwrap();
        assert_eq!(
            source.root(),
            home::packages_dir()
                .join("github.com")
                .join("intuitums/e-diff")
        );
        assert!(Source::parse("git:github.com/../x").is_err());
        assert!(Source::parse("git:../intuitums/e").is_err());
        assert!(Source::parse("git:.hidden/intuitums/e").is_err());
        assert!(Source::parse("git:github.com/intuitums/e@-bad").is_err());
        assert!(Source::parse("--upload-pack=x").is_err());
        assert!(Source::parse("git:github.com").is_err());
    }

    #[test]
    fn npm_names_are_scoped_versioned_and_kept_safe() {
        let plain = Source::parse("npm:e-diff").unwrap();
        assert_eq!(
            plain,
            Source::Npm {
                name: "e-diff".into(),
                version: None
            }
        );
        assert_eq!(
            plain.root(),
            npm_prefix().join("node_modules").join("e-diff")
        );
        let scoped = Source::parse("npm:@fschr/e-diff@1.2.0").unwrap();
        assert_eq!(
            scoped,
            Source::Npm {
                name: "@fschr/e-diff".into(),
                version: Some("1.2.0".into())
            }
        );
        assert_eq!(scoped.identity(), "npm:@fschr/e-diff");
        assert_eq!(
            Source::parse("npm:@fschr/E-Diff@2").map(|s| s.identity()),
            Err("`npm:@fschr/E-Diff@2` is not an npm package name".into())
        );
        for bad in [
            "npm:",
            "npm:.hidden",
            "npm:-x",
            "npm:a/b",
            "npm:@s",
            "npm:x@",
            "npm:x@-y",
        ] {
            assert!(Source::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn filters_include_exclude_and_round_trip_through_settings() {
        let value = serde_json::json!({
            "source": "npm:pack",
            "extensions": ["!extensions/legacy.mjs"],
            "prompts": ["prompts/review.md", "prompts/r*.md", "!prompts/rough.md"],
        });
        let entry = Entry::from_value(&value).unwrap();
        let filter = &entry.filter;
        // Only exclusions: everything else loads.
        assert!(filter.allows("extensions", "diff.mjs"));
        assert!(!filter.allows("extensions", "legacy.mjs"));
        // An inclusion: only what it names, minus exclusions.
        assert!(filter.allows("prompts", "review.md"));
        assert!(filter.allows("prompts", "recap.md"));
        assert!(!filter.allows("prompts", "rough.md"));
        assert!(!filter.allows("prompts", "other.md"));
        // A kind with no list is untouched.
        assert!(filter.allows("skills", "anything"));
        assert_eq!(entry.to_value(), value);
        assert_eq!(
            Entry::plain("git:github.com/u/r").to_value(),
            serde_json::Value::String("git:github.com/u/r".into())
        );
        assert!(Entry::from_value(&serde_json::json!({"extensions": []})).is_none());
    }

    #[test]
    fn local_paths_load_in_place_and_bare_words_are_refused() {
        assert!(matches!(
            Source::parse("/tmp/pkg").unwrap(),
            Source::Local(_)
        ));
        assert!(matches!(Source::parse("./pkg").unwrap(), Source::Local(p) if p.is_absolute()));
        assert!(Source::parse("intuitums/e-diff").is_err());
        let (_, host, path, _) = git_parts("file:///tmp/pkg");
        assert_eq!((host.as_str(), path.as_str()), ("file", "tmp/pkg"));
    }
}
