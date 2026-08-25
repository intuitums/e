# Audit backlog triage (#51–#99)

Forty-nine issues filed 2026-08-23 from the post-audit pass (after #42–#47 landed in PR #50). Grouped into six epics.

**GitHub epics:** [#100](https://github.com/intuitumxyz/e/issues/100)–[#105](https://github.com/intuitumxyz/e/issues/105) · **Index:** [#106](https://github.com/intuitumxyz/e/issues/106)

## Epics

| Order | Epic | Issues |
|-------|------|--------|
| 1 | [#100 Auth, credentials, config](https://github.com/intuitumxyz/e/issues/100) | 64, 65, 66, 89, 97, 98, 99 |
| 2 | [#101 Session persistence](https://github.com/intuitumxyz/e/issues/101) | 57, 78, 79, 80, 91 |
| 3 | [#102 Tool execution](https://github.com/intuitumxyz/e/issues/102) | 53, 63, 95, 96 |
| 4 | [#103 Provider streaming](https://github.com/intuitumxyz/e/issues/103) | 51, 52, 56, 58, 73, 74, 75, 76, 90 |
| 5 | [#104 TUI hardening](https://github.com/intuitumxyz/e/issues/104) | 81–88, 93, 94 |
| 6 | [#105 Extension lifecycle](https://github.com/intuitumxyz/e/issues/105) | 54, 55, 60–62, 67–72, 77, 92 |

## Per-issue slotting

| Issue | P | Epic | Title |
|-------|---|------|-------|
| 51 | P0 | 103 | Anthropic tool loops discard signed thinking blocks |
| 52 | P1 | 103 | Anthropic models receive unsupported manual-thinking requests |
| 53 | P1 | 102 | Live bash output corrupts UTF-8 across pipe reads |
| 54 | P1 | 105 | Failed extension init leaves orphan processes |
| 55 | P2 | 105 | Empty quoted prompt-template args shift positional params |
| 56 | P2 | 103 | Cache-token accounting wrong |
| 57 | P1 | 101 | New/resume during active turn corrupts sessions |
| 58 | P1 | 103 | Responses API accepts incomplete output |
| 59 | P1 | 100 | Concurrent config writes race on shared temp file |
| 60 | P1 | 105 | Late shell results contaminate wrong session |
| 61 | P2 | 105 | Initial prompts drop hyphen-prefixed words |
| 62 | P2 | 105 | Prompt templates listed but not invokable |
| 63 | P2 | 102 | grep zero matches on explicit dotfile |
| 64 | P2 | 100 | Esc closes OAuth UI without cancelling login |
| 65 | P1 | 100 | Wrong-state OAuth callback aborts login |
| 66 | P1 | 100 | Auth errors recommend unusable `e auth` command |
| 67 | P1 | 105 | One-shot CLI skips extension shutdown |
| 68 | P1 | 105 | Extension blocking stdin bypasses timeouts |
| 69 | P1 | 105 | Duplicate extension tools, wrong owner executes |
| 70 | P0 | 105 | Pasted API keys exposed to input hooks |
| 71 | P1 | 105 | Concurrent input hooks reverse prompt order |
| 72 | P1 | 105 | Late extension-command results wrong session |
| 73 | P1 | 103 | SSE transport errors before output non-retryable |
| 74 | P1 | 103 | Chat Completions accepts truncation silently |
| 75 | P1 | 103 | Anthropic stop reasons ignored |
| 76 | P1 | 103 | OpenAI refusal dropped → blank turn |
| 77 | P1 | 105 | Prompt at TurnEnd stranded/reordered |
| 78 | P1 | 101 | Stale/inherited session names |
| 79 | P2 | 101 | Status line never shows name/queue |
| 80 | P0 | 101 | Session append failures silently discarded |
| 81 | P1 | 104 | Error paths leave bracketed-paste/keyboard modes on |
| 82 | P0 | 104 | Model/extension text injects terminal controls |
| 83 | P2 | 104 | Bad theme hex panics TUI |
| 84 | P1 | 104 | clip_styled miscounts Unicode / breaks OSC 8 |
| 85 | P2 | 104 | code_panel panics at 0–3 columns |
| 86 | P2 | 104 | Two cursors at wrap boundaries |
| 87 | P2 | 104 | Paste placeholders never retired |
| 88 | P1 | 104 | Composer uses char count not display width |
| 89 | P1 | 100 | Lossy trust keys for non-UTF-8 paths |
| 90 | P1 | 103 | Malformed SSE JSON silently dropped *(body was empty — see below)* |
| 91 | P0 | 101 | Concurrent resumes corrupt JSONL |
| 92 | P1 | 105 | Esc doesn't cancel extension hooks/tools |
| 93 | P1 | 104 | OSC probe swallows startup keystrokes |
| 94 | P1 | 104 | Long tokens clipped not wrapped |
| 95 | P0 | 102 | Parallel edit loses changes, both succeed |
| 96 | P1 | 102 | FS tools block forever, Esc ignored |
| 97 | P0 | 100 | Config update erases unreadable files |
| 98 | P0 | 100 | Partial models.json sends creds to OpenCode Go |
| 99 | P0 | 100 | xAI refresh loses rotated creds on persist fail |

## P0 summary (9 issues — do these first)

98, 97, 99, 95, 91, 80, 51, 82, 70

## Overlap (not duplicates)

| Theme | Issues | Fix in |
|-------|--------|--------|
| Esc / cancel | 64, 92, 96 | 100 / 105 / 102 |
| Session corruption | 57, 91 | 101 |
| Late async results | 60, 72 | 105 |
| Unicode width | 84, 88, 94 | 104 |

## Defer / discuss before closing

| Issue | Note |
|-------|------|
| 85 | 0–3 column terminal — extreme edge |
| 89 | Non-UTF-8 path trust collision — exotic |
| 63 | grep dotfile quirk |

## #90 body (was empty at filing)

Malformed JSON in SSE `data:` payloads is `continue`'d in `completions.rs`, `anthropic.rs`, and `responses.rs` — the HTTP stream can still return 200 while text, tools, usage, or stop events are lost. Should surface a provider error or fail the turn.
