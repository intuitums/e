# Releases and testing

## Run changes locally

Use Rust for local builds and Python 3.11 or newer for scenario and release tooling.
PR preview commands also require an authenticated GitHub CLI.

```sh
./x dev /path/to/project
./x scenario streaming
./x scenario tools
./x scenario cancellation
./x scenario long-output
./x scenario resume
```

`./x dev` builds the current checkout and runs it in the selected project.
Arguments after the project path go to e. Local builds use `~/.e-dev` and never
self-update. Set `E_HOME` to use another dedicated state directory.

Scenarios run the real terminal against the existing loopback fixture provider.
They use temporary settings, dummy credentials, and a disposable project; no
paid provider calls run. Streaming, long-output, and cancellation share the same
paced response so you can inspect scrolling or interrupt it. Tools enables a
synthetic shell command and leaves the workspace trust choice to you. Resume
reopens the saved conversation after the first terminal exits. State is removed
when the scenario command finishes.

## Release channels

Production releases live in `arocomputer/e`. PR previews build unsigned
artifacts on request and never publish. There are no dev or beta channels: a
release either ships from a version tag, or it is a pinned preview.

| Channel | Trigger | Executable | Default state |
| --- | --- | --- | --- |
| production | `vX.Y.Z` tag on a commit reachable from main | `e` | `~/.e` |
| PR | `preview` workflow, explicitly requested PR | `e-pr-NUMBER` | `~/.e-pr/COMMIT` |

The root `Cargo.toml` owns the production version, under `[workspace.package]`. `scripts/release/identity.py` derives channel,
package tag, executable name, and preview version. `crates/core/build.rs` embeds the workflow's
version, channel, and full source commit. Production identities use `X.Y.Z`;
PR previews use `0.0.0-pr-BUILD`, where BUILD is the `preview` workflow run
number, so a preview never claims a release version it is not. `e --version
--json` and `e doctor` report the build identity.

Curl installations follow production; package installations update through their
package manager. PR and local builds never self-update.

Each home owns its credentials, settings, sessions, and extensions. Use a
disposable project or worktree when trying unfinished features.

## Install production

```sh
curl -fsSL https://e.intuitum.sh/install.sh | sh
npm install -g @arocomputer/e
bun add -g @arocomputer/e
brew install arocomputer/tap/e
```

To reinstall an older version with curl, pass `--version X.Y.Z`. npm and
bun accept an exact package version after `@`. Use curl in a separate directory
for a historical brew build. An older curl build follows newer releases again
unless auto-update is disabled in its settings.

## Try a PR before merging

```sh
./x preview 123
# After its Preview run succeeds:
./x preview 123 --run RUN_ID
```

Requires `gh` authentication with access to Actions. The request returns
immediately; find the run with `gh run list --repo arocomputer/e --workflow preview.yml`.
The installer verifies the artifact checksum and prints its source commit.
It installs `e-pr-123` under `~/.local/bin`, or `E_INSTALL_DIR`.
Artifacts expire after 14 days. The selected run stays pinned even if the PR
changes later; request a new run to test new commits. PR code is unreviewed and
can execute arbitrary code when built or run. Its workflow uses read-only
permissions, no publishing credentials, and no persisted checkout token.

## Ship production

1. Prepare the next base version in `Cargo.toml` and `Cargo.lock` on main.
2. Review the release notes, move Unreleased into `## X.Y.Z`, and add its date,
   title, introduction, and fixed groups. Create a fresh Unreleased section.
3. Qualify the final commit with `./x check` and `./x release-check vX.Y.Z`.
   Create and push the production tag:

   ```sh
   git tag -a vX.Y.Z -m "Release X.Y.Z"
   git push origin vX.Y.Z
   ```

Production recompiles under the production identity, runs `./x check` and
`./x ui`, then builds all four platform archives and publishes the installers.
No beta prerequisite and no permanent release branch are required.

## Release notes and the website

Use `### New features`, `### Improvements`, and `### Fixes`, in that order;
omit empty groups. Keep `## X.Y.Z` exact for extraction. Put the date below it,
then one `###` release title and a short introduction. Bullets may wrap across
lines. Use inline code and bold for emphasis. Put **Upgrade:** instructions
first under Improvements and **Security:** fixes under Fixes.

The GitHub body comes from that version's section verbatim. The workflow
exports the same content as `release.json`, with its version and source commit.
The website reads that asset and GitHub's publication date, refreshing every
five minutes. It excludes drafts, prereleases, and Unreleased.
The first historical release predates the asset and remains a checked-in website
entry. New releases need no separate website copy or deployment.

## Verification and retrying publication

Release builds use the committed lockfile. Each build has four archives,
`checksums.txt`, a CycloneDX SBOM, and GitHub build-provenance attestations.
The workflow smoke-tests native binaries and the shell installer before
publication, and tests pinned and channel installs through the public website.
Actions retains build archives and metadata for 14 days.

The Linux legs build in `rust:1.98-bullseye` (Debian 11, glibc 2.31) and the
macOS legs on `macos-latest`; `scripts/release/build.sh` does both. A Linux
binary links against the glibc of the image that built it, so that image is the
floor every release inherits — Debian 11+, Ubuntu 22.04+, RHEL 9+ — and the
script refuses to publish a binary that requires anything newer, which the
`glibc` job proves on the pull request. Change the image and the
ceiling (`E_GLIBC_CEILING`, and the refusal in `install.sh`) move together.

```sh
sha256sum -c checksums.txt --ignore-missing
gh attestation verify e-x86_64-unknown-linux-gnu.tar.gz --repo arocomputer/e
```

On macOS use `shasum -a 256`.

To retry a partial package publication, run Release
with action `retry` and the existing version tag. This downloads the existing
archives instead of rebuilding. For a draft whose builds finished, retry regenerates
checksums, the SBOM, and provenance in an isolated temporary directory, then
publishes the draft before updating packages. Incomplete drafts fail until all
four archives exist. npm compares the existing package's integrity;
a different tarball under the same version fails. The Slack bot keeps its own
version, which must change whenever any packaged file changes, including its
README. New bot versions publish under `latest` with production application
releases. Each registry can fail
independently; rerun failed publication after recovery. There is no transaction
across registries.

```sh
python3 -m unittest discover -s scripts/release -p 'test_*.py'
python3 -m unittest discover -s scripts/packaging -p 'test_*.py'
node --test scripts/packaging/publish-npm.test.mjs
scripts/packaging/smoke.sh
```

PR CI runs installer checks only when packaging, installer, identity, updater,
or workflow sources change. Release installation checks always run. The npm
smoke check retries both installation and the executable version check six
times, twenty seconds apart, with a fresh prefix and cache each time. This
covers delayed wrapper metadata and missing optional platform packages. Preview
builds require an explicit request.

## Publishing credentials

Homebrew uses `HOMEBREW_TAP_TOKEN`, a fine-grained token limited to Contents
read/write on `arocomputer/homebrew-tap`. Deploy keys are disabled by repository
policy. Renew the token before its expiry.
npm uses trusted publishing (OIDC); no npm token is stored. Each of the six
packages (`e` and its four platform packages, plus `e-slack`) trusts the GitHub
organization `arocomputer`, repository `e`, workflow `release.yml`, with direct
publishing allowed. Use Node 24 with npm 11.5.1 or newer.

Trusted publishing can only be attached to a package that already exists, so the
first publication under a new scope needs another auth method — an interactive
`npm login` with 2FA, or a temporary publishing token. After that first publish,
configure the trust relationships with an interactive npm login (2FA enabled,
npm 11.15.0 or newer):

```sh
for package in e e-darwin-arm64 e-darwin-x64 e-linux-arm64 e-linux-x64 e-slack; do
  npm trust github "@arocomputer/$package" \
    --repository arocomputer/e --file release.yml --allow-publish --yes
  sleep 2
done
```

Complete npm's browser authentication when prompted. An API token that bypasses
2FA cannot configure trust relationships. Verify each package with `npm trust
list @arocomputer/<package>`. From then on the release workflow authenticates
with OIDC and no token is needed.

The website renders this repository's `docs/guides/`, so
`.github/workflows/docs.yml` starts arocomputer/web's Deploy workflow whenever a
guide reaches `main`. It uses `WEB_DEPLOY_TOKEN`, a fine-grained token limited
to arocomputer/web with Actions: write. Set the secret once; without it the job
fails loudly rather than going stale silently.

The application does not publish to crates.io. Its installers are the shell
script, Homebrew, and the npm packages. The one crate that publishes is the
embedded SDK (`aro-e-sdk`), which versions itself and is the only way to embed e
in a Rust program. Publishing it uses `CARGO_REGISTRY_TOKEN`, a token scoped to
`aro-e-sdk` and `aro-e-core` (the SDK depends on it) and no others. Create the
token at https://crates.io/settings/tokens and set the first publication up
interactively with `cargo login` if it is rotated.

The five `aro-e*` crates were published once at `0.0.1` and yanked when the
application left crates.io; only `aro-e-sdk` publishes going forward.

The website installer at `https://e.intuitum.sh/install.sh` serves the maintained
script from main with a five-minute cache. No separate deployment is required
for each binary release. The production homepage installation stays unchanged.

## Deployment history

GitHub's Deployments panel tracks `production`. The
release workflow records its selected source commit, not the branch used to run
the workflow.

A final reporting job marks production successful after npm, Homebrew,
website installation checks, and package publication pass. Failed or cancelled
attempts link to Actions logs. The run summary lists each
distribution result separately, so an incomplete package publication does not
hide a working direct download. Successful
production entries link to their release repository. Documentation and artwork-only
skips and invalid release selections do not create deployment entries. Reporting
starts with runs using this workflow; earlier releases are not backfilled.

npm can accept an upload before its registry metadata becomes available. The
publisher waits up to twenty minutes per package, then checks its tarball
integrity. It never republishes an accepted upload in the same attempt. A rerun
recognizes an already staged version and resumes waiting. A processing timeout
means availability is unconfirmed; check npm package status and rerun failed jobs.
Authentication errors and checksum mismatches fail immediately. The npm job allows
110 minutes for the application packages, optional bot publication, and
installation verification.
