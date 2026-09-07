# Architecture

e is one primary Rust crate with two directional layers:

```text
CLI / TUI
    │ subscribes to one ordered SessionEvent stream
    ▼
terminal-free core
    ├── agent turn loop ──► provider wire dialects ──► model APIs
    ├── tools ────────────► the selected working directory
    ├── extension host ───► user-installed child processes over JSONL
    └── stores ───────────► ~/.e only
```

The single-crate shape is intentional. A new crate needs an independent
consumer, release/API boundary, platform boundary, or measured build-time
benefit. The `sdk/` package is that case made explicit: an independent
consumer of the library target whose release boundary is the point (see
[decisions/0002-rust-sdk-package.md](decisions/0002-rust-sdk-package.md)).
It is a consumer, not a fourth layer. File length alone is a reason to
extract a module, not a crate.

## Invariants

- The frontend receives text, tools, usage, warnings, and errors through one
  ordered event stream. There are no state side channels.
- The core owns run completion, steering, and compaction. `TurnEnd` means
  the run has stopped; a context checkpoint emits `Compacting` and
  `Compacted` without ending the run. RPC and the TUI share this lifecycle.
- Each agent captures its working directory and configuration home and owns
  file observations and background handles. Explicit `AgentOptions` paths
  allow concurrent callers without changing process environment variables.
- `SessionLog` stores the conversation tree. An OS-held sidecar lock excludes
  other writers. The sidecar stays on disk; ownership ends when the handle closes.
- `ChatMessage` carries a tagged `MessageKind`. Only assistant records hold
  tool calls; tool records require a call id. Existing JSONL formats still load.
- `core/` is terminal-free. Terminal behavior stays in `tui/`.
- Provider differences terminate at the dialect seam; the agent loop consumes
  one request and event vocabulary.
- User-controlled behavior is file-backed or supplied by the extension
  process boundary. e does not embed a scripting runtime or daemon.
- `~/.e/` is e's only home. Store writes merge unknown keys and replace files
  atomically.
- Trust gates repository-provided context. It is not an execution sandbox.
  The complete threat model is in [../SECURITY.md](../SECURITY.md).

## Decisions

Short records in `docs/decisions/` preserve why a difficult-to-reverse choice
was made. They are required for changes to persistence formats, public wire
protocols, trust boundaries, process architecture, or the single-event-stream
model. Ordinary features and refactors do not need one.
