# Changelog

All notable changes. Nothing has been cut as a release yet — every entry
lives under `## Unreleased` until e's first deliberate release. To cut one:
rename `## Unreleased` to `## X.Y.Z — <date>` (the release workflow's
release notes are that section verbatim), open a fresh empty
`## Unreleased` above it, tag `vX.Y.Z`, and the pipeline publishes.

## Unreleased

- Launch and automation have one tested contract. Canonical flags keep their
  descriptive names and gain compact aliases: `--no-extensions`/`--ne`,
  `--no-save`/`--ns`, `--read-only`/`--ro`, `--no-tools`/`--nt`, plus
  process-scoped `--model`/`-m`, `--effort`/`--ef`, repeatable
  `--image`/`-i`, and `--json`/`-j`. Restricted modes filter schemas and
  independently block execution; read-only mode never dispatches an
  extension override masquerading as `read`. `e ask --json` returns final
  output, errors, warnings, usage, cost, and tool counts, while `e rpc`
  provides the same result as a persistent one-request/one-response JSONL
  protocol that defaults to memory-only turns.
- Provider handling is split into explicit definitions, credential runtime,
  catalog strategy, inference dialect, and per-model profile. Auth is
  resolved/refreshed once before an adapter runs; live catalog shape is no
  longer inferred from the inference dialect. Providers declare a
  native/compatible/experimental support tier, and `e doctor` / `e providers`
  produce paste-safe text or JSON diagnostics without credential values.
  The deterministic dialect harness now requires semantic tool-call
  start/argument/end identity (including interleaved calls), and an ignored
  `E_LIVE_MODEL` canary verifies a paid end-to-end tool loop on demand.
- Model profiles can declare tool and image capabilities plus optional USD
  rates per million input/output/cache-read tokens. Cost estimates appear in
  turn trailers and machine results without double-charging cached input.
  PNG, JPEG, GIF, and WebP attachments are magic-byte checked, bounded,
  retained in the session, and translated through all four native dialect
  request shapes.
- The extension protocol advertises an additive `tool.update` capability for
  correlated stdout/stderr chunks while retaining strict protocol-v1 startup
  compatibility;
  extension tools now stream through the same ordered ToolOutput lifecycle as
  built-ins. The dependency-free scaffold exposes an `update()` callback,
  and worked `mcp.mjs` and `subagent.mjs` examples provide MCP stdio tools and
  bounded delegated e turns without expanding the harness core.
- Loading a branched session now follows parent links from the most recently
  appended head. Resume no longer replays an abandoned branch merely because
  its records remain earlier in the append-only file; missing parents,
  duplicate ids, and cycles are reported as corruption.

- Sessions are a tree, not just a line: every message carries an id and its
  parent, in the same file — `/tree` lists this session's earlier user
  turns, and rewinding to one points the next message's parent at it instead
  of the file's last line, growing a second branch beside the abandoned tail
  rather than overwriting it. A plain, never-rewound session still reads
  exactly like the straight line it always was, and a session written before
  this existed resumes correctly — its untagged records get an id and
  parent synthesized from their position on load.
- `bash` can start something long-lived without blocking the turn: pass
  `background: true` to get a handle back immediately instead of waiting for
  the command to exit. A later call with that `handle` (no `command`) reads
  its status and output so far; add `signal: "kill"` to stop it. The process
  outlives the call that started it but not the e process itself.
- `grep` takes an optional `glob` to restrict which files it searches (e.g.
  `*.rs`, `src/**/*.json`) — the filtering half of the old `find` tool,
  recovered as a parameter instead of a second tool.
- The composer's editing keys are file-backed: `~/.e/keybindings.json`
  overrides any chord (`ctrl+w`, `alt+enter`, …) to a named action, or to
  `"none"` to unbind it, the same override-a-default pattern as themes.
  e's application-level shortcuts (ctrl+c, ctrl+p, tab, menu navigation) are
  claimed earlier in the key dispatch and are not affected.

- The built-in tool surface is the reference design's four plus grep: read ·
  write · edit · grep · bash. `ls` and `find` are gone — bash covers both
  without a schema riding along in every request — and the dedicated `skill`
  tool is gone with them: the system-prompt catalog now advertises each
  skill's name, description, and SKILL.md path, and the model pages a body
  in with the ordinary `read` tool (the reference's progressive disclosure).
  Fixes riding along: the `$` picker's injected skill now carries the same
  skill-directory hint the tool path had, so a body that says "see
  reference.md" no longer strands the model only when a human invoked it;
  and a frontmatter `description:` may span lines (block scalars, indented
  continuations) instead of silently truncating at the first newline.
- Repository hardening: persisted sessions and configuration now declare
  format versions backed by retained compatibility fixtures; `e doctor`
  provides redacted, local-only diagnostics; Rust and repository
  checks are pinned behind `./x`; CI adds property tests, scheduled fuzzing,
  performance budgets, path triage, and downloadable PR binaries; releases
  qualify tags and changelogs, use locked builds, smoke-test installation,
  and publish a CycloneDX SBOM with signed build provenance. Architecture,
  compatibility, release verification, review, support, and first-run safety
  documentation now state the maintained contracts explicitly.

- Internal simplification, no behavior change: message commits go through
  one `TurnLog` handle instead of six threaded parameters; every
  file-scanning tool shares one traversal (same skip rules, same cap
  semantics); each tool's
  schema, runner, system-prompt snippet, and transcript labels live in one
  table so they cannot drift apart; tool cancellation uses one shared
  helper; the TUI's session-event handling, menus, and login flows moved
  into their own modules under `src/tui/app/`; the compact and toolloop
  tests use the shared `tests/common` fixtures; a stray `screen.rs.bak`
  was removed. A `CLAUDE.md` pointer and per-area test recipes in
  `AGENTS.md` were added for agents working on e.

- Tool-call assembly is visible while it streams: the one long stream phase
  with no text — a model writing a large `write` call generates argument
  JSON for tens of seconds — used to freeze the transcript and the token
  counter while the turn was alive. Dialects now emit argument-delta
  progress, the activity row gets its own "Writing tool call (Ns) (↑ ↓)"
  phase, and the argument bytes tick the live output estimate.
- The live token estimate resets at every real usage report, so the
  in-flight display is real counted tokens plus only the current step's
  chars/4 delta — never an estimate stacked on tokens already counted.
- A resumed session seeds the context gauge from its restored history
  instead of showing an empty context until the first usage report.
- `/new` and `/resume` also refuse during the brief gap between a submit
  and its TurnStart event, closing a race that could swap session state
  under a just-started turn.

- Turn token accounting is honest: the trailer's ↑ used to sum every step's
  full context (a 20-step turn over a 100k context showed ↑2000k); it now
  shows the request size (latest wins) while ↓ keeps accumulating what each
  step actually generated. The agent emits exactly one Usage event per step
  — the stream's final frame — so dialects that report usage cumulatively
  mid-stream can't be double-counted either.
- Tool results stop echoing content the model just wrote: `write` and
  `edit` return a one-line confirmation to the model (the full diff moves
  to the ctrl+o detail viewer via a new display channel on `ToolOutput`),
  ending the pattern where every written file was billed into the context
  twice and then re-sent with every later request.
- `bash` keeps the *tail* of over-long output instead of the head — the
  verdict of a compile or test run lives at the end — with the truncation
  marker up front; ANSI escape sequences are stripped from the model's copy
  and carriage-return progress bars collapse to their final frame.
  `sanitize_display` now removes whole CSI/OSC sequences instead of leaving
  `]0;…`/`[31m` fragments behind.
- `read` returns line-numbered lines (`N<tab>…`), so the model can
  correlate compiler line numbers with file content and knows where a
  windowed read sits; `edit`'s description warns the prefix is not file
  text.
- Silent caps now speak: `grep` marks a result stopped at its 200-match cap
  (`200+ matches`) and documents its traversal skips; `ls` output is
  bounded like every other tool; broken tool-argument JSON is reported as
  invalid JSON instead of a misleading "missing <param>".
- New `find` tool: filename search with `*`/`?`/`**` globs, grep's
  traversal rules, and a capped-result marker — locating a file by name no
  longer needs bash gymnastics.
- `edit` and `write` fail with "changed on disk" when the target changed
  since e last read or wrote it (a user's editor, a bash `sed -i`), instead
  of silently clobbering the newer copy from a stale read.
- Anthropic prompt caching actually covers the conversation: a moving
  `cache_control` breakpoint rides the last message, so each tool-loop step
  extends the previous step's cached prefix instead of re-billing the whole
  history uncached; `max_tokens` (and the manual thinking budget) clamp to
  the model's declared window.
- The system prompt carries an environment tail — platform and today's UTC
  date — so the model stops guessing dates from its training cutoff; the
  `skill` tool result names the skill's directory so bodies can reference
  their bundled files.
- Turn robustness: text the user watched stream is committed to history
  when a stream errors or Esc lands mid-reply (calls that never ran get an
  honest synthetic result, keeping the history replayable); a blank
  successful stream gets one quiet re-request before the error; a
  provider's `Retry-After` is honored up to 60s instead of being clamped to
  30s and burning attempts; a 256-step backstop stops runaway tool loops.
- Mid-turn context guard: when real usage crosses the compaction threshold
  inside a tool loop, the turn ends cleanly with a queued continuation
  instead of dying on a rejected over-window request — the frontend
  compacts between turns as before, then resumes the task.
- Compaction thresholds scale with the window (reserve an eighth, keep a
  quarter, both bounded at the reference values) so a 32k local model no
  longer compacts at half its context or keeps more than the whole window;
  the summarize request budgets its flattened transcript to half the
  window, dropping the oldest messages with a marker, instead of
  overflowing at the exact moment compaction is needed.
- Session resume tolerates a torn final line (the artifact of a crash
  mid-append) by dropping that one record instead of refusing the whole
  session, and repairs a tail cut between a tool call and its result with a
  synthetic "not executed" result so the history replays on every dialect.
  Interior corruption stays a hard error.
- SECURITY.md states explicitly that directory trust gates context
  injection (AGENTS.md, repo skills/prompts), not execution.

- Gemini thought deltas count as a produced stream: a thinking-only turn
  that ends without text (MAX_TOKENS mid-thought) now ends with the
  truncation warning instead of a spurious "empty response" error, and a
  retryable failure after a long thinking phase is no longer replayed
  (which used to re-stream the thoughts on screen).
- The anthropic live model sync fetches `GET {base}/v1/models` — the
  declared base is the bare host, so the old `/models` path 404'd on every
  refresh and the live catalog never populated.
- Plain-key `responses` requests no longer carry the ChatGPT-backend
  `prompt_cache_key` body field; only the OAuth mount sends it.
- Syntax highlighting is linear per line instead of O(n²): long single-line
  tool output (minified JSON, log lines) no longer stalls input for
  seconds, with a coarse perf regression test pinning it.
- `FlagDecl` retains an extension's declared flag `default` instead of
  silently dropping it (the scaffold applies it; e's flags notification
  still carries only flags actually passed).

- The main-screen renderer no longer moves the cursor relatively: every
  paint diffs the visible window row-by-row against a shadow of what each
  screen row currently shows, and rewrites only the rows that changed, each
  starting from a `\r` plus an absolute position. No motion depends on where
  the cursor was left, so the pending-wrap rewind class (#123) is
  structurally gone rather than patched around. When the frame grows past
  the bottom of the screen, the display scrolls up just enough for the
  window to sit at the top rows and only the newly blank rows are painted —
  the transcript keeps flowing into the terminal's scrollback. A resize no
  longer sends `\x1b[2J` (no blank flash): the shadow is marked unknown and
  the next paint rewrites the window in place, clearing anything stale
  below a short frame.

- Theme detection is stdin-free: it reads only `COLORFGBG` (the
  rxvt/iTerm-style report) and falls back to dark, replacing the OSC 11
  probe entirely — reading its reply off stdin races whatever reader owns
  the terminal (audit #93), so there are no probe-window keystrokes to
  salvage in the first place.

- The tab title now tracks the session lifecycle: it launches as
  `𝑒 · <short path>`, switches to `𝑒 · <session name>` when a session is
  named (a rename command or tool), returns to the path on `/new`, and is
  cleared on exit so the terminal is left pristine. Title writes are
  guarded by a tty check so escape codes never land in a redirected pipe.
- The TUI draws a turn's reasoning live and keeps it on screen until the turn
  ends: thinking dims with the committed turn instead of vanishing while the
  reply streams, gated by a file-backed `show_thinking` setting (default on;)
  the ↓ token estimate always counts it either way. Every thinking segment of
  the turn dims at that moment — including pre-tool reasoning that a tool
  batch, retry, or steered message replaced with a fresh block. The tab title
  now shows a short path showcase — `~`-relative under `$HOME`, the last two
  components elsewhere — instead of the full absolute working directory.

- Bugbot review fixes: a failed session-name write at log creation no longer
  discards the freshly created session (the next commit would open a
  different file and strand in-memory history); compaction's keep-cut no
  longer separates an Anthropic signed thinking block from the assistant
  turn it precedes; and Google's live model refresh speaks the Gemini
  dialect (`x-goog-api-key`, `models[].name`) instead of an OpenAI-shaped
  `/models`, so signed-in Google users see more than the seed ids. A failed
  compaction-seed write now keeps the old session log attached instead of
  detaching first, so a crash resumes into the complete pre-compaction
  conversation rather than a fresh file holding only an unanchored tail.

- The audit backlog's five remaining epics are fixed:
  - *Sessions* — persistence failures (unwritable home, full disk) surface
    as a visible warning instead of silently losing history; /new and
    /resume are refused while a turn is running; a `-r` launch prompt
    waits for the session pick; the session name travels with session
    identity (cleared on /new, restored on resume); and the status line
    shows the live session name and queued prompt count.
  - *Providers* — Anthropic signed thinking blocks replay on tool loops;
    a body transport failure before output retries like a 503; usage
    reports the inclusive prompt total, ending premature auto-compaction
    on cached turns.
  - *Tools* — parallel edits to one file can no longer erase each other
    while both claim success; FIFOs and other non-regular files fail fast
    instead of hanging the turn with Esc inert; live bash output survives
    UTF-8 split across pipe reads; grep searches an explicitly requested
    dotfile.
  - *Extensions* — a child that stops reading stdin can't wedge hooks or
    quit; failed handshakes can't leave orphans; one-shot CLI paths send
    shutdown; duplicate tool names are rejected visibly; late command and
    shell results are discarded when the session changed under them;
    prompts queue behind a running `!` command and survive the TurnEnd
    race in order.
  - *TUI* — model output and extension notices render inert (no terminal
    control injection); one guard restores every terminal mode on every
    exit path; width math is display columns end to end (CJK, emoji);
    overlong tokens wrap instead of vanishing; the composer draws exactly
    one cursor; paste placeholders retire on submit; and keystrokes typed
    during the startup background probe reach the composer.

- Streaming no longer lags or hangs the TUI. The blink tick stops
  invalidating finished blocks' render caches (only a running tool row
  actually blinks), the statusline's sign-in and effort state is cached
  instead of read from disk every frame, painting moved to a dedicated
  thread with latest-wins coalescing so a slow terminal can't stall input
  handling, session events are drained in batches under a 33ms frame
  budget, and the OSC-11 background probe runs once at startup instead of
  blocking the loop on every theme change.

- Truncated, refused, or filtered replies are no longer silent successes:
  every dialect maps its stop/finish reason, the agent surfaces abnormal
  endings as visible warnings, and malformed SSE payloads are counted and
  reported instead of being dropped.

- Provider coverage: a native Gemini dialect (thought-signature replay on
  tool loops, thinking levels, safety/limit finishes), eight new API-key
  providers as data — Google, Groq, Mistral, DeepSeek, Cerebras,
  OpenRouter, Together, Fireworks — and keyless local backends (Ollama,
  LM Studio) whose models appear whenever the local server is running.

- The provider module is reorganized as `providers/{api,data}`: wire
  dialects under `api/`, provider definitions under `data/`.

- Audit fixes: session directories now use collision-resistant workspace keys
  while safely discovering legacy logs; provider SSE parsing preserves UTF-8
  across arbitrary byte chunks; live-discovered models inherit their
  provider's wire dialect; crashed extensions wake pending calls immediately;
  command-line prompts wait for the first-visit trust choice; and OAuth opens
  with `open` on macOS or `xdg-open` on Linux while always showing a copyable
  fallback URL.

- Flags are delivered to every extension that declares them, not just
  startup-hook ones: e sends a `flags` notification right after launch
  (`{"method":"flags","params":{"flags":{…}}}`), so a tool-only
  extension reads its flags in any handler.
  The scaffold adds `flag(name)` (the passed value, else the manifest's
  `default`, else undefined) and `flagPassed(name)` (true only when it
  was on the command line); typed flags may carry a `default` now. The
  worktree example uses a boolean flag with the branch
  read from argv.

- Flags become typed: an extension declares `"type":"boolean"` (default)
  or `"string"` on a flag, and e parses it from startup argv — booleans
  match `--name`, `--name=true|false`, `--no-name`; strings match
  `--name=value` or `--name value` (a following `-` token is never taken
  as a value). Parsed values ride `hook.startup`'s new `flags` params, so
  extensions read them without hand-scanning argv; display-only flag names
  (like `"-w, --worktree"`) are unchanged. `e --help` renders typed flags
  as `--name`/`--name <value>`. The scaffold gains a `flag()` getter and
  the worktree example moves onto typed flags.

- `docs/extensions/scaffold.mjs`: the extension protocol's shared
  plumbing — framing, id routing, a `connect({manifest, handlers})`
  that reads like an SDK with nothing to import but Node. The examples
  move onto it: `hello.mjs` (every surface in ~50 lines of handlers),
  new `gate.mjs` (the tool_call hook as a fail-open guard), and
  `worktree.mjs` (the -w launcher) all now go through it. A scaffold
  dropped into ~/.e/extensions/ by accident is a harmless no-op.

- The extension surface grows the pieces real extensions reach for:
  `input` hook (consume/rewrite a submitted line, fail-open, API keys
  never reach it), `session_name` from commands and tools (shown in
  /resume), a `flags` manifest entry surfaced in `e --help`, and a
  namespaced config key (`settings.json` `extensions.<name>`) delivered
  with every initialize. `e --help` now lists extension flags and
  commands; `/help` lists extension commands; `docs/extensions/`
  ships `hello.mjs` (every surface in ~90 lines) and `worktree.mjs` (a
  minimal -w launcher); docs/extensions.md documents the new results.

- Retries now show their work instead of a scrollback notice: a retryable
  failure (429/408/5xx, a network drop, a stalled request, or a provider
  error frame naming an outage or rate limit) replaces the Thinking row in
  place with the cause, a short reason, the backoff, and an attempt count
  (`Provider unavailable · 503 Service Unavailable · retrying in 4s ·
  attempt 3/10`), toned as a warning; the first content after a retry flashes
  a brief `✓ Recovered` before reverting. Backoff follows the reference
  client's shape — 250ms, 1s, 2s, 4s, 8s, 16s, then flat at a 30s ceiling —
  for up to 10 attempts before the turn fails with how long it tried.
  `Retry-After` is honored (capped at the same ceiling) when a provider
  sends one. Esc now cancels a retry wait immediately rather than after the
  full backoff, and every non-2xx status is classified by what it means for
  retrying (`FailureCause`) instead of the old binary Auth/Transient/Delivered
  split that only ever retried a bare connection failure.
- The last silent-stall window is closed: the wait for response headers is
  bounded by the same budget as the stream body (a provider that accepts the
  request but never answers now fails the turn visibly), and turn-path token
  refreshes carry a request timeout so a hung token endpoint cannot park a
  turn before the provider request even starts.
- Structure: `tui/` is grouped into `paint/` (SGR, screen, theme), `content/`
  (markdown, transcript, composer, statusline), `surfaces/` (footer panels),
  and `app/` (the interactive frame loop, moved out of `main.rs`). Short
  paths (`tui::theme`, …) still re-export. Provider dialects share one
  `Api::parse` and pull OAuth refresh from `auth::login` instead of inlining it.
- Composer typing feel: drafts word-wrap (a word that crosses the edge
  comes down whole instead of tearing mid-letter); ↑/↓ move between wrapped
  or multi-line rows, falling back to history recall at the edges; and the
  kitty keyboard protocol is enabled so shift+enter inserts a newline in
  supporting terminals (alt+enter remains the universal fallback).
- Sessions begin only with user intent. Merely opening and closing e creates no
  session, and `/resume` plus `-c` ignore header-only or assistant-only logs.
- Reference-grammar pass over the tool surface, from frame-by-frame comparison
  against the reference design's own recordings: provider batches group from
  first start (single calls included) at column zero, with stable child order
  and in-place gerund-to-terminal transitions; command pipes stream live only
  while a command owns focus and are withdrawn on completion, full output
  staying behind ctrl+o; batches execute concurrently with results committed
  in assistant source order; failures tally in the header and wear their own
  label; full workspace-relative targets clip with an explicit ellipsis; the
  focused marker blinks and activity no longer duplicates as a footer row;
  reasoning summaries are counted but never drawn; thinking shows token
  estimates from the first second; turns end with the reference's dim
  duration-and-tokens trailer while cancellation reads `● System:
  cancelled`; and tool results persist outcome metadata so resume rebuilds
  groups faithfully.
- Extensions may advertise a fatal `startup` hook that rewrites raw argv,
  adjusts environment variables, and requests a same-binary relaunch in a new
  cwd — enough to build managed Git worktree launches (`-w`) as plain
  extensions. Additional capabilities will be added to e's
  language-neutral line protocol when concrete extension needs arise.
- `/reload` replaces its `reloading…` notice with the completion text instead
  of appending a second transcript line.
- Turn endings hold: a stalled or broken provider stream no longer leaves the
  spinner running with Esc inert (cancel is checked while waiting on SSE, not
  only after the next byte); quiet sockets fail after 180s and incomplete EOF
  is an error, not a silent success; a failed turn closes with its error
  persisted in the transcript in error color below the duration trailer; retry
  notices carry an esc-to-cancel hint; and the thinking dot blinks presence —
  visible then hidden, no dim half-state.
- Repo-local resources: a trusted directory's `.e/skills/` and
  `.e/prompts/` load beside the global ones (same formats, same trust gate
  as AGENTS.md); a repo resource shadows a global of the same name.
- Shift+Tab cycles the reasoning effort through the levels the current model
  declares (low/medium/high today; xhigh and friends as models expose them);
  the choice persists in settings and shows in the status line.

- Release notes now carry the version's changelog section and install
  instructions; `e docs models` documents env-var credentials and the
  live catalog; ROADMAP records the 0.4 line.
- Picker order: /models groups models by provider (registry order, live
  additions inline with their provider); /scoped-models lists the scoped
  entries first; ctrl+x on the scoped picker resets the scope entirely.
- The Zen provider's id is `opencode-zen`, matching its display name and
  `opencode-go` — the two OpenCode gateways now read as a pair. Auth.json
  keys written under the old `opencode` id still sign in (read-only
  alias, the file is untouched); a saved `opencode/…` model slug falls
  back to the picker once, then persists under the new id.

- The catalog splits along its one real seam: `catalog/mod.rs` decides
  which models exist (registry projection, models.json overrides,
  resolution, scope) and `catalog/remote.rs` owns the live sync (the
  GET /models refresh, its cache, and the window-precedence overlay).
  External paths are unchanged.

- Providers are data, the reference architecture: each built-in lives in
  `src/core/provider/providers/<name>.json` — gateway, dialect, auth
  surface (which OAuth flow, which API-key env var), display name, seed
  models with per-model windows and effort support. The sign-in panels,
  /login dispatch, display names, and catalog all derive from the
  registry; adding an API-key provider is now a data edit. API keys fall
  back to their conventional environment variables (ANTHROPIC_API_KEY and
  friends) when auth.json has no entry — auth.json wins. Unknown dialects
  or OAuth flows in the data fail at startup, never on the wire.
  `scripts/generate-catalog.py` syncs the seed metadata from models.dev;
  this run corrected the gpt-5.x windows to 1,050,000, kimi-k3 to
  1,048,576, gave the OpenCode Go models real windows instead of a flat
  200K default, and trimmed Zen's minimax-m3 to 512K (compaction triggers
  earlier there). The seeds are fallbacks only: a provider's own reported
  context window always wins — the model chooses its window, not our
  tables.
- The model catalog is live, the reference behavior: every signed-in
  provider's own `GET /models` is fetched in the background (at launch
  and after each sign-in), cached in `~/.e/models-store.json` with a
  four-hour freshness window — a model a gateway ships today appears in
  `/models` today, no e release involved — and opening the picker asks
  the gateways again (60-second floor), popping new rows into the open
  picker the moment the answer lands. Windows the gateway reports
  (context_length and friends) are kept instead of the 200k default;
  non-chat ids (embeddings, audio, images, moderation) and dated aliases
  of listed models are filtered; refreshes are serialized in-process; and
  grok-build-0.1 leaves the built-ins. Built-ins and `models.json` always
  win a name clash; failures are silent.
- OpenCode Go and OpenCode Zen are two providers, as they actually are:
  `opencode-go` (the Go plan gateway, zen/go/v1) and `opencode` (the Zen
  gateway, zen/v1), each with its own sign-in row and models.
- Self-update: `e update` fetches the latest release for this platform,
  verifies its checksum, and swaps the binary atomically; the TUI does the
  same silently in the background at launch and notices "e X.Y.Z
  installed — /reload to switch to it now": with an update on disk,
  /reload exits through the normal cleanup and execs the new binary with
  -c, resuming the same session in place — no manual restart. The
  Auto-update setting in /settings opts out of the launch check only;
  `e update` always works. Dev builds (under `target/`) are always exempt, and
  `api.github.com`/`github.com` join the guard allowlist for it.
- The rest of the reference tool surfaces: command rows preview their
  output beneath the row (first four `│` lines, a `│ … N lines more
  (ctrl o to view)` elision, `│ exit code N` on failure); ctrl+o opens
  the full-detail viewer (scroll, ←/→ between outputs, the reference's
  footer wording) fed by every tool and `!` output; long or multiline
  pastes become `[Pasted text #N, L lines]` placeholders that expand on
  submit; and a tool interrupted mid-run wears the `■` cancelled glyph,
  tallied as `· N cancelled` in collapsed groups.
- Finished tool runs collapse into the reference group when the turn
  ends: a tallied header (`● 3 tool calls · 2 read · 1 command · 1
  failed`, with the reference's own pluralization) over `├` children and
  a `└` last — the dot-and-connectors shape. Live calls stay individual
  rows until then.
- Tool rows wear the reference grammar: a finished row is just the row —
  no "(done)" — and a failed tool turns its marker to the error token
  with a `│ <outcome>` continuation line (`│ exit 128`) beneath.
  Reasoning summaries render their inline markdown (**bold titles**, code
  spans) instead of showing literal asterisks.
- Installation: `install.sh` (curl-able, checksum-verified, macOS and
  Linux on both architectures) and a Homebrew tap, both fed by a release
  workflow that builds and publishes the four binaries on every version
  tag.
- Audit follow-up, control flow off prose: provider errors now carry a
  structured kind (auth / transient / delivered) instead of the retry
  decision matching message text like "no credentials"; login flows report
  a typed Outcome alongside their display notices instead of the frame
  parsing "signed in" strings. /copy uses OSC 52 — the terminal-native
  clipboard, no pbcopy, works over ssh. The HTTP client is pooled across
  requests instead of rebuilt per turn. Documentation now matches what the
  code does: tool execution is ungated (yolo); trust gates instructions,
  not tools.
- Slash commands match on a word boundary: /loginfoo no longer starts an
  API-key flow for a provider named "foo" — it falls through to the
  unknown-command notice, like any other typo.
- A pasted API key no longer lands in the composer's recall history,
  where up-arrow would recall it for the rest of the session; it goes to
  ~/.e/auth.json and nowhere else.
- The `!` passthrough sent its argument under the wrong key ("cmd" vs
  the tool's "command"), so every `!` command returned an argument error
  instead of running — found reviewing this stack; the smoke that should
  have caught it matched the typed text instead of real output.
- The bash tool's timeout is real: the advertised wall-clock bound was
  never enforced (a runaway command hung the agent forever). Commands now
  run as their own process group and the group is killed at the deadline;
  output pipes are drained while polling so large output cannot deadlock
  the wait. guard.sh gains core/tools/bash.rs as an audited unsafe site
  (setsid + kill).
- Release profile: thin LTO, one codegen unit, stripped symbols — a third
  off the binary (8.02 → 5.18 MiB) with speed unchanged. Unwinding stays
  on so a panicking tool remains a tool error, not a dead session.
- `benchmarks/`: a dependency-free suite for the numbers e's identity
  depends on — binary size, cold start, spawn-to-first-frame — with
  timestamped reports under `benchmarks/results/`. First baseline:
  8.02 MiB, 2.5 ms cold start, 9.0 ms to first frame.
- First-live-turn fixes: the Responses dialect sent tools in the
  chat-completions nested shape and every codex/OpenAI tool call 400'd
  ("Missing required parameter: tools[0].name") — tools are now flat, as
  the API requires, pinned by a wire test. The composer wraps long drafts
  onto extra rail rows (the reference shape) instead of scrolling one row,
  with the cursor on its visual row. The screen differ clips any overlong
  frame line instead of letting it wrap physically and desync the painter.
  Audit follow-up: Responses-dialect reasoning items are now captured and
  replayed verbatim ahead of the calls they produced — without this, the
  second step of every codex/OpenAI tool turn 400s; other dialects and
  compaction skip them. SIGTERM/SIGHUP now exit through the normal cleanup
  (terminal restored, extension host shut down) and a panic hook restores
  the terminal before reporting — a killed e no longer strands the shell
  in raw mode with a hidden cursor.
- `e -r` / `--resume` launches straight into the session picker (the
  reference behavior); `e -c` continues the directory's latest session as
  before. `e --help` documents the CLI surface.
- The home is now created lazily (the reference behavior): no seeded
  skeleton — directories appear the first time something is written, so a
  fresh boot-and-quit leaves no `~/.e` at all.
- `e docs [topic]`: the format guides ship inside the binary — extensions,
  themes, models, prompt-templates, skills, plus both built-in themes
  verbatim as starting points. The system prompt points the agent at them
  (gated: only when asked about e itself), so "write me an extension" gets
  the protocol right on the first try.
- Core restructured into domain folders (the reference tree's names):
  `agent/`, `provider/` (with the catalog), `auth/`, `config/`,
  `resources/` — pure moves, no behavior change.
- `/reload`: hot-reload without restarting the session — the extension
  host restarts (picking up new, changed, or removed extensions) and the
  theme re-resolves; skills, prompts, AGENTS.md, settings, and models.json
  are read fresh on every use already. Refused mid-turn and during
  compaction; prompts typed while reloading are held and submitted after.
- The OAuth callback page declares UTF-8 (the em-dash rendered as mojibake
  in the browser) and now wears e's look: the wordmark, the message, dim
  detail, light/dark via prefers-color-scheme.
- Scoped models, the reference workflow: `/models` (renamed from `/model`,
  which still works) lists every signed-in model; `/scoped-models` is a
  multi-select — Space toggles, no scope means everything, the first toggle
  narrows to just that model — persisted as `scoped_models` in settings.
  ctrl+p / ctrl+shift+p cycles through the scope (or all available models),
  wrapping, skipping signed-out entries, persisting each switch; the
  statusline is the feedback.
- The model picker shows only models from signed-in providers; `/model`
  resolution works on the same set, so a pick is always usable. The default
  model follows availability instead of sticking (a configured model whose
  provider is signed out falls back, with a notice). Signed out entirely, e
  says so at launch — "use /login" — the reference behavior; after a login,
  a stranded model switches to an available one automatically.
- OpenAI and Anthropic API keys: the key panel now lists OpenCode Go, xAI,
  OpenAI (platform responses dialect at api.openai.com), and Anthropic — a
  new Messages dialect (`core/anthropic.rs`) with streamed thinking, tool
  use, prompt caching, and effort mapped to a thinking budget. gpt-5.x and
  claude-\* models join the catalog with their real context windows.
  `api.anthropic.com` joins the guard allowlist.
- xAI support: sign in with a SuperGrok / X Premium subscription (device
  code — a code to confirm in the browser) or an API key; grok-4.6,
  grok-4.3, and grok-build-0.1 join the catalog with their real context
  windows. Access tokens refresh lazily. `auth.x.ai` and `api.x.ai` join
  the guard's network allowlist.
- The sign-in flow now has a provider step per method, labeled with
  display names: OpenAI Codex, OpenCode Go, xAI.
- `!<cmd>` runs a shell command directly; the output shows in the
  transcript and is recorded into history, so the model sees what you did.
  The composer rail turns the `bashMode` theme color (green) the moment a
  draft starts with `!` — the reference convention: color, not words — and
  the finished block renders `$ cmd` in the same color, output muted.
- `e ask "prompt"` — one full agent turn without the TUI: styled output on
  a terminal, plain streaming text when piped. The session is saved, so
  `e -c` continues it.
- Prompt templates: `~/.e/prompts/<name>.md` becomes `/name` in the picker,
  with frontmatter `description` / `argument-hint` and bash-style argument
  substitution (`$1`, `$@`, `${1:-default}`, `${@:2}`).
- Trust: the first visit to a directory asks once whether e may load its
  AGENTS.md; declined directories still work but their instructions stay
  out of context (`/trust` grants later). Remembered in `~/.e/trust.json`.
- `~/.e/models.json` entries can declare a `context_window` (per model, or
  as a provider default) — compaction and the statusline follow the active
  model's real window. A file entry with a built-in's name now replaces the
  built-in, the same file-wins rule as themes.
- `/compact`: summarizes the older part of the session and continues in a
  fresh session file, keeping roughly the most recent 20k tokens of
  messages verbatim (the cut never separates a tool result from its call).
  Compaction only runs between turns — a mid-turn `/compact` defers to the
  end of the turn — and it also triggers automatically when real context
  usage crosses the model's window minus a 16k reserve. Messages typed
  while compacting are held and submitted after the swap; the old session
  stays fully resumable under `/resume`.
- Dependencies current: rand 0.10, sha2 0.11, base64 0.23, crossterm 0.29,
  pulldown-cmark 0.13. The parity suite pins the rendered output, so the
  markdown and terminal bumps are verified byte-for-byte.
- Open-source hardening: CI (fmt, clippy, tests on Linux + macOS),
  `scripts/guard.sh` — a security-surface audit pinning the allowed network
  hosts, the sovereign `~/.e/` home, store-only credential writes, the one
  `unsafe` file, and SHA-pinned workflow actions. CONTRIBUTING.md,
  SECURITY.md, CODEOWNERS, issue/PR templates, dependabot, weekly
  `cargo audit`, and branch protection on `main`.
- The codebase is now rustfmt-formatted and clippy-clean; both are CI gates.
- **Extension API** (`src/core/api/`): executables in `~/.e/extensions/`
  run as long-lived subprocesses speaking a JSONL line protocol — custom
  tools (overriding built-ins by name), slash commands in the `/` picker,
  a `tool_call` gate hook (fail-open), `turn_end` events, and transcript
  notices. Protocol reference and a worked shell example in
  [docs/extensions.md](docs/extensions.md).
- **Editable themes**: `~/.e/themes/<name>.json` appears in `/settings`;
  a user file wins over the built-in for the same name.
- **Non-destructive config**: every settings/auth write is read-merge-write
  with an atomic rename — unknown keys survive, corrupt files are
  quarantined (`.corrupt-<ms>`), never overwritten.
- Steering fix: a message typed mid-turn is held and folded into the
  running turn (it was being rejected with a notice).
- The harness, rewritten in Rust from scratch: own agent loop (request →
  stream → tools → repeat), steering, delivery-aware retry, one ordered
  session event stream.
- Two wire dialects (chat-completions SSE, responses) with API-key and
  OAuth/PKCE sign-in; `/login` flow with account-or-key choice.
- Built-in tools: read · write · edit · ls · grep · bash · skill.
- JSONL sessions under `~/.e/sessions/<cwd-slug>/`; `-c` and `/resume`.
- Context: system prompt (overridable via `settings.json`), global and
  project `AGENTS.md`, skills catalog.
- The full reference-shape TUI in Rust: line-differ renderer, markdown, code
  panels, pickers, settings, auth panel — pinned by the parity suite.
- The TypeScript TUI that defined the look: the byte-for-byte parity suite
  that still governs it shipped here.
