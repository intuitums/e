# Inline rendering contract

The main screen shows a transcript with the composer and status rows at its
tail. The renderer owns visible rows; the terminal owns rows that have
scrolled out of view. Those historical rows are snapshots of what was shown
at that time. They are not a second editable copy of the session document.

- Streaming may change visible Markdown, tool rows, and the dock. A large
  append must paint every new row on its way into scrollback, even if an
  earlier Markdown row also changed.
- A shorter frame repaints its visible tail in place. The logical window
  can move backward without scrolling the terminal backward, so collapsing
  tool output or closing a panel cannot move the composer off screen.
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
