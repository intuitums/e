# e releases

## Unreleased

### Packages, planning, and longer-running sessions

Install shared extensions and resources, delegate tasks, and return to work with
session forks, exports, and persistent prompt history. The terminal gains side
panes and a fuller tool-output reader, with fixes for interrupted and resumed work.

### New features

- Test current work with dev npm/bun packages, beta installers, or a pinned PR build. Preview channels have separate state.
- Run a checkout with `./x dev` and inspect repeatable terminal scenarios with `./x scenario`. Build diagnostics include the channel and source commit.

- Install e through brew, npm, or bun. Releases publish the native packages automatically; package-managed installations use their package manager for updates.
- Install resource packages from git, npm, local directories, or checksum-verified release archives. `e packages` lists them, `e remove` removes them, and `e packages init` creates a package to share.
- Load a package for one run with `--package` (`-P`), share project packages through `.e/packages`, and filter which extensions, skills, prompts, or themes a package loads. npm lifecycle scripts stay disabled.
- Add extension side panes, widgets, status slots, activity text, editors, and selection dialogs. Configure pane placement and status-row templates in `~/.e/layout.json`; Ctrl+T moves focus between regions.
- Extension commands support argument hints and completions. Hooks can shape input, turns, tool results, rendering, and compaction summaries. Session requests can steer, name, interrupt, compact, or narrow a session's tools.
- Keep extension entry points and helper files together in bundle directories. Examples cover planning, delegated agents, project startup, and MCP tools.
- Run a headless turn with `e -p`; add `--json` for session events. `e rpc` accepts a built-in tool allowlist and returns the saved session path.
- `e rpc` is a session server: `session.create` opens a conversation against any working directory, `session.prompt` streams its events tagged with the session and request and answers with the turn result, and sessions run side by side. Steer, interrupt, compact, fork, export, resume saved sessions, list models, and change model or effort between turns. Extension questions reach the client as `ask` lines to answer. Version-1 one-shot lines keep working unchanged.
- Trust a workspace without the terminal with `e trust [dir]`, and reverse it with `e untrust [dir]`. An unattended session — a channel bot, a CI job — cannot answer the trust panel, and until now that left it loading none of the repository's own instructions.
- Install the Slack channel from npm (`npm install -g @intuitums/e-slack`, or `npx @intuitums/e-slack`). It ships with each release — its own version, the release's npm tag.
- Reference channels under `channels/`: a Slack bot (one thread, one session, with buttons for extension questions, created from `channels/slack/manifest.json`) and a GitHub Actions workflow answering `/e` on issues and pull requests. `e docs channels` describes the pattern.
- Run the Slack channel on a server from the published `ghcr.io/intuitums/e-slack` image, or build `channels/slack/Dockerfile` from a checkout: e comes from the release and the bot from the repository, so the host needs neither Node nor a checkout of e.
- Embed e with the `intuitums-e-sdk` package. Its session builder configures models and resources, streams typed turn events, and supports steering and cancellation.
- Install the SDK from crates.io with `cargo add intuitums-e-sdk`. It follows semantic versioning on a version of its own, independent of the application (`intuitums-e` — the npm naming, since `e` is taken on crates.io), and each stable release publishes whichever of the two has a new version.
- Use `/fork` to continue a branch in a new session and `/export` to save a self-contained HTML conversation.
- Use `/undo` to restore up to 100 session writes or edits and `/usage` to inspect recorded tokens and estimated cost by model.
- Recall prompts from previous sessions with Up on an empty composer. Ctrl+G opens the current draft in an external editor.
- Load nested `AGENTS.md` instructions when tools first touch their directories in a trusted workspace.
- Page retained tool output with `read_result`. Bash retains up to 4 MiB; session retention is bounded to 32 results and 16 MiB.
- Give `/compact` a focus to guide its checkpoint. Filter `/resume` and `/tree` by typing.
- Select Inline or Fullscreen TUI mode in `/settings`. `tool_label_rows` controls the height of command labels, and `paste_placeholder` controls pasted-text collapse.
- Use the updated model catalogs, including GPT-6 Astra and the GPT-5.6 models. ChatGPT subscription discovery follows its live model picker.
- New models arrive with their facts, not just their ids. e reads models.dev in the same background refresh as the providers' own lists, caches a trimmed copy in `~/.e/models-dev.json`, and sets context windows, effort levels, the Anthropic thinking shape, image and tool support, and pricing on built-in and discovered models. Seeds are the offline fallback; `models.json` still wins.

### Improvements

- **Upgrade:** e refuses to run in an untrusted workspace instead of running it without the repository's own instructions. Accept the trust dialog, or record the decision with `e trust [dir]` for a session with no terminal.
- The installer refuses a host below the Linux binaries' glibc floor (2.39) with a message naming the requirement, instead of failing after the download with a linker error.

- **Upgrade:** Beta binaries move to a separate repository. Reinstall beta once to adopt its new update source. Dev builds now use npm/bun; production releases remain in the main repository.

- **Upgrade:** Local Cargo builds now use `~/.e-dev` instead of the stable home. Set `E_HOME` explicitly to select another dedicated home.

- **Upgrade:** `/diff` is now the separate [e-diff](https://github.com/fschrhunt/e-diff) package. Install it with `e install git:github.com/fschrhunt/e-diff`; the old in-repository package and its build instructions are removed.
- **Upgrade:** `e ask` is removed. Use `e -p` for a headless turn or `e rpc` for JSONL automation. Piped stdin requires a supported headless mode.
- **Upgrade:** read-only mode and the `ask` tool are removed. Use `--no-tools` or a tool allowlist; extensions should obtain required input through configuration or their supported interface requests.
- **Upgrade:** Ctrl+D deletes forward. Press Ctrl+C twice to exit, or change the keybindings. Unlabeled code fences no longer guess a language, and footnotes remain literal text.
- **Upgrade:** the unstable Rust API uses `core::extensions`, `SessionLog`, and tagged `MessageKind` values. Session logs use OS-held locks; stop older processes before resuming their sessions.
- Ctrl+O groups full tool details behind a three-line Review view. Right expands Full detail; scrolling back to the bottom resumes following output.
- Consecutive edits to one file share a transcript row with cumulative counts. Set `combine_consecutive_edits` to `off` to keep separate rows; history retains every call.
- Running tools retain connected trees, wrapped labels, and live output tails. Completed output replaces previews, and long heredocs stay in the reader.
- Clipboard paste handles images and text, reports attachment progress, and defers submission while reading the clipboard. Pasted-text labels include a number and character count.
- Long replies render with bounded work. The composer stays visible through resize, and terminal rendering preserves native scrollback.
- Compaction preserves instructions and prior summaries. Invalid or ineffective checkpoints leave history intact; RPC waits for the final answer after automatic compaction.
- Usage records distinguish uncached, cache-read, and cache-write tokens, retain response identity, and include compaction and billed empty replies.
- File edits preserve links, ACLs, extended attributes, and existing line endings. Separate agents retain their own homes, observations, and background-process handles.
- Provider failures show a short message while retaining diagnostics privately. Quota failures stop retries; transient failures show cancellable backoff.
- Provider-level model settings and context-window overrides survive catalog refreshes. Anthropic tool-result batching and prompt caching follow the conversation.
- Startup and long-session rendering have performance budgets. Terminal-frame checks cover tool trees, composer placement, errors, and review colors.

### Fixes

- Partial declarations of new models retain cached model facts. Explicit provider image-support settings win over feed facts for discovered models.
- RPC memory-only resume leaves saved logs untouched, and forks preserve the current effort setting.
- Slack restores conversations from saved paths after restart and keeps extension questions and answers with their owning thread.
- The GitHub channel requires repository write permission before execution and replies correctly to inline PR review comments.

- Cancelled turns skip queued tools, and late events cannot mutate a newer turn. Continuous output no longer prevents shell timeouts.
- Tool batches use bounded concurrency and preserve provider order for calls that name the same file.
- Session recovery repairs torn final records and unfinished calls. Resume locks history before reading; corrupt parent links fail even on inactive branches.
- Compaction rejects stale snapshots and checks cancellation before replacing history. Failed turn or paint workers report errors instead of leaving activity running.
- Removing paste or diff markers also removes their hidden payload. Draft history and completion preserve remaining attachments; CRLF pastes no longer add newlines.
- Ctrl+O opens correctly after long sessions and restores the main terminal buffer on close. Labels, wide characters, table fallbacks, and hyperlinks stay within their display bounds.
- Rejected images preserve the question as literal text. Editing queued prompts no longer pauses the queue; already-consumed edits become new prompts.
- Picker navigation no longer changes reasoning effort behind the picker. Model selection scrolls into view, and unavailable model IDs remain visible in settings.
- Tools handle oversized lines, numeric overflow, missing paths, stale observations, externally deleted files, and link-less filesystems without hanging or silently overwriting newer content.
- Background handles retain process ownership until their pipes close and never signal a completed process again. SIGTERM and SIGHUP stop tracked shell process groups.
- Provider streams retain error details, split text, and reasoning boundaries. Partial failures cannot appear successful; usage counters saturate instead of overflowing.
- Extensions report crashes once, discard oversized stderr lines without terminating the protocol, and reap children on shutdown. Notice-only input verdicts remain visible.
- OAuth refresh is serialized per home and provider. Callback readers have size and time limits, and cancellation interrupts accepted connections.
- **Security:** provider and OAuth requests reject redirects; release downloads reject HTTPS downgrades. Credentials and request bodies stay on their intended origin.
- **Security:** new credentials and sessions are private from creation. Unix homes use `0700`, session files use `0600`, and reopening older sessions tightens permissions.
- **Security:** model output, tool labels, draft text, trust paths, and extension notices cannot inject terminal controls. Git review disables external helpers and does not write the index.
- **Security:** file writes detect replacement during staging, remove partial new files after failure, and fail freshness checks on metadata errors.
- `--package npm:<name>` loads the package it installed. Git URLs with `user:password@` credentials parse as one source, `e remove` finds a clone recorded under different case, prompt templates with CRLF endings keep their front matter, and an empty `E_HOME` no longer means the current directory.
- Responses streams surface a top-level `error` event instead of stalling, keep one cache key per session on the Codex mount, and leave out a reasoning item that no output followed. Gemini calls without `args` carry `{}`; a request that cannot be built is not retried.
- `read` pages past a line over the 64 KiB cap instead of failing the file, `read_result` accepts numeric strings and never returns an empty window, `edit` counts overlapping matches as ambiguous, and bash keeps a code point split around the other stream, reports dropped bytes correctly, accepts `"background": "true"`, and keeps text before a trailing carriage return.
- A session tail torn inside a character still loads and lists, and resuming after a lost final newline starts the next record on its own line. A `grep` of a directory loads that directory's `AGENTS.md`. Usage from a failed attempt no longer stands in for the retry's.
- Extensions: `"error": null` beside a result is success, a `tool.update` carrying a top-level id stays a progress chunk, a failed start no longer blocks startup behind a full notice channel, and `rpc` behind an extension flag answers `ui.*` requests headlessly.
- Pane text and markdown sections scroll through all of their rows, loose list items keep every paragraph, and the Ctrl+O review no longer shows a previous session's rows after `/resume`.

## 0.0.1

September 9, 2026

### A coding agent for your terminal

The first release of e runs on macOS and Linux. Connect a hosted or local model,
work in a repository, and save conversations you can resume or branch later.

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

- **Upgrade:** `ls`, `find`, and the dedicated `skill` tool are removed. Use bash for directory listings and filename searches, and `read` to load a skill's `SKILL.md`.
- **Upgrade:** OpenCode Zen's provider ID is now `opencode-zen`. Existing `opencode` credentials still work; saved model selections under that ID need to be selected once more.

- Tool calls render as connected trees with live command output, outcome counts, and edit statistics. Full write content stays in Ctrl+O rather than filling the conversation.
- Markdown supports highlighted code, tables, task lists, nested blockquotes, and terminal hyperlinks. The composer wraps drafts, supports selection, and collapses large pastes.
- Provider retries show their cause and backoff and can be cancelled immediately. Billing and quota failures stop without retrying; `retry_max_attempts` controls the retry budget.
- Sessions record timestamps and real token usage. Footer counts use provider reports rather than estimated output, and Show thinking is off by default.
- Reasoning effort reaches supported models across all four provider dialects. Anthropic prompt caching covers the growing conversation.
- Write and edit results send a short confirmation to the model instead of repeating the new content. Long shell results retain their output tail.
- Model, skill, and session pickers have filter tabs. `/help` opens the searchable command picker.
- Terminal rendering preserves native scrollback and redraws changed rows. Long syntax-highlighted lines no longer stall input.

### Fixes

- Resume follows the selected branch rather than replaying abandoned messages. Torn final records can be recovered; interior corruption reports an error.
- Interrupted replies retain text already shown, and unfinished tool calls get explicit results so history can replay.
- Provider streams preserve split UTF-8, reasoning blocks, and tool-call identity. Truncated or filtered replies report warnings instead of appearing successful.
- Shell commands enforce their timeout, stop their process group, and drain output without deadlocking. Non-regular files fail instead of hanging tools.
- Edits reject stale file observations, and parallel edits to the same file no longer silently overwrite each other.
- Extension crashes and blocked pipes cannot leave calls waiting indefinitely. Failed startup cleans up child processes.
- Startup preserves typed input, and exit paths restore terminal modes. Wide characters and long drafts keep the cursor in the right column.
- Session and configuration write failures report warnings. Unknown configuration keys survive updates, and corrupt configuration files are preserved for recovery.

- **Security:** Directory trust controls project instructions, skills, and prompts, not tool execution. Trusted ancestors cover their children unless a workspace has its own recorded choice.
- **Security:** Model output and extension notices cannot inject terminal controls. Hyperlinks reject control bytes and oversized URLs.
- **Security:** Pasted API keys stay out of composer recall history. Diagnostics omit credential values.
- **Security:** Releases include checksums, a CycloneDX SBOM, and signed build provenance. CI checks allowed network hosts, configuration write paths, and pinned workflow actions.
