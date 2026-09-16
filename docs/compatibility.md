# Compatibility

e is still pre-1.0. This page names the surfaces users can persist or build
against so changes to them are deliberate rather than accidental.

## Supported contracts

- **CLI:** documented commands and exit statuses are user-facing. Before 1.0,
  incompatible changes require a changelog entry and migration guidance.
- **Sessions:** JSONL headers carry `format_version`. Version 0 (the unmarked
  pre-release format), version 1, and version 2 are readable. Version 2 keeps
  response provenance and disjoint usage in an envelope outside replayable
  message content. Readers reject a newer version with an actionable error
  instead of guessing.
- **Configuration:** writes to `settings.json`, `auth.json`, and `trust.json` carry
  `format_version: 1`. Readers accept unversioned files, preserve unknown
  keys, and quarantine corrupt input before creating a replacement. An older
  e will not write over a file carrying a newer or invalid format version.
- **Layout:** `~/.e/layout.json` (`panes`, `split_min`, `focus`, `banner`,
  `status.left`, `status.right`) is documented in [layout.md](layout.md);
  unknown keys are ignored and a malformed file falls back to the
  defaults.
- **Packages:** the `packages` list in `settings.json` holds source strings
  as typed (`npm:name[@version]`, `git:host/user/repo[@ref]`, a git URL, or
  a directory path), or objects carrying a `source` plus per-kind filter
  lists (`extensions`, `skills`, `prompts`, `themes`); npm packages live
  under `~/.e/packages/npm/node_modules/<name>`, git packages under
  `~/.e/packages/<host>/<path>`. All are documented in
  [packages.md](packages.md) and pinned by
  `tests/fixtures/config/settings-v1-packages.json`; a reader that meets an
  entry it cannot parse reports it and loads the rest.
- **`e rpc`:** the headless session protocol reports `protocol: 2` in
  `hello`. A line without `method` is the version-1 one-shot request and
  keeps its flat response. Methods, parameters, result fields, and the
  `session`/`request` tags on event lines are a supported contract once
  documented in [automation.md](automation.md); new methods and fields are
  additive and do not change the number, a change to an existing shape
  does. `tests/fixtures/rpc/v2-requests.jsonl` pins the request shapes.
- **Extensions:** the JSONL protocol is versioned independently. e sends its
  protocol number during `initialize`; additive fields do not change the
  number, while incompatible wire changes require a new protocol version.
  Version 1 is documented in [extensions.md](extensions.md). The families
  beyond it (`events`, `hooks`, `display`, `ui`, `session`, `shortcuts`)
  are additive: each is advertised in `capabilities`, declared in the
  manifest, or initiated by the extension, so a version-1 extension is never
  sent a message it did not ask for. A method name, event name, field, or
  result shape in those families is a supported contract once documented.

CLI one-shot commands return 0 after completing their requested operation, 1
for an operational/provider failure, and 2 for invalid arguments or an unknown
requested resource. `e doctor` is a local-only diagnostic command and returns
0 after producing its report; it never turns provider reachability into a
network side effect.

Compatibility fixtures under `tests/fixtures/` are release artifacts in
source form. Once committed for a release, they are not rewritten: newer
readers must continue to load them or intentionally document the migration.
Regenerable caches such as `models-store.json` and `models-dev.json` are
internal and are not a
persisted compatibility contract.

Session sidecars now use OS-held locks. Stop older e processes before
resuming their sessions with the new writer; mixed PID-lock and OS-lock
writers must not open the same session concurrently. Existing JSONL needs
no migration. Empty `.lock` sidecars are expected and should not be deleted.

Provider failure diagnostics use separate `<session-stem>.errors.jsonl` files,
leaving message logs readable across their supported versions. These sidecars
carry their own `format_version: 1` and link records to message IDs. They can be
removed without changing conversation history. Headless responses add an
optional `error_details` object while retaining the `error` string.

On Unix, e creates its state directories with `0700` and session logs with
`0600`. Configuration writes and session creation or reopening also tighten
the e home directory to `0700`, protecting older files underneath it without
rewriting their contents. Reopening an older session sets its file to `0600`.
Stricter owner permissions are preserved, including read-only directories.
Stable uses `~/.e`, dev/local uses `~/.e-dev`, beta uses `~/.e-beta`, and PR
builds use `~/.e-pr/COMMIT`. `E_HOME` overrides the channel default.
Use a dedicated directory for `E_HOME`; it is private application state, not a
shared workspace. Files copied outside that directory are not migrated.
Credential staging files start at `0600`, before any secret is written.

Provider and OAuth endpoints must be final URLs: authenticated requests no
longer follow HTTP redirects, including same-origin redirects. Update any
custom gateway URL that relied on one. Release asset downloads still follow
redirects, without provider credentials, and reject HTTPS-to-HTTP downgrades.

Filesystem `write` and `edit` stage and sync content before committing it.
Existing files are updated through their original inode, preserving symlink
targets, hard-link aliases, ACLs, and extended attributes. Staging failures
leave the original intact; an I/O failure during the in-place copy can leave
a partial update. New files are published without overwriting a concurrent
creator. On Unix, the parent directory is synced before success is reported.
Unix writes also check that the target still names the opened inode before
and after copying. A detected external replacement fails the write so the
caller can reread and retry; external writers still need their own coordination.

Tool integer arguments accept JSON unsigned integers, integral JSON floats below
2^64, and decimal integer strings within the u64 range. Out-of-range values fail
validation instead of saturating. A read line larger than the output window is
reported as an error with an offset to skip it, never as a complete truncated
line. Files and saved sessions need no migration.

On filesystems without hard links, a failed new-file copy removes its partial
target when it still identifies the created file. Freshness checks allow a
confirmed deletion but fail closed on other metadata errors.

## Not a supported contract

The Cargo library target lets the binary, the integration tests, and the
`sdk/` package share code. Its public Rust items are not a stable third-party
API in themselves. The supported Rust SDK is the separate `intuitums-e-sdk` crate in
`sdk/` (see [sdk.md](sdk.md)): the API it consumes is its documented contract,
and it follows semantic versioning from its first published release — before
1.0, a breaking change moves the minor version and is named in the changelog.

## Change process

Changes to a supported contract need all of the following in one pull request:

1. a compatibility fixture or contract test;
2. migration behavior for existing user data or extensions;
3. documentation and a changelog entry;
4. updated contract documentation in the relevant guide.
