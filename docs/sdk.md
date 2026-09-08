# SDK

The `e-sdk` package (`sdk/`) provides programmatic access to e's agent core
from Rust: create a session, send prompts, subscribe to one ordered event
stream, and run tools — the same harness the terminal frontend drives,
without a terminal.

**Status: minimal facade implemented; unstable.** A session builder, the
event stream, and prompting/steering are in place. Until the first release
declares a semantic-versioning policy, the surface may still change (see
[compatibility.md](compatibility.md)).

## Why a separate package

The SDK is not part of `core/` and not an extension. It is a second in-repo
consumer of e's library target — the same code the binary and the
integration tests link — given its own release boundary so that
stabilization is a deliberate act rather than an accident of visibility. See
[decisions/0002](decisions/0002-rust-sdk-package.md).

## Building

```sh
cargo build -p e-sdk
cargo test -p e-sdk # once the SDK has tests
```

The package is a member of the root workspace, so `./x check` and `./x test`
cover it like the rest of the repository.

## Surface

The SDK mirrors what `main.rs` does before handing off to the terminal
frontend.

Implemented today:

- **Session creation** — `Session::builder()` against a working directory
  (defaults to the process cwd), persistent or in-memory (`save_session`),
  with a model chosen by slug or the catalog default.
- **One event stream** — `next_event().await` yields the same ordered
  `SessionEvent` vocabulary the frontend consumes: text and thinking deltas,
  tool execution, usage, errors. No side channels, matching the architecture
  invariant. It also does the per-turn `TurnEnd` bookkeeping, so a caller can
  simply prompt again after one.
- **Prompting and steering** — `prompt()` starts a turn, or steers into a
  running one; `interrupt()` stops a turn at the next safe point.
- **System prompt** — the session assembles the same prompt the frontend
  builds for the working directory — base instructions plus the skills
  catalog and any AGENTS.md the directory trusts — fresh at each prompt, or
  uses a `system()` override.
- **Configuration isolation** — `home()` (or `E_HOME`) points a session at an
  isolated `~/.e`-style home so an embedding runs fully self-contained, the
  same seam the test suite uses.

```rust
use e_sdk::{Session, SessionEvent};

let mut session = Session::builder()
    .cwd("/path/to/project")
    .model("anthropic/claude-opus-4-5") // optional; catalog default otherwise
    .save_session(false)
    .build()?;

session.prompt("What files are in the current directory?");
while let Some(event) = session.next_event().await {
    match event {
        SessionEvent::TextDelta(delta) => print!("{delta}"),
        SessionEvent::TurnEnd { .. } => break,
        _ => {}
    }
}
```

Not yet on the stable facade — reach in through `agent_mut()` for now:
attaching an extension host, resuming an existing session file, per-run tool
allowlists, and thinking-level selection.

## What the SDK is not

- **Not an extension.** Extensions are child processes speaking a JSONL
  protocol to a running e ([extensions.md](extensions.md)). The SDK links the
  core into your program.
- **Not a daemon.** e stays a spawned process; there is no server to run.

If you are integrating from another language, the JSON output and RPC modes
in [automation.md](automation.md) remain the language-agnostic surface; the
SDK is the in-process Rust alternative.
