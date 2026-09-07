# Core owns complete runs

Status: accepted
Date: 2026-09-06

## Context

The agent paused at low context and relied on the TUI to compact, restart,
and reset its running state. RPC interpreted the same pause as completion.
Shared cancellation flags and process-global tool state also allowed old
work or another agent to affect a later run.

## Decision

The core supervisor owns the prompt queue and terminal event. Publishing
`TurnEnd` and becoming idle occur under the same queue lock. A prompt that
arrives before publication is handled within the active run.

The turn worker compacts at provider boundaries, after committing all tool
results. Manual compaction uses the same path. Summarization protects user
instructions and prior checkpoints; an incomplete response or an ineffective
checkpoint leaves history unchanged and reports an error.

Each run receives a fresh cancellation token. Each agent owns its tool state
and captures its configuration home and working directory. Provider tasks
inherit the home explicitly; blocking session writes enter its synchronous
scope. The process-wide shell registry remains solely for exit cleanup.

Session ownership uses an OS-held lock on a persistent sidecar. PID contents
no longer determine ownership, and contenders never unlink the lock file.
Session JSONL remains format version 1. `MessageKind` makes message payloads
distinct in Rust without changing the persisted role names or field names.

## Consequences

The TUI projects progress instead of managing agent execution. RPC returns
once after the complete run, including context maintenance. Embedders can
drive the core without reproducing frontend bookkeeping.

The unstable Rust paths change from `core::api` to `core::extensions` and
from `session::Session` to `session::SessionLog`. Message fields specific to
a role are accessed through the tagged kind or read-only accessors.

Old PID-lock binaries must be stopped before upgrading a session writer;
concurrent writers from both locking implementations are unsupported.
