# Compatibility

e is still pre-1.0. This page names the surfaces users can persist or build
against so changes to them are deliberate rather than accidental.

## Supported contracts

- **CLI:** documented commands and exit statuses are user-facing. Before 1.0,
  incompatible changes require a changelog entry and migration guidance.
- **Sessions:** JSONL headers carry `format_version`. Version 0 (the unmarked
  pre-release format) and version 1 are readable. Readers reject a newer
  version with an actionable error instead of guessing.
- **Configuration:** writes to `settings.json`, `auth.json`, and `trust.json` carry
  `format_version: 1`. Readers accept unversioned files, preserve unknown
  keys, and quarantine corrupt input before creating a replacement. An older
  e will not write over a file carrying a newer or invalid format version.
- **Extensions:** the JSONL protocol is versioned independently. e sends its
  protocol number during `initialize`; additive fields do not change the
  number, while incompatible wire changes require a new protocol version.
  Version 1 is documented in [extensions.md](extensions.md).

CLI one-shot commands return 0 after completing their requested operation, 1
for an operational/provider failure, and 2 for invalid arguments or an unknown
requested resource. `e doctor` is a local-only diagnostic command and returns
0 after producing its report; it never turns provider reachability into a
network side effect.

Compatibility fixtures under `tests/fixtures/` are release artifacts in
source form. Once committed for a release, they are not rewritten: newer
readers must continue to load them or intentionally document the migration.
Regenerable caches such as `models-store.json` are internal and are not a
persisted compatibility contract.

## Not a supported contract

The Cargo library target lets the binary, the integration tests, and the
`sdk/` package share code. Its public Rust items are not a stable third-party
API in themselves. The supported Rust SDK is the separate `e-sdk` package in
`sdk/` (see [sdk.md](sdk.md) and
[decisions/0002-rust-sdk-package.md](decisions/0002-rust-sdk-package.md)): the
API it consumes becomes its documented contract, and its first release
declares the semantic-versioning policy. Until then its surface is unstable.

## Change process

Changes to a supported contract need all of the following in one pull request:

1. a compatibility fixture or contract test;
2. migration behavior for existing user data or extensions;
3. documentation and a changelog entry;
4. an architecture decision when the change is difficult to reverse.
