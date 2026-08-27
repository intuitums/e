# Releases and verification

A tag `vX.Y.Z` is publishable only when it exactly matches the user-facing
`VERSION` in `src/lib.rs`, has a dated `CHANGELOG.md` section, and passes the
complete repository contract. `Cargo.toml` deliberately stays at `0.0.0`
while dogfood builds identify themselves as `dogfood`; a release changes the
user-facing identity to `X.Y.Z` before tagging.
Release jobs build with the committed lockfile and smoke-test the native
binary and installer before publication.

Each release contains four binary archives, `checksums.txt`, and a CycloneDX
`e-sbom.cdx.json`. GitHub generates signed build-provenance attestations for
the artifacts. Given a downloaded archive:

```sh
sha256sum -c checksums.txt --ignore-missing
gh attestation verify e-x86_64-unknown-linux-gnu.tar.gz \
  --repo intuitumxyz/e
```

On macOS use `shasum -a 256` to compare the archive with the corresponding
line in `checksums.txt`. A checksum detects corruption; provenance verifies
that GitHub Actions built the artifact from this repository's release
workflow.

Maintainers can qualify a candidate locally with:

```sh
./x check
./x release-check vX.Y.Z
```
