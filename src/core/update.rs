//! Self-update: fetch the latest release binary for this platform, verify
//! its checksum, and swap it in place — `e update` runs it by hand, and the
//! TUI runs it in the background at launch (opt out with the Auto-update
//! setting). The swap is an atomic rename next to the running binary; the
//! new version takes effect on the next start, which the notice says.
//!
//! Dev builds are exempt: a binary living under a `target/` directory is a
//! cargo artifact, and auto-update must never stomp one. So is any platform
//! off the release matrix (`target()` is `None`): a `cargo install` on musl,
//! armv7, FreeBSD, … must never be overwritten with a tarball its host
//! cannot run.

use std::path::Path;

const RELEASES: &str = "https://github.com/intuitums/e/releases";
const API_LATEST: &str = "https://api.github.com/repos/intuitums/e/releases/latest";

/// Release assets redirect to GitHub's download hosts. This client carries
/// no provider credentials and must not be reused for authenticated requests.
fn download_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .user_agent(format!("e/{}", crate::VERSION))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    let downgrade = attempt.previous().last().is_some_and(|previous| {
                        previous.scheme() == "https" && attempt.url().scheme() != "https"
                    });
                    if downgrade || attempt.previous().len() >= 10 {
                        attempt.error("unsafe or excessive release redirects")
                    } else {
                        attempt.follow()
                    }
                }))
                .connect_timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The release artifact name for this build's platform — `None` when the
/// release matrix (`.github/workflows/release.yml`) ships nothing for it, so
/// no update path may guess a tarball.
pub fn target() -> Option<&'static str> {
    release_target(
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "gnu"),
    )
}

/// `target()` as a pure function of the platform, so the off-matrix case
/// can be pinned from a machine that is on it. Linux releases are glibc
/// builds; a musl (Alpine) install is off the matrix.
pub fn release_target(os: &str, arch: &str, gnu_libc: bool) -> Option<&'static str> {
    Some(match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") if gnu_libc => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") if gnu_libc => "x86_64-unknown-linux-gnu",
        _ => return None,
    })
}

/// True when the running binary is a cargo build, not an installed release.
pub fn is_dev_build() -> bool {
    std::env::current_exe()
        .map(|p| p.components().any(|c| c.as_os_str() == "target"))
        .unwrap_or(true)
}

/// "1.2.3" -> comparable parts; unparseable segments compare as 0.
fn parts(version: &str) -> [u64; 3] {
    let mut out = [0u64; 3];
    for (i, piece) in version
        .trim_start_matches('v')
        .split('.')
        .take(3)
        .enumerate()
    {
        out[i] = piece.parse().unwrap_or(0);
    }
    out
}

/// True when the version is release SemVer — three numeric segments — the
/// same shape `scripts/release-check.sh` demands of a `vX.Y.Z` tag. Anything
/// else (a checkout stamped `dev`, a hand-edited identity) is not a release
/// and must never update itself or check for updates.
pub fn is_release_version(v: &str) -> bool {
    let v = v.trim_start_matches('v');
    let mut segs = v.split('.');
    let three = [segs.next(), segs.next(), segs.next()];
    segs.next().is_none()
        && three
            .iter()
            .all(|s| s.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    // A build whose version is not release SemVer never rolls itself
    // forward: a source checkout must update via cargo, not over itself.
    if !is_release_version(current) {
        return false;
    }
    parts(candidate) > parts(current)
}

/// The latest release tag ("v0.4.1"), from the GitHub API.
/// `None` — no release published — means nothing to update to, which the
/// flow reads as already current, not a failure.
pub async fn latest_tag() -> Result<Option<String>, String> {
    latest_tag_from(API_LATEST).await
}

/// `latest_tag` against an explicit API URL, so tests can serve the API.
pub async fn latest_tag_from(url: &str) -> Result<Option<String>, String> {
    let response = crate::core::providers::http()
        .map_err(|e| e.message)?
        .get(url)
        .header("accept", "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("update check failed: {e}"))?;
    if !response.status().is_success() {
        if response.status() == 404 {
            return Ok(None);
        }
        return Err(format!("update check failed: {}", response.status()));
    }
    let body: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    body["tag_name"]
        .as_str()
        .map(|tag| Some(tag.to_string()))
        .ok_or_else(|| "release has no tag".into())
}

/// Download `tag` for this platform from `base`, verify its checksum, and
/// atomically replace `dest`. Returns the installed version. `base` is a
/// parameter so tests can serve a fake release.
pub async fn install_from(base: &str, tag: &str, dest: &Path) -> Result<String, String> {
    let target = target().ok_or(NO_RELEASE)?;
    let tarball_url = format!("{base}/download/{tag}/e-{target}.tar.gz");
    let sums_url = format!("{base}/download/{tag}/checksums.txt");

    let tarball = fetch(&tarball_url).await?;
    let sums = String::from_utf8(fetch(&sums_url).await?).map_err(|e| e.to_string())?;
    let expected = sums
        .lines()
        .find(|l| l.ends_with(&format!(" e-{target}.tar.gz")))
        .and_then(|l| l.split_whitespace().next())
        .ok_or("no checksum for this platform")?;
    let actual = {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(&tarball);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    if actual != expected {
        return Err("checksum mismatch — refusing to install".into());
    }

    // Unpack next to the destination so the final rename stays on one
    // filesystem; the system tar does the extraction (no archive deps).
    let dir = dest.parent().ok_or("binary has no parent directory")?;
    let staging = dir.join(format!(".e-update-{tag}"));
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let archive = staging.join("e.tar.gz");
    std::fs::write(&archive, &tarball).map_err(|e| e.to_string())?;
    let unpacked = std::process::Command::new("tar")
        .arg("xzf")
        .arg(&archive)
        .current_dir(&staging)
        .status()
        .map_err(|e| format!("tar failed: {e}"))?;
    if !unpacked.success() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err("tar failed to unpack the update".into());
    }
    let new_binary = staging.join("e");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&new_binary, std::fs::Permissions::from_mode(0o755));
    }
    std::fs::rename(&new_binary, dest).map_err(|e| format!("install failed: {e}"))?;
    let _ = std::fs::remove_dir_all(&staging);
    Ok(tag.trim_start_matches('v').to_string())
}

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let response = download_client()?
        .get(url)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("download failed: {}", response.status()));
    }
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| e.to_string())
}

/// Why an off-matrix platform cannot self-update; `e update` prints it.
pub const NO_RELEASE: &str =
    "no release is published for this platform — update from source, not e update";

/// The whole flow for the running binary: check, install if newer. Ok(None)
/// means already current (or not applicable).
pub async fn self_update() -> Result<Option<String>, String> {
    // A build whose identity is not release SemVer — a source checkout —
    // or whose platform has no release artifact is never replaced by a
    // published release; the check is skipped, not just the install.
    if !is_release_version(crate::VERSION) || target().is_none() {
        return Ok(None);
    }
    // No published release: nothing exists to update to — already current.
    let Some(tag) = latest_tag().await? else {
        return Ok(None);
    };
    if !is_newer(&tag, crate::VERSION) {
        return Ok(None);
    }
    let dest = std::env::current_exe().map_err(|e| e.to_string())?;
    install_from(RELEASES, &tag, &dest).await.map(Some)
}
