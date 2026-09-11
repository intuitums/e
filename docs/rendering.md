# Inline rendering contract

The main screen starts inline beneath the shell prompt, with the composer and
status area following the transcript. The renderer owns visible rows; the
terminal owns rows that have scrolled out of view. Those historical rows are
snapshots of what was shown at that time. They are not a second editable copy
of the session document.

- Streaming may change visible Markdown, tool rows, and the dock. A large
  append must paint every new row on its way into scrollback, even if an
  earlier Markdown row also changed.
- A shorter full-height frame repaints its visible tail in place. Its logical
  window moves backward without scrolling the terminal backward, keeping the
  dock at the bottom when tool previews disappear or panels close.
- Short conversations use a compact layout by default. In `/settings`, change
  **TUI Mode** from **Inline** to **Fullscreen** to pin the composer to the
  bottom of the terminal, leaving blank space above it. Changes apply immediately
  and persist in `~/.e/settings.json` as `"tui_mode": "inline"` or
  `"tui_mode": "fullscreen"`. Missing or invalid values use `inline`.
  Manual file edits apply with `/reload`. The older
  `"composer_position": "bottom"` preference selects Fullscreen until a TUI mode
  is saved. Both modes use the normal terminal screen and native scrollback.
- Resize redraws only the new visible tail. It does not erase scrollback or
  print the entire transcript again. The terminal controls how existing
  history wraps. The full-detail viewer renders current source at the new
  width when historical presentation is insufficient.
- Finishing thinking leaves it expanded. A later tool batch cannot absorb
  that reasoning as a collapsed summary. Only legacy summaries marked done
  may be absorbed into a continuing tree.
- The paint worker receives owned frames through a single pending slot.
  Resize discards a queued frame at the old width; the next frame uses the
  new dimensions. An already executing write can finish before that redraw.

This preserves existing native history, but does not promise that history
matches a later rendering of the full document. Edits above the visible
window remain historical snapshots. Reflow and large collapses can bring
previously seen source back into the visible tail. After resize, pre-launch
content still on the visible screen may be overwritten because its row
positions are no longer known. Content already in scrollback is retained.

Completed blocks cache their rendered Markdown. Live Markdown still parses
the complete response under a source-size pacing budget. Each frame still
assembles the cached transcript rows, but the painter takes ownership of
that frame and compares only the reachable suffix. Further incremental
parsing or shared-row storage should be justified by a measured bottleneck.

## Tool trees and shell input

Running and completed calls occupy the same tree positions, in provider order.
Paths, commands, failure reasons, and edit statistics wrap by display-cell width.
Every visible tool has a `├` branch, with `│` on wrapped continuation rows.
Groups close with `└ ctrl+o to view`, including single-tool and completed groups.
If the closing hint wraps, its elbow stays on the final display row.
Review attaches details after each action's final row. Its branch stays
connected through the output and closes on the group's final displayed row,
including any omission hint. Review adds no redundant shortcut to open itself.

Tool action labels use at most two display rows by default, measured at the
current terminal width after the tree gutter. A clipped label ends with `…`.
Resize reflows from the original arguments, revealing more on wider terminals.
Heredoc commands show their invocation through the header and `…`, never the
script body. Ctrl+O retains the complete command, including line boundaries.
Quoted `<<` text and here-strings are not treated as heredocs.

`"tool_label_rows"` in `~/.e/settings.json` changes this budget from 1 through 20
rows; missing or invalid values use 2. Apply with `/reload`. Existing groups,
new calls, and restored sessions use the same preference. Review is uncapped.
Failure status and colored edit counts are not truncated with the arguments.
Failure reasons have their own bounded continuation using the same row budget.

Running commands show a wrapped output tail inside their branch, followed by
an omission count when needed. There is one review hint per group, always on
its own closing row, never embedded in a preview's omission count. Connectors
share the theme's `muted` token; output text uses `dim`. File writes and edits
do not stream their contents inline.
Edit/write counts use the theme's green added-marker and red removed-marker
tokens, including their 256-color fallbacks. The slash stays dim and zero counts
are omitted. Wrapping and review preserve those colors.

`"tool_preview_rows"` in `~/.e/settings.json` controls the live preview budget.
The default is 5 rendered rows; accepted values are 0 through 20. Apply with
`/reload`. Running buffers retain the latest 64 KiB and disclose when earlier
output was omitted. Ctrl+O can show retained output before completion. After
completion, the tool's final retained result replaces the live buffer.

A literal `!` at the start of the draft replaces its first gutter rail and uses
`bashMode`. The prefix is not repeated in the command text. Continuation rails
stay neutral. Editing, selection, history, and submission retain raw indices
and the original prefix; masked credential entry never activates shell styling.

## Verify session transitions

```sh
cargo test --lib tui::paint::screen::tests
cargo test --test pty_stream
cargo test --test parity
./x test
./x check
./x bench
```

The screen regressions cover a collapse larger than the viewport, a resize
of an overflowing frame, and a large append combined with an offscreen
edit. The PTY tests run the real application through sustained Markdown
streaming and through a command tool followed by a reply. Both resize
during the session and check terminal cleanup. Literal appearance remains
pinned by the parity tests.

For visual inspection, retain the synthetic PTY captures and replay them
with the existing `pyte` helper:

```sh
E_PTY_ARTIFACTS=/tmp/e-rendering-review cargo test --test pty_stream
python3 scripts/term.py /tmp/e-rendering-review/PTY_STREAM_FINISHED.raw 100 30
python3 scripts/term.py /tmp/e-rendering-review/PTY_TOOL_FINISHED.raw 100 30
```

`ptycap.py` answers startup probes before typing and records resize offsets
in a `.sizes.json` sidecar. `term.py` applies those sizes while replaying.
The helper requires Python's `pyte` package; the Rust and PTY tests do not.
Inspect transitions as well as the final screen when investigating a new
failure. A tail marker in captured bytes alone cannot prove that every
earlier frame was correct.

## Display power and interrupted responses

Turning off the display or moving focus away from the terminal does not cancel
a run. Streaming and tools continue while the process and network remain
available. Actual system sleep can suspend both; e estimates some sleep gaps
from clock divergence, not display notifications. That estimate depends on the
platform's monotonic clock and can also be affected by wall-clock corrections.

`provider response interrupted` means the HTTP body could not be read to
completion. Backend diagnostics include the underlying transport cause where
available; the TUI shows only `Provider response interrupted.`.
Reqwest's `error decoding response body` alone does not mean the model sent
invalid JSON; a truncated HTTP body produces the same headline. The message
cannot, by itself, prove whether a provider, proxy, or local network caused it.

Before any output, retryable transport failures use the normal retry budget.
After output, including partial tool arguments, e retains the partial response
and reports the failure rather than blindly replaying the request. Sleep-attributed failures have a separate
bounded continuation policy. A display-off event alone does not activate it.

`./x ui` checks completed terminal frames for composer anchoring, shell styling,
short errors, and colored diff counts. See [tests/ui/README.md](../tests/ui/README.md)
for checked scenarios and capture-only repros of paste safety and trust panels:

```sh
cargo build
python3 -m venv /tmp/e-replay-venv
/tmp/e-replay-venv/bin/pip install -r tests/ui/requirements.txt
PYTHON=/tmp/e-replay-venv/bin/python ./x ui --out /tmp/e-replay-new
```

Use a fresh output directory. Read the generated `.txt` frames or replay the
`.raw` files through `scripts/term.py`; do not print raw injection captures
directly into a terminal. The helper uses dummy credentials, an isolated home,
and a loopback provider, with extensions and auto-update disabled. Tools are
disabled except in `tool-tree`, which runs synthetic `printf` and `sleep` commands,
and `diff-counts`, which edits a generated file in the isolated workspace.

## Backend error details

Terminal provider failures emit `SessionEvent::ErrorDetails` immediately before
`SessionEvent::Error`. The existing error string remains available to backend
callers. The TUI consumes the report's short summary, not the diagnostic body.
Cancellation, sleep stops, tool exit codes, and recovered retries do not become
terminal provider-error reports.

Reports retain the observed failure stage and cause, provider and model,
available HTTP status and request ID, provider code, timestamp, attempt duration,
retry decision and budget, partial-output counts, and settled tool counts.
Recovery guidance belongs in the report. A transport failure does not establish
whether the provider, proxy, or local network caused it.

Saved sessions append reports to a private `<session-stem>.errors.jsonl` sidecar.
Each record links to the preceding message on its branch. Reports never enter
model context or change the session's message format. `--no-save` writes no
report file. A diagnostic append whose rollback also fails retires the session
handle, preventing later records from extending a torn JSON line. Headless JSON
includes `error_details` even without saving.

Reports collect no request bodies or authentication headers. Known bearer
credentials and transport-error request URLs are redacted; provider-authored error text can
still contain sensitive information and should be reviewed before sharing.
Only allowlisted response IDs are captured, with length limits. Diagnostic text
is bounded before compatibility error events are published and marks truncation.
Nothing is uploaded and no reporting command or team-submission prompt is added.

Short headlines can be overridden in `~/.e/settings.json` with `error_auth`,
`error_network`, `error_stalled`, `error_rate_limited`, `error_quota`,
`error_unavailable`, and `error_rejected`. Missing or empty values use the
built-in headline. These keys are read on a blocking worker when a failure
occurs, with the same scoped e home as the failed turn.
