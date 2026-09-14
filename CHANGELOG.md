# e

## Unreleased

**Packages and an extension surface at pi's reach, plus the core features that could not be extensions: a headless print mode, tool-result paging, session fork and export, undo, prompt history across sessions, a usage dashboard, and nested instructions. Ctrl+O reads full tool output in a two-depth reviewer; pasted text is easier to identify and remove; cancellation, compaction, and session saving have been hardened. The `/diff` review is now the `e-diff` package, outside this repository.**

### Breaking changes

- `/diff` is no longer in this repository. The `packages/diff` crate left
  with its docs and scripts; the Git review is the
  [e-diff](https://github.com/fschrhunt/e-diff) package
  (`e install git:github.com/fschrhunt/e-diff`), which speaks the same
  display surface. The `packages/` directory is gone with it: the palette,
  highlighter, and text sanitizers it held are back inside e, and the
  repository ships nothing but e and its SDK — packages are the community's.
- `e ask` is removed. Use `e rpc` for headless automation, with one JSON request and response per line. Piped stdin without `e rpc` now reports a usage error.
- Read-only tool mode is removed, including `--read-only`, `--ro`, the `read_only` RPC mode, and `read_only_notice`. Use `--no-tools` to disable tools, or the RPC `tools` allowlist to select built-ins.
- The `ask` tool and its question panel are removed. Extensions should read required input from configuration or report what is missing.
- Ctrl+D no longer quits from an empty composer. Press Ctrl+C twice to exit. Ctrl+D deletes forward and can be rebound in `~/.e/keybindings.json`.
- Unlabeled code fences no longer guess a language. Markdown footnotes render as literal text.
- The unstable Rust API now uses `core::extensions`, `SessionLog`, and tagged `MessageKind` payloads. Role-specific message fields become accessors.

### New features

- Extension commands can declare `arguments` (a hint shown in the `/`
  picker; picking the command leaves `/name ` to finish) and
  `completions` (typing `/name pre` asks the extension and offers its
  answers in a picker). Prompt templates with an argument hint start the
  same way.
- `e --package <source>` (`-P`) loads a package for one run without
  recording it; a trusted repository's `.e/packages` lists packages the
  team shares. `session.send` takes `when: "next_turn"` to attach an
  internal message to the user's next prompt.
- Release packages: `e install release:<owner>/<repo>/<name>[@tag]` fetches
  a compiled extension published as `<name>-<target>.tar.gz` on a GitHub
  release, verifies it against the release's `checksums.txt`, and installs
  it like any other package.
- Nested `AGENTS.md` files load on demand: the first time a tool touches a
  path under a directory that has one, its instructions join the
  conversation, nearest last. Trusted workspaces only, once per directory
  per session. `e docs instructions` covers the three levels.
- `/usage [24h|7d|30d|all]` shows requests, tokens, and estimated cost by
  model from the sessions on disk, as a table in the transcript.
- `/undo` puts back what the last write or edit replaced, up to a hundred
  changes back in the session; a file the tool created is removed.
- The activity row says `Compacting context` while a mid-turn compaction
  runs instead of `Thinking`.
- Typing while `/resume` or `/tree` is open filters the list; Enter or Esc
  clears the filter from the composer.
- `/fork [name]` continues in a new session file seeded with the current
  branch; the original stays as it was. Extensions see `session_start` with
  reason `fork`.
- `/export [path]` writes the session as a self-contained HTML page:
  prompts, replies rendered from markdown, tool calls and results folded.
  Default `e-session-<id>.html` in the working directory.
- `/compact <focus>` tells the checkpoint what to keep the most of; the
  summary keeps its fixed sections (goal, constraints, progress, decisions,
  next steps, critical context), and a summary that lost one is kept but
  reported as a warning. Extensions pass `focus` to `session.compact`.
- Truncated tool results can be paged. Bash keeps up to 4 MiB of a
  command's output and grep and extension results are kept whole; the
  model sees the usual 32 KiB with a notice naming a result id, and the
  new built-in `read_result` reads the rest by byte window or by query.
  Results are kept for the session, bounded to 32 entries and 16 MiB.
- Prompt history persists across sessions (`~/.e/history.jsonl`, newest
  thousand, private): ↑ on an empty composer recalls prompts from earlier
  sessions too.
- ctrl+g opens the composer draft in an external editor (`editor` setting,
  `$VISUAL`, `$EDITOR`, then `vi`) and loads what was saved back into the
  composer.
- `e -p "prompt"` runs one turn headless and streams the reply to stdout;
  the prompt can come from stdin. `e -p --json` streams every session event
  as a JSON line and ends with the same result object `e rpc` returns. See
  `docs/automation.md`.
- The extension surface grows to pi's reach, across the process boundary
  (decision 0005). Extensions can subscribe to lifecycle events
  (`session_start`, `turn_start`, `tool_end`, `compact_end`, `model_change`,
  …), shape a turn with `before_turn` (a system-prompt paragraph and a
  message), `tool_result` (redaction), and `compact_summary` hooks, give
  their tools built-in-style rows (`label`) and results a summary, viewer
  detail, and a format (`text`, `markdown`, `diff` — a unified diff paints
  with line numbers and coloured markers, like an edit's), show blocks in
  the transcript, and declare shortcuts. A new direction: extensions ask e
  things and get answers — `ui.notify`, `ui.show`, `ui.select`,
  `ui.confirm`, `ui.input`, `ui.status`, `ui.compose`, `ui.panel` (an
  interactive panel receives keys and redraws), `session.send`, `.info`,
  `.name`, `.model`, `.effort`, `.tools` (narrow the toolset: plan mode),
  `.interrupt`, `.compact`. Everything shown is data painted through the
  theme; requests are bounded and answered "no ui" under `e rpc`. Version-1
  extensions are unchanged. `docs/extensions/plan.mjs` shows the surface;
  the scaffold gains promise-returning `ui` and `session` helpers.
- The frame is regions an extension or the user can set (decision 0007).
  `ui.pane` opens a side pane beside the conversation — a selectable
  list, a diff, text, markdown, or themed rows — and e owns focus,
  scrolling, selection, the mouse, and the split; what the user does
  comes back as `pane.select`, `pane.activate`, `pane.key`, and
  `pane.closed`, and Enter attaches the selected rows to the composer.
  `ui.widget` puts rows above the composer; `ui.status` takes a `key` for
  several slots. `~/.e/layout.json` decides where every pane goes and how
  wide it is, the chord that moves focus (`ctrl+t`), whether the banner
  shows, and what the status row says, as templates of tokens (`{model}`,
  `{effort}`, `{context}`, `{cwd}`, `{session}`, `{status}`), and the
  activity row (`Thinking (3s) (↑1k ↓20)`) the same way (`{phase}`,
  `{elapsed}`, `{tokens}`, `{activity}`), with `ui.activity` giving
  extensions a place on it. `e docs layout` carries the guide. A `render` hook lets an extension re-render
  a tool's finished result or a completed reply from data (`renders:
  ["tool:bash", "assistant"]`), and `ui.editor` asks for a multi-line
  answer.
- Packages: `e install <source>` clones a git repository (or references a
  local directory) shaped like `~/.e/` — `extensions/`, `skills/`,
  `prompts/`, `themes/` — under `~/.e/packages/<host>/<path>`, records it in
  the `packages` list of `settings.json`, and every loader reads it after the
  home's own resources. `e packages` lists them, `e remove` forgets one and
  deletes its clone, and `e install` alone makes disk match settings (clone
  what is missing, re-check pinned refs, fast-forward unpinned ones). A
  listed package missing on disk is reported at startup; startup itself never
  touches the network. The `$` picker labels package skills `Package`.
  `e docs packages` carries the guide.
- Packages from npm: `e install npm:<name>[@version]` installs with your own
  `npm` into `~/.e/packages/npm/`, lifecycle scripts always off, and the
  `e-package` keyword lists a package in the catalog (`site/packages/`). A
  git package with a `package.json` gets its dependencies installed the same
  way. A `packages` entry may be an object with `source` and per-kind glob
  filters (`"extensions": ["!extensions/legacy.mjs"]`) so part of a package
  stays unloaded. `e packages init <dir>` starts a package to publish, and
  trusting a repository installs what its `.e/packages` lists.
- `/diff` ships as an optional extension (`packages/diff`), not part of the e binary. Build `e-diff`, drop it in `~/.e/extensions/`, and the command prints the continuous Git review — file summaries, per-file patches, syntax colors, and word-level changes — into the transcript. `/diff <path>` reviews one file. See `docs/diff.md`.
- Ctrl+O adopts fx's full-output reader layout: spliced tool details, vertical rails, and a navigation footer. Review folds each detail to three lines; `→` expands to Full. Scroll with the keyboard or mouse; returning to the bottom resumes following new output.
- Extensions can live in directories under `~/.e/extensions/`, keeping their entry point and helper files together.
- `e rpc` accepts a built-in `tools` allowlist and returns the saved `session` path. The subagent example uses these for delegated tasks and access to their full results.
- `e help` prints the same usage as `e --help`.
- The `e-sdk` package is implemented: `Session::builder()` resolves model, effort, tools, home, and a session file to resume up front; `prompt()` returns a lazy, backpressured `Turn` that streams typed events and settles into a `Reply` (or a `TurnError` carrying the partial reply); `steer()`, `cancel()`, and drop-to-interrupt mirror the terminal. Sessions are memory-only and extension-free unless asked. Core: tool batch events carry each call's name and raw arguments, `Agent::steer` holds a message for a running turn without ever starting one, `submit_message_with_steers` queues steering atomically with its prompt, and `ExtensionHost::start_in` lets an embedding choose the extensions' workspace and command line.

### Improvements

- Consecutive successful edits to the same file share one transcript row with cumulative counts. Any intervening tool call breaks the group. Ctrl+O and session history keep every call. Set `combine_consecutive_edits` to `"off"` in `~/.e/settings.json` to keep separate rows.

- Pasted-text labels show their draft-local number and character count in the same dim gray as image attachments. Set `paste_placeholder` in `~/.e/settings.json` to change the collapse threshold; `0` inserts pastes literally.
- Running tools stay connected to their tree while output streams. Multiline commands keep dim continuation rows, and completed commands replace previews with their retained output.
- Long replies no longer copy the entire response on each update. Live Markdown rendering has a fixed work budget.
- RPC continues through automatic compaction and returns the final answer. `/compact` uses the same core path at the next provider boundary.
- Compaction preserves user instructions and earlier summaries. Truncated or ineffective summaries leave history intact and report an error.
- Session writers use OS-held locks. Stop older processes before resuming their sessions; existing JSONL files need no migration. Empty lock sidecars may remain.
- File edits preserve symlink targets, hard links, ACLs, and extended attributes. A failed final copy can still leave a partial update.
- Independent agents can use separate configuration homes and workspaces. File observations and background-process handles belong to each agent.
- The default system prompt asks for concise answers and clear file paths.
- Changelogs and GitHub releases use short summaries and grouped bullets, without an appended install block.

- Provider usage now uses disjoint uncached, cache-read, and cache-write
  counters. Saved sessions keep response identity, provider, model, purpose,
  and usage outside replayed message content, including compaction requests
  and billed blank replies; copied history retains the original response identity. RPC output reports every category and pricing accepts separate cache-write rates.

### Bug fixes

- Deleting a pasted-text or diff marker discards its hidden payload. Diff markers select on the first Backspace and delete on the second. History and completion preserve attachments that remain in the draft, and CRLF pastes no longer gain extra newlines.
- Ctrl+O no longer opens blank after long conversations. Closing either reader restores the main terminal buffer without adding expanded output to chat scrollback.
- Long tool labels, including image paths, fit their column without a one-cell overflow.
- Cancelled runs skip queued tools, and late tool events cannot change a newer turn. Continuous shell output no longer starves timeout checks.
- Tool batches have bounded concurrency. Calls that name the same file run in provider order.
- Compaction rejects stale history snapshots and checks cancellation before replacing history.
- Session resume locks the log before reading it. Corrupt parent links are rejected even on inactive branches.
- Failed turn and paint workers report errors instead of leaving the activity row running silently.
- A screenshot sent to a model without image support keeps your question and drops only the image. Rejected-image text remains literal even if it begins with a command.
- Shift+Tab moves backward through an open picker's tabs without changing reasoning effort behind it.
- Explicit `context_window` overrides survive live model refreshes and e updates.
- Editing a queued prompt no longer pauses the queue. If the turn has already consumed it, saving the edit submits a new prompt.
- Grep no longer skips the line after an oversized line. Provider usage totals saturate instead of overflowing.
- OpenAI reasoning summaries preserve paragraph breaks and stay hidden unless Show thinking is enabled.

### Security

- Provider and OAuth requests refuse redirects, keeping credentials and private request bodies on the intended origin. Release downloads reject HTTPS downgrades.
- Credential staging files are private from creation. On Unix, e's home uses `0700` and session files use `0600`, including older files when reopened.

- Clipboard paste now handles Command+V without inserting a stray `v`, reads
  macOS image clipboards with one pasteboard probe, falls back to text, shows read
  and attachment state outside editable history, colors `[Image n]` labels in
  the existing light gray, and defers Enter until the clipboard result arrives.
  Scoped-model settings show preserved unavailable IDs and accurate availability
  counts; corrupt settings warnings name the recovery file instead of silently
  presenting defaults.
- Restore compact Inline mode by default. `/settings` now offers TUI Mode with
  Inline and Fullscreen choices; existing `composer_position: bottom` preferences
  remain supported until a new mode is saved.
- Tool labels use two terminal-width rows by default, adjustable through
  `tool_label_rows`. Wider windows reveal more text; heredoc bodies stay in
  Ctrl+O rather than filling the transcript. Full commands remain available.
  Arithmetic shifts and `<<` inside shell comments do not hide subsequent lines.
- Tool trees keep one closing review hint after completion. Ctrl+O branches
  stay connected through arguments, output, and omission rows without changing
  the review layout or controls.

- Review follow-ups: partial tool arguments prevent retries, SSE error messages
  are bounded before publication, and failed diagnostic rollback retires the
  session handle. Error-summary settings load off async workers. Ctrl+C closes
  trust and queue navigation without submitting held prompts, and the shell
  prefix space has its own cursor and selection cell. The network guard now
  scans tooling filenames containing spaces or quotes.

- Running tools stay in connected, wrapped trees with live output tails and
  Ctrl+O review. Leading `!` uses a green gutter marker, and edit counts show
  green additions and red deletions.
- Provider failures show brief UI errors and retain local diagnostic details in
  private session sidecars and headless JSON. Partial completions with error
  frames fail instead of appearing successful. Display-off time is not sleep.
- Drafts, trust paths, and tab titles cannot inject terminal controls. Ctrl+C
  cancels across panels, pasted line endings normalize once, vertical cursor
  motion follows display columns, and long trust questions can be scrolled.
- `./x ui` checks terminal frames for tool trees, composer placement, shell
  styling, short errors, and diff colors on Linux and macOS CI.

- Review fixes for the audit: background handles reserve their PID until pipes
  close; failed new-file copies remove their partial target; metadata errors
  fail freshness checks closed. Read windows reject oversized first lines with
  an offset to skip them, and integer arguments reject overflow without rounding.
- OAuth refresh waits are scoped to each home and provider; fresh Codex tokens
  bypass them. Extension exit notices survive EOF immediately after initialize.
  Stream error codes distinguish authentication failures and transient server
  disconnects while preserving hard-quota classification.

- Sessions survive a crash: a torn final line is truncated on reopen instead
  of fusing with the next record, and dangling tool calls or orphaned
  reasoning blocks are repaired anywhere in a history (on `/tree` too), so a
  resumed session stays resumable and replayable on every dialect.
- Providers: Anthropic and Together live model sync no longer discards every
  chat model (`type` is a deny-list of non-chat kinds now, and the Anthropic
  list asks for 1000 entries); a mid-stream `{"error":…}` frame in a
  completions or Gemini body surfaces as the provider's error instead of a
  clean success or a bare stall; provider-level `context_window`,
  `max_output`, `effort`, `thinking`, and `pricing` in models.json apply to
  built-in seed models as documented; parallel Anthropic tool results ride
  in one user turn. models.md lists the `chatgpt` catalog strategy.
- Tools: `write` creates files on link-less filesystems (exFAT) and after an
  external delete; `edit` matches across CRLF line breaks and keeps the
  file's endings; `read` cuts on a whole line and says which offset to
  continue from; `grep` fails on a missing path and stops on a file whose
  reads keep failing instead of spinning; `offset`, `limit`, and `timeout`
  accept integral floats and numeric strings; a finished background handle
  is never signalled again (pid reuse); a writable file inside a read-only
  directory can be edited.
- Extensions: a runtime crash is announced once in the transcript (louder
  when the extension owned a `tool_call` or `input` hook); an over-long
  stderr line is discarded instead of closing the pipe and killing the
  extension; children are reaped on exit and shutdown; notice-only input
  verdicts are shown; the startup-hook example now routes launches to existing
  project directories, and the MCP guide notes the `npx -y` cold start; the
  protocol.rs header matches what the host sends.
- Config and CLI: self-update declines on platforms with no release
  artifact instead of installing the x86_64-gnu tarball over a source build;
  concurrent OAuth refreshes are serialized so one refresh token is never
  redeemed twice; shift+letter and `-`/`+` chords bind; `doctor` or
  `providers` behind an extension flag is a usage error, not a prompt; the
  help text no longer claims piped stdin is read.
- TUI: a caught panic (paint worker, tool task, turn worker) no longer drops
  the terminal out of raw mode; the vertical table fallback and link
  destinations with spaces keep OSC 8 sequences whole; `/models` scrolls
  to the current model; CRLF pastes keep one newline per line; the
  composer hangs seam whitespace off the row and moves ↑/↓ by display
  column over wide characters.
- GPT-6 Astra, OpenAI's new flagship, is seeded on both OpenAI providers: the
  API (`openai`, 1.05M window, `xhigh`/`max` efforts included) and ChatGPT
  Codex (`openai-codex`, 272k codex lane). The GPT-5.6 trio is seeded on the
  API side too. Codex's live model discovery now works at all: its /models
  is the ChatGPT model-picker payload, not an OpenAI `data` list, so it was
  failing silently — the new `chatgpt` catalog strategy reads the picker
  (work-mode entries, `-wm` suffix stripped, `max_tokens` as the window),
  and a new Codex model appears in the picker with no e release.

## 0.0.1 — 2026-09-08


- ctrl+v attaches desktop-clipboard images to the composer draft: file copies
  become attachments by path, bitmaps are read directly (macOS via osascript,
  Linux via wl-paste/xclip). Helpers run with a timeout and a byte cap so a
  hung or flooding clipboard cannot stall the session, and a failed read
  surfaces as a notice. Attachments ride the same steering queue as text
  (whole messages, images included), never attach over an open surface, and
  commands submitted with a draft attached dispatch without them. Submitted
  labels `[Image 1] [Image 2]` match the transcript.
- The composer stays visible when a scrolled frame shrinks, and large streamed
  appends paint every new row even when earlier Markdown changes. Resize keeps
  native scrollback and redraws only the visible tail. Later tool batches retain
  expanded thinking instead of deleting it.
- Painting takes ownership of pending frames and compares only visible content,
  reducing repeated work in long sessions. PTY regressions cover streaming and
  tool completion across resize; release benchmarks now budget long-session
  frame rendering.

- Cancellation skips queued tool waves, and late tool events cannot change a
  newer turn. Rejected-image text stays a literal prompt, even when it starts
  with a command.
- Compaction rejects stale history snapshots and observes cancellation while
  reserving its start and completion events. File writes detect inode replacement
  during staging and copying on Unix.
- Session resume locks the log before loading history, and tree readers reject
  corrupt parent links on inactive branches too.
- Tool batches have bounded concurrency and execute calls naming the same file
  in provider order. Completed commands replace partial streaming previews with
  their retained output.
- The TUI and its background tasks inherit the selected configuration home.
  Compaction checks cancellation after staging its checkpoint, before replacing
  history. Provider usage totals saturate instead of overflowing.
- Provider and OAuth requests refuse redirects so API-key headers and private
  request bodies cannot reach another origin. Release downloads retain a
  separate redirect policy that rejects HTTPS downgrades.
- Credential staging files are private from creation. On Unix, state writes
  secure the e home to `0700`, new session files use `0600`, and reopening an
  older session tightens that file's permissions.

- Tool labels, targets, and live previews strip terminal control sequences.
- OAuth callbacks have size and time limits. Malformed or idle connections no longer end login, and cancellation interrupts accepted connections.
- File writes detect inode replacement during staging and copying on Unix.
- SIGTERM and SIGHUP stop all tracked built-in shell process groups before `e rpc` exits.
- Git review disables external diffs, text conversion, filesystem monitors, and clean/process filters. It uses bounded reads and does not write the index.

## 0.0.1

**The first e release brings a Rust coding agent to macOS and Linux. Use hosted or local models, resume and branch conversations, and add your own tools, commands, and themes.**

### Breaking changes



- `ls`, `find`, and the dedicated `skill` tool are removed. Use bash for directory listings and filename searches, and `read` to load a skill's `SKILL.md`.
- OpenCode Zen's provider ID is now `opencode-zen`. Existing `opencode` credentials still work; saved model selections under that ID need to be selected once more.

### New features

- Connect to OpenAI, Anthropic, Google, xAI, OpenCode, OpenRouter, and other hosted providers, or use Ollama and LM Studio locally.
- Sign in with supported subscriptions or API keys. The model picker refreshes live catalogs and lets you save a preferred model scope.
- Resume with `e -c` or `/resume`, and branch from an earlier prompt with `/tree`. Older linear sessions load without conversion.
- Steer an active turn by sending another message. Review queued prompts above the composer.
- Attach PNG, JPEG, GIF, and WebP images. Supported models receive them, and sessions retain them for resume.
- Open full tool output and edit diffs with Ctrl+O. Run shell commands directly with `!`.
- Use `/compact` or automatic compaction to continue long conversations. The previous session remains available in `/resume`.
- Add tools, slash commands, hooks, and typed launch flags through executable JSONL extensions. Examples include MCP tools, delegated agents, and worktree launches.
- Customize themes, composer keybindings, models, prompt templates, and skills under `~/.e/`. Trusted projects can supply their own instructions, skills, and prompts.
- Use `e ask` for a headless turn or `e rpc` for JSONL automation. `e doctor` and `e providers` provide redacted diagnostics.
- Install checksum-verified binaries for macOS and Linux on ARM64 or x86-64. `e update` downloads updates, and `/reload` switches to an installed update while resuming your session.
- Read the bundled configuration and extension guides with `e docs`.

### Improvements

- Tool calls render as connected trees with live command output, outcome counts, and edit statistics. Full write content stays in Ctrl+O rather than filling the conversation.
- Markdown supports highlighted code, tables, task lists, nested blockquotes, and terminal hyperlinks. The composer wraps drafts, supports selection, and collapses large pastes.
- Provider retries show their cause and backoff and can be cancelled immediately. Billing and quota failures stop without retrying; `retry_max_attempts` controls the retry budget.
- Sessions record timestamps and real token usage. Footer counts use provider reports rather than estimated output, and Show thinking is off by default.
- Reasoning effort reaches supported models across all four provider dialects. Anthropic prompt caching covers the growing conversation.
- Write and edit results send a short confirmation to the model instead of repeating the new content. Long shell results retain their output tail.
- Model, skill, and session pickers have filter tabs. `/help` opens the searchable command picker.
- Terminal rendering preserves native scrollback and redraws changed rows. Long syntax-highlighted lines no longer stall input.

### Bug fixes

- Resume follows the selected branch rather than replaying abandoned messages. Torn final records can be recovered; interior corruption reports an error.
- Interrupted replies retain text already shown, and unfinished tool calls get explicit results so history can replay.
- Provider streams preserve split UTF-8, reasoning blocks, and tool-call identity. Truncated or filtered replies report warnings instead of appearing successful.
- Shell commands enforce their timeout, stop their process group, and drain output without deadlocking. Non-regular files fail instead of hanging tools.
- Edits reject stale file observations, and parallel edits to the same file no longer silently overwrite each other.
- Extension crashes and blocked pipes cannot leave calls waiting indefinitely. Failed startup cleans up child processes.
- Startup preserves typed input, and exit paths restore terminal modes. Wide characters and long drafts keep the cursor in the right column.
- Session and configuration write failures report warnings. Unknown configuration keys survive updates, and corrupt configuration files are preserved for recovery.

### Security

- Directory trust controls project instructions, skills, and prompts, not tool execution. Trusted ancestors cover their children unless a workspace has its own recorded choice.
- Model output and extension notices cannot inject terminal controls. Hyperlinks reject control bytes and oversized URLs.
- Pasted API keys stay out of composer recall history. Diagnostics omit credential values.
- Releases include checksums, a CycloneDX SBOM, and signed build provenance. CI checks allowed network hosts, configuration write paths, and pinned workflow actions.

[Detailed development history](https://github.com/intuitums/e/blob/48d1e0cba4665ca6c2f9050d27a510f3bfa989aa/CHANGELOG.md).
