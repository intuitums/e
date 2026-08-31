# SDK

The `e-sdk` package (`sdk/`) provides programmatic access to e's agent core
from Rust: create a session, send prompts, subscribe to one ordered event
stream, and run tools — the same harness the terminal frontend drives,
without a terminal.

**Status: scaffolded, not yet implemented.** The package exists and builds as
part of the root workspace, but the facade below is the planned surface, not
a shipped one. Until the first release, the SDK's surface is unstable (see
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

## Planned surface

The SDK mirrors what `main.rs` does before handing off to the terminal
frontend:

- **Session creation** — a session against a working directory, persistent or
  in-memory, with resume of an existing session file.
- **One event stream** — the same ordered `SessionEvent` vocabulary the
  frontend consumes: text and thinking deltas, tool execution, usage, errors.
  No side channels, matching the architecture invariant.
- **Prompting and steering** — send a prompt, steer mid-run, queue follow-ups.
- **Model and tool selection** — pick a model and thinking level; enable or
  disable built-in tools.
- **Configuration injection** — an `E_HOME`-style override so embeddings run
  fully isolated, the same seam the test suite uses.

The intended shape, not yet implemented:

```rust
use e_sdk::{Session, SessionEvent};

let mut session = Session::builder()
    .cwd("/path/to/project")
    .model("anthropic/claude-opus-4-5")
    .build()?;

session.subscribe(|event| {
    if let SessionEvent::TextDelta(delta) = event {
        print!("{delta}");
    }
});

session.prompt("What files are in the current directory?").await?;
```

## What the SDK is not

- **Not an extension.** Extensions are child processes speaking a JSONL
  protocol to a running e ([extensions.md](extensions.md)). The SDK links the
  core into your program.
- **Not a daemon.** e stays a spawned process; there is no server to run.

If you are integrating from another language, the JSON output and RPC modes
in [automation.md](automation.md) remain the language-agnostic surface; the
SDK is the in-process Rust alternative.
