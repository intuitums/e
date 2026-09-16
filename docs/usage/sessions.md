---
title: Sessions
description: resume, branch, compact, and export a conversation
order: 1
---

# Sessions

A session is one conversation with its history: messages, tool calls, their
output, and the usage they recorded. Sessions are the unit that `e rpc` opens
([automation](automation.md)), the unit `/resume` lists, and the unit a
[channel](../usage/channels.md) maps to a thread.

## Where they live

Saved conversations are JSONL files under `~/.e/sessions/`, one file per
session, appended as the turn runs. `e` starts a new one; `-c` continues this
directory's most recent; `-r` picks from the list. In the terminal, `/resume`
does the same with a picker, and `/tree` shows the current session's shape.

Treat session files as project data. They hold what the tools read — source,
configuration, and anything else the model was shown — so a shared session or
an [export](#export) is as sensitive as the repository was.

## Branch in place

Every message has an id and a parent, so a session is a tree, not a line.
`/tree` moves back to an earlier message and continues from there; the branch
you abandoned stays in the file, reachable the same way. Nothing is rewritten.

Branching changes the conversation, not the working directory, and it does not
undo edits a tool already made. Use git for the files.

`/fork` copies the current branch into a session file of its own, which is what
you want when the next task deserves a separate history rather than a second
branch of this one. `e rpc` exposes both as `session.fork` and the `parent` of
a resumed session.

## Context

When the model's window fills, e summarizes older messages to make room and
continues the same turn; you do not restart the task. `/compact` asks for that
summary by hand, and `/compact <focus>` steers what it keeps — decisions,
unfinished work, a subsystem.

Compaction preserves the instructions and the previous summaries, so a long
session keeps its rules. `/usage` reports recorded tokens and estimated cost by
model for the session, including compaction's own requests.

## Export

`/export` writes the active branch as one self-contained HTML page: the
conversation, the tool activity, and the code it touched. Read it before you
send it — the page is exactly what the model saw.

## Recall

Up on an empty composer walks back through your prompts from this and earlier
sessions (`~/.e/history.jsonl`), which is usually faster than remembering which
session you meant. `/undo` restores up to 100 session writes or edits.

## Commands

| Command | Effect |
| --- | --- |
| `/resume` | reopen a saved session |
| `/tree` | return to an earlier message and branch in place |
| `/fork` | continue in a separate session file |
| `/compact [focus]` | summarize older context, optionally around a focus |
| `/export` | save the active branch as one HTML page |
| `/undo` | restore session writes or edits |
| `/usage` | tokens and estimated cost per model |
| `/name <label>` | name the session, shown in `/resume` |
