# Terminal UI checks

These tests run the real e binary in a PTY against a loopback provider. They
replay completed synchronized frames with `pyte`, checking cell positions and
foreground colors. Raw captures, resize offsets, text snapshots, and synthetic
provider requests remain available after a pass or failure.

## Run

On macOS or Linux, create a Python environment once:

```sh
python3 -m venv /tmp/e-ui-env
/tmp/e-ui-env/bin/pip install -r tests/ui/requirements.txt
PYTHON=/tmp/e-ui-env/bin/python ./x ui
```

`./x ui` builds the current binary and runs the seven checked scenarios. It
prints the temporary artifact directory and exits nonzero if a check fails.
Both Linux and macOS CI run it after `./x check` and retain artifacts on failure.
Rust rendering and PTY tests remain part of `./x test` without Python packages.

Choose one scenario or a fresh output directory:

```sh
PYTHON=/tmp/e-ui-env/bin/python ./x ui --out /tmp/e-ui-review diff-counts
```

| Scenario | Contract |
| --- | --- |
| `single-tool` | One command stays within two label rows, retains connected rails, and reveals its full label after widening the terminal. |
| `heredoc-tool` | The main tree shows only the heredoc invocation; Ctrl+O retains its body and connects the branch through output in both review depths. |
| `tui-mode` | Startup is inline by default; the settings switch pins and unpins the composer. |
| `tool-tree` | With `tui_mode` set to `fullscreen`, composer stays bottom-pinned during concurrent command output, wrapping, resize, and completion. Running commands retain connected rails and output hints. |
| `shell-composer` | Only the leading `!` turns green; wrapped rails stay neutral; deleting `!` restores the normal gutter. |
| `body-error` | A truncated provider response retains partial text and displays only the short error. |
| `diff-counts` | A real file edit displays green `+2` and red `-1`, with neutral labels and separator. |

The checks require evidence that the relevant states occurred. An empty capture
or a turn that never finished cannot pass. Startup and completion waits have
bounds; timed steps still pace typing and resizing to exercise live transitions.

## Inspect or add scenarios

`run.py` owns the isolated provider and input steps. `checks.py` owns assertions.
They share `scripts/term.py` with existing replay tooling rather than introducing
a second terminal emulator. Checks inspect each completed frame, not just whether
a success marker occurred somewhere in the raw output.

Open a generated `.txt` snapshot or replay a `.raw` capture:

```sh
/tmp/e-ui-env/bin/python scripts/term.py /tmp/e-ui-review/diff-counts/session.raw 100 30
```

Do not print raw captures directly into your terminal. Exploratory scenarios
include intentional terminal-control injections.

Older investigation scenarios remain available explicitly as capture-only
repros. They do not claim a test pass:

```sh
PYTHON=/tmp/e-ui-env/bin/python ./x ui --record-only narrow-trust paste-control
```

Each scenario gets fresh `HOME`, `E_HOME`, and workspace directories. Fixtures
use dummy credentials, disable extensions and auto-update, and send requests only
to the local mock provider. Only `tool-tree`, `single-tool`, `heredoc-tool`, and `diff-counts` enable tools.
They run generated `printf`/`sleep`/`cat` commands or edit a generated file. No real
provider credentials, paid requests, or repository files are used.

## Limits

These are terminal-state tests, not pixel screenshots. They cannot validate a
terminal's font, glyph rendering, compositor, or every emulator's behavior.
Timing-heavy failures still need inspection of the retained frames. Keep the
Rust byte-pinned rendering tests and occasionally review a real terminal too.
