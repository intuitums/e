# Changelog

All notable changes. To cut a release: rename `## Unreleased` to
`## X.Y.Z — <date>` (the release workflow's release notes are that section
verbatim), open a fresh empty `## Unreleased` above it, tag `vX.Y.Z`, and
the pipeline publishes.

## Unreleased

- The base system prompt's guideline set now matches the reference design's
  exactly — "Be concise in your responses" and "Show file paths clearly when
  working with files" — dropping the two e-specific additions (the
  small-focused-changes and stop-when-done lines). Everything else in the
  prompt already tracked the reference; the tools list stays e's real toolset,
  the self-docs section stays `e docs`, and the cwd/platform/date grounding
  stays.
- `e add <path>` installs a local extension file into `~/.e/extensions/`,
  makes it executable, and seeds `scaffold.mjs` beside it — so an extension's
  `import "./scaffold.mjs"` resolves with nothing for the author to place by
  hand. The scaffold is embedded in the binary (the copy always matches your
  e) and seeded non-executable, so the host skips it while extensions import
  it. Local files only for now; remote sources (git/https) are a later,
  trust-gated addition.
- `e rpc` gains two generic per-request knobs so a delegated turn can be
  shaped without the core binary learning any new concept: `system` (extra
  system-prompt text, appended to the base) and `tools` (a positive built-in
  allowlist, advertised and enforced). The `subagent.mjs` example builds its
  agents entirely on top of these — `Explore` (a light-model, read-only
  scout), `Plan` (a read-only strategist), and `Build` (a full-access
  worker) — defined right in the extension and composed into the request. No
  separate agents directory: an extension is where a user's additions live,
  and core never learns what an "agent" is.
- The `e rpc` response gains a `session` field: the saved turn's JSONL path
  (or `null` for a memory-only run). A delegated turn's whole transcript —
  every tool call and its output — lives there, so the dispatching agent can
  read the full turn when it needs more than the final answer, not just the
  summary the tool returns.
- Read-only tool mode is removed. `--read-only`/`--ro`, the `read_only`
  RPC `tool_mode`, and the `read_only_notice` override are gone; `ToolMode`
  is now `all` or `none`, with `--no-tools` the one deliberate opt-out. A
  coding agent acts — a half-disabled toolset was ceremony, not a use.
- The `e ask` headless subcommand is removed. Headless automation goes
  through `e rpc` (JSONL in, one object out per line); piped stdin with no
  terminal is now a usage error that points at `e rpc` rather than running
  a one-shot. The `subagent.mjs` example extension is rewritten to drive a
  single-shot `e rpc` child instead of shelling out to `e ask`.
- The `ask` tool is retired outright, not moved. A coding agent decides
  and acts; it does not interview its user. The core loses the ask schema,
  the answer registry, the read-only special case, and the question panel,
  and the extensions protocol does not grow a replacement: an extension
  that needs the person at the keyboard gets the answer from configuration
  under `~/.e/` or fails loudly and is steered, like any other missing
  input. If a question capability ever earns its way back in, it does so
  by need, with a use in hand.
- The queued-prompt review no longer pauses the queue or holds the turn
  open. Pending prompts are keyed; the review edits by key, so an entry
  the turn already steered commits as a fresh prompt instead of being
  resurrected, and `pause_queue`/`resume_queue` and the turn loop's
  hold-open wait are gone.
- The composer's paste-collapse threshold is a setting:
  `paste_placeholder` in `~/.e/settings.json` (codepoints, default 1000,
  `0` inserts pastes literally). The safety bounds — the 2 MiB diff-read
  cap, the 4M-cell diff budget, the ctrl+o output store limits, the OSC 8
  URL cap — stay as named constants: performance guards, not preferences.
- Two reference ports retired as drift, deliberately: unlabeled code
  fences no longer infer a language from their content (they render bare —
  the highlighter colors nothing it cannot name), and the markdown
  footnote grammar is gone (`[^label]` renders as the literal text the
  author wrote; a definition is an ordinary paragraph). The parity suite
  pins both retirements so they cannot creep back unnoticed.

## 0.0.1 — 2026-08-29

- Provider failures are classified by the error body's own wording, not
  just the HTTP status: a hard quota or billing wall (OpenCode Zen Go's
  `GoUsageLimitError`, `insufficient_quota`, "available balance", and
  friends) fails the step immediately instead of burning the retry ladder
  on requests that cannot succeed, while transient wording wrapped in a
  generic 400 stays retryable. The computed backoff also carries downward
  jitter, and the attempt budget is settable via `retry_max_attempts` in
  `~/.e/settings.json` (0–20, default 10).

- Sessions record what happened, not just what was said: every message
  entry carries a wall-clock `timestamp`, and assistant entries carry the
  step's real token `usage` (input, output, cached). A session file can
  now answer "where did the time and tokens go" on its own.

- The footer's token counts come only from real provider usage frames —
  the chars÷4 live estimate of streamed output is gone. A reasoning model
  streaming minutes of invisible thinking no longer balloons ↓ into a
  number nothing on screen can explain.

- Streamed thinking is shown by default (`show_thinking: on`), so a long
  thinking phase reads as thinking rather than as a hung stream. Set it
  back to `off` for the old quiet behavior.

- The completions dialect now sends `reasoning_effort` for models that
  declare an effort knob — the one wire dialect that silently dropped it.
- A preference pass — five deliberate departures from the reference where
  its choices didn't earn their keep here:
  - The statusline slims back to just the model (accent-bright) and the
    context percent, muted — the extra segments (sign-in nudge, queue
    counts, effort, session title, `Context: 12k/200k`, workspace tail and
    git branch) are gone.
  - The activity row's text wears one tone — verb, elapsed, and token
    tail all dim, no second color for the counts — beside the accent
    dot's unchanged presence-blink.
  - The `• Thinking (Ns) (↑… ↓…)` row persists through the whole turn —
    tool trees and streaming reply text included — instead of vanishing
    whenever the phase changed.
  - The running tool row's tree connector holds steady in the accent; the
    activity dot below is the one blinker (a flickering `└` read as a
    glitch).
  - The trust panel's descriptions sit right beside their choices (three
    spaces past the longest label) instead of a value column two-thirds
    across the frame.

- The trust panel gains a middle choice: trust the broader ancestor — the
  top-most directory under home containing the workspace (`Trust ~/code`
  for `~/code/clones/e-1`; the immediate parent outside home). Trust now
  propagates downward: a trusted ancestor covers every workspace inside
  it, so sibling clones and worktrees skip their first-visit question. A
  workspace's own recorded answer still wins over an ancestor's, and a
  *declined* ancestor answers only for itself — its other children keep
  their own question.

- An exactness batch closing the remaining gaps against the reference
  design, each behavior verified in its source before porting:
  - Picker tabs: the skills picker gains Source tabs (All · Global ·
    Workspace, from e's own two skill roots), the model picker Provider
    tabs (All plus each provider with models, degrading to a window around
    the active tab with dim `…` markers), and /resume Scope tabs (Current
    workspace · All workspaces, opening on the current one). Tab cycles;
    headers brighten `{title} {count}` with the active tab `[bracketed]`,
    degrade per the reference's ladders, and the hints name the dimension
    (`Tab Source` / `Tab Provider` / `Tab Scope`).
  - Selection corrected to the reference's real split: the catalog menus
    (models, skills, sessions, /tree) signal by brightness alone; only the
    inline completion pickers (slash commands, files) fill the row.
  - Skills rows go single-line — name plus a right scope column, no
    description — and sessions rows carry the `workspace · age · N turns`
    cluster in shared fixed columns (age right-aligned, compact `now/5m/2h/1d`
    grammar, turns = user messages), the title middle-ellipsized into its
    column and metadata hiding below a twelve-cell title floor.
  - /resume lists every workspace's sessions (scoped by the tabs), and
    session logs expose their recorded workspace for the cluster.
  - A queued-prompt banner above the composer while a turn runs with
    messages waiting (`N steering messages · ↑ to edit`, ink-bright), and
    the queue review behind it: ↑ on an empty composer pauses the queue
    and loads the newest prompt for editing, ↑/↓ step entries, Enter
    commits edits back (an emptied draft sends unchanged), Backspace on an
    emptied draft deletes the entry, and a fresh prompt or turn end
    resumes. With the banner above it the composer trades its leading
    blank for its top divider — the reference's chrome rule.
  - Composer shift-arrow selection: shift with any motion extends a
    reverse-video range, plain arrows collapse to its edge, typing or
    Backspace replaces it.
  - File picker rows segment long paths the reference way — dirname
    middle-ellipsized into a narrow fixed budget, basename prefix-biased —
    directories list too, with a trailing slash.
  - Markdown footnotes: `[^label]` renders a dim `[N]` numbered by first
    use; definitions collect out of the flow and close the message with
    dim `[N] ` markers and a hanging indent; unreferenced definitions
    never print.

- A functionality pass inheriting the reference design's behavior — all of
  it except the sandbox/permission layer, which e deliberately does not
  have (tools keep running without prompts; see the safety model):
  - The `ask` tool: the model can ask one question and wait. A footer
    question panel offers numbered options (digits answer in one stroke),
    ↑↓ selection, a freeform typed slot, and Esc to dismiss — a dismissal
    reads back to the model as a cancelled call, and Esc on the panel does
    not abort the turn. Available in read-only mode too.
  - Real diffs in the detail viewer: edits and writes render line numbers
    against the file as it exists now, three context lines, `⋯` elision
    between hunks, and the diff-marker green/red on the number-and-sign
    column — the changed text itself stays neutral. Behind ctrl+o.
  - Faithful resume: consecutive silent tool batches replay as the one
    growing tree they were live; restored groups seal, so a recorded call
    whose result never came renders an explicit "Tool completion was not
    reported" row and an `unreported` tally instead of silently vanishing
    under a header that counts it; recorded tool results come back to the
    ctrl+o viewer; and a live batch after a resume starts its own tree
    instead of splicing into a restored one.
  - The composer caps at half the frame plus one row: a longer draft
    scrolls behind a cursor-following window whose first row wears `┃↑`.
    Pastes collapse to a placeholder only past a thousand codepoints, the
    reference threshold — short multiline pastes insert literally.
  - Filtered picker rows brighten the chars the query matched, so a fuzzy
    hit shows why it matched. `/help` opens the commands picker itself
    (browse, filter, Enter to use) instead of printing a wall of text.
  - Link URLs are validated before entering an OSC 8 sequence — control
    bytes or an oversized URL render as plain text instead of corrupting
    the terminal's escape stream.

- A full visual-parity pass against the current reference design. The
  look moves to the reference's own literals across every surface:
  - Code fences drop the box frame for the reference's dim horizontal rules
    (`─ lang ────`) with flush-left code, a content-inferred label for
    unlabeled fences, and bare wrapped code below six columns.
  - The highlighter carries the reference's full language table (25
    profiles with aliases, block comments, per-language quote sets,
    case-insensitive SQL/Dockerfile/PowerShell), a distinct literal class in
    the number gray, and renders unknown languages raw instead of guessing.
  - Lists keep the author's ordered numbering, dim their markers, and speak
    task lists (dim `☐`, accent `✓`); blockquotes nest one rail per level;
    headings no longer lose their style to nested bold or links; tables
    honor `:---:` alignment with plain separators; bare http(s) URLs
    autolink; images render as `▧ alt` links.
  - Wrapping closes and reopens SGR and OSC 8 hyperlinks at every seam,
    avoids single-word orphan lines, and preserves the author's line breaks
    instead of reflowing paragraphs.
  - Tool trees: tallies order by descending count with the reference's
    outcome grammar (`timed out · failed · denied · cancelled`), a non-zero
    exit stays on its `Ran` row, the tree never caps its rows, edit/write
    rows carry the `+N / -M` stat suffix with the diff-marker green/red
    (truecolor with a 256-color fallback), and command previews wear the dim
    gutter with width-degrading, pluralized elision wording.
  - Errors and system rows speak the notice grammar (`● Error: …`,
    `● System: …`); a cancelled tool row brightens its summary and asks
    "What can e do differently?"; the welcome banner stays home on a fresh
    session only (a resumed transcript no longer re-banners).
  - The activity row's dot, verb, and elapsed wear the accent with the
    compact `18m0s` elapsed grammar (the statusline later slims back to
    the model and percent, and the token tail joins the accent — see the
    preference pass below).
  - Inline picker selection fills the row (selection background and ink;
    the catalog menus brighten instead — see the exactness batch above),
    headers are uniformly dim, empty states use the reference's wordings,
    nav hints degrade stepwise with the frame, and the picker band shrinks
    with its rows instead of blank-padding to six.

- `/tree` now restores the selected prompt text in the composer after
  rewinding. Edit it or resend it to grow the new branch without recreating
  the original prompt.

- Write and edit tool rows stay lean while running: the tree says
  "Writing src/lib.rs" and nothing more, instead of streaming the file's
  content as pipe rows beneath the action and displacing the thinking
  indicator. Commands keep their live output rows; full content for every
  tool still lands behind ctrl+o, and a completed write still shows its
  "Wrote … +2 -0" summary on the row itself.

- Launch starts from a clean slate: whatever the terminal showed before e
  started is scrolled into the scrollback instead of staying on screen, so
  launching in a terminal with recent commands no longer paints the frame
  (trust panel, status line) over the old content. The pre-launch output
  stays reachable by scrolling up, and the transcript reads as one
  continuous flow.

- Internal hardening pass, no user-visible behavior change. The turn loop's
  session-file writes (`TurnLog::commit`, `load_compacted`) now run on the
  blocking pool instead of inline on the async task, matching the built-in
  tools' own `spawn_blocking` treatment. Mutex locking on shared
  history/session/extension-request state is consistently poison-tolerant
  (`unwrap_or_else(|e| e.into_inner())`) instead of a mix of that and bare
  `unwrap()`, so one panic under a lock can no longer cascade into every
  later call failing. The Anthropic stream now drops a nameless `tool_use`
  block the same way the other three dialects already drop an empty-named
  call. The extension host's argv flag parser and its argv-stripping pass
  share one `match_flag` instead of two independently maintained copies of
  the same matching rules. A finished background `bash` process's tracked
  handle is now evicted (oldest-finished-first) once the process-lifetime
  map passes 64 entries, instead of growing unbounded for the life of the
  session; a still-running handle is never evicted. `docs/sandboxing.md`
  points at `thule`, the planned first-party sandboxing project, instead of
  a sibling project's example.

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
  live catalog.
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
  Linux on both architectures), fed by a release workflow that builds and
  publishes the four binaries on every version tag.
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
