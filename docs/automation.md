# Automation

## One turn: `e -p`

`e -p "prompt"` runs one turn without a terminal and prints the reply as it
streams. The prompt is the argument, or piped stdin when there is none.
Warnings and the error, if any, go to stderr. Exit status is 0 for a
completed turn, 1 for an error or an interrupted turn, 2 for a usage
problem. `--no-save` keeps it memory-only; every other run option
(`--model`, `--effort`, `--no-tools`, `--image`, `--no-extensions`) applies.

```sh
e -p "what does this repo do"
git diff | e -p "review this diff"
```

`e -p --json` streams every session event as one JSON line, then a final
`{"type":"result", …}` line with the same fields as an `e rpc` response:

```json
{"type":"turn_start"}
{"type":"text","delta":"The repo"}
{"type":"tool_batch","calls":[{"id":1,"name":"read","arguments":{"path":"README.md"},"category":"read","target":"README.md"}]}
{"type":"tool_start","id":1}
{"type":"tool_end","id":1,"outcome":"completed","summary":"12 lines","content":"…"}
{"type":"usage","input_tokens":1200,"output_tokens":80,"cache_read_tokens":0,"cache_write_5m_tokens":0,"cache_write_1h_tokens":0}
{"type":"turn_end","aborted":false}
{"type":"result","output":"…","final_output":"…","model":"provider/model","effort":"high","aborted":false,"error":null,"error_details":null,"warnings":[],"usage":{…},"cost_usd":null,"tools":{"calls":1,"failures":0},"session":null}
```

Event types: `turn_start`, `text`, `reasoning`, `tool_batch`, `tool_start`,
`tool_output`, `tool_end`, `compacting`, `compacted`, `usage`, `warning`,
`error`, `error_details`, `retry`, `recovered`, `session_name`, `steered`,
`slept`, `sleep_stopped`, `discarded`, `turn_end`. Consumers should ignore
types they do not know; new ones are additive.

## Many turns: `e rpc`

`e rpc` is the headless session server: JSONL over stdin and stdout, the
same framing extensions speak. A client spawns it, keeps the pipes, and
drives sessions from any language. There is no port, no token, and no
daemon — the process lives as long as its client, and shutting the client
down shuts e down. This is what a Slack bot, a Linear integration, or a
CI job builds on (see [channels.md](channels.md)).

Every input line is one request:

```json
{"id":"c1","method":"session.create","params":{"cwd":"/repo","model":"anthropic/claude-opus-5","save":true}}
```

Every request gets exactly one response line carrying its `id`, either
`{"id":…,"result":{…}}` or `{"id":…,"error":"…"}`. `id` is any JSON value.
Events are the lines with a `type`; a line without one is a response.

```json
{"id":"c1","result":{"session":"01a0…","model":"anthropic/claude-opus-5","effort":"high","cwd":"/repo","path":null}}
```

### Sessions

A session is one conversation against one working directory. Sessions run
concurrently in one process; each runs one turn at a time. A prompt streams
its events as they happen, each tagged with the `session` and the `request`
it serves, and answers when the turn ends:

```json
{"id":"p1","method":"session.prompt","params":{"session":"01a0…","prompt":"what does this repo do?"}}
{"type":"turn_start","session":"01a0…","request":"p1"}
{"type":"text","delta":"It is","session":"01a0…","request":"p1"}
{"type":"tool_batch","calls":[…],"session":"01a0…","request":"p1"}
{"type":"usage","input_tokens":1200,"output_tokens":80,…,"session":"01a0…","request":"p1"}
{"type":"turn_end","aborted":false,"session":"01a0…","request":"p1"}
{"id":"p1","result":{"output":"…","final_output":"…","model":"…","effort":"high","aborted":false,"error":null,"error_details":null,"warnings":[],"usage":{…},"cost_usd":null,"tools":{"calls":1,"failures":0},"session":"01a0…","path":"/home/u/.e/sessions/…/….jsonl"}}
```

The event vocabulary is the one `e -p --json` prints, above. A turn stays
active through automatic compaction and continuation; the result describes
the completed run. A second `session.prompt` while a turn runs is refused —
steer or interrupt instead.

### Methods

```
hello              {ask?}                        → {protocol, version, channel, commit, cwd, home, methods, ask}
models.list        {}                            → {default, models:[{model, provider, id, effort, image_input, tools, context_window}]}
session.create     {cwd?, model?, effort?, tools?, tool_mode?, save?, resume?, name?}
                                                 → {session, model, effort, cwd, path}
session.list       {cwd?, all?}                  → {sessions:[{path, title, name, modified, messages, turns, cwd}]}
session.info       {session}                     → {session, model, effort, cwd, path, name, messages, running}
session.prompt     {session, prompt, images?}    → events…, then the turn result
session.steer      {session, text}               → {held:true}       a message into the running turn
session.interrupt  {session}                     → {}
session.compact    {session, focus?}             → compacting, compacted, then a result
session.set        {session, model?, effort?}    → {model, effort}  between turns; history carries over
session.messages   {session}                     → {messages:[…]}   the conversation in e's persisted shape
session.fork       {session}                     → {session, path}  a new session carrying the history
session.export     {session, path?}              → {path, title}    the conversation as one HTML page
session.close      {session}                     → {}
ask.reply          {ask, result?}                → {}               answer an extension's question
shutdown           {}                            → {}               then the process exits 0
```

`session.create`:

- `cwd` is the working directory, absolute or relative to the process's;
  it must exist. Each session may use a different one — a Slack channel per
  repository. Trust for that directory's `AGENTS.md` follows
  [instructions.md](instructions.md) as it does in the terminal, so a session
  refuses an untrusted directory: a caller with no terminal records the
  decision first with `e trust <dir>`.
- `model` and `effort` override process defaults from `-m` / `--ef`.
- `tools` is a positive built-in allowlist; `tool_mode` is `all` or `none`.
  Both may narrow what the process allows (`--no-tools`), never widen it.
- `save` defaults to false: memory-only. `true` writes the session log like
  the terminal does; the result's `path` names it, and `session.list` finds
  it later. `--no-save` on the process wins.
- `resume` is a saved session's path, from `session.list` or an earlier
  result. With `save` true the conversation continues in that file, locked
  for this process while the session is open; with `save` false the history
  is loaded without opening a writer, repairing the file, or taking a
  session lock. `--no-save` also keeps resume read-only.
- `name` labels the session (`session.list` shows it).

`session.prompt` takes `images`, a list of PNG, JPEG, GIF, or WebP paths
(ten files, 20 MiB each, 40 MiB total); the model must declare image input.

`session.set` changes the model or effort for the following turns without
touching the user's saved settings. `session.fork` copies the branch into
a session of its own — a file of its own when the original persists — so
the two grow apart from there. A fork inherits the current model and effort,
including changes made with `session.set`.

### Extensions

Extensions are shared by every session in the process, as they are in the
terminal. When an extension asks the person something (`ui.confirm`,
`ui.select`, `ui.input`, `ui.editor`) and the client said `hello` with
`ask: true`, the question arrives as an `ask` line and waits for
`ask.reply`; the `result` is what the extension's request expects
(`{"confirmed":true}`, `{"value":"a","label":"a"}`, `{"text":"…"}`), or
`{"cancelled":true}` when omitted:

```json
{"type":"ask","ask":1,"extension":"deploy","method":"ui.confirm","params":{"title":"Deploy?","message":"to prod"}}
{"id":"r1","method":"ask.reply","params":{"ask":1,"result":{"confirmed":true}}}
```

`ask` lines belong to the process, not a named session. A client relaying
questions to separate conversations must dedicate one RPC process to each,
as the Slack channel does. Do not infer the owner from the most recent prompt.

Without `hello`, or with `ask` false, questions are refused `no ui` at once,
as `e -p` refuses them. `ui.notify` and `ui.show` become `notice` lines
after `hello`; the remaining `ui.*` and `session.*` requests are refused —
they describe a terminal that is not there. Extension notices about
themselves (a crash, a missing package) are `{"type":"notice","message"}`
lines, also only after `hello`, so a client that never greeted never sees a
line it did not ask for.

### Version 1: one-shot lines

A line without a `method` is the original one-shot request and still works
unchanged: a fresh memory-only agent runs one turn and answers with the flat
result object, no events.

```json
{"id":"one","prompt":"summarize this repository","model":"openai/gpt-5.5","effort":"high","tool_mode":"none","tools":null,"save":false,"images":[]}
{"id":"one","output":"...","final_output":"...","model":"provider/model","effort":"high","aborted":false,"error":null,"error_details":null,"warnings":[],"usage":{…},"cost_usd":null,"tools":{"calls":2,"failures":0},"session":null}
```

`prompt` is required and non-empty; `save` true persists the turn and
`session` is then its JSONL path. Everything else is as for
`session.create`. One-shot responses come in the order the turns end, which
for a caller that waits for each response is input order.

### Process

Add `--no-tools` (`--nt`) for a no-tool policy and `--no-extensions`
(`--ne`) when startup must be hermetic. Tool batches run in bounded waves
(`tool_concurrency` in `~/.e/settings.json`, default 8, 1 to 64); calls
naming the same file run in provider order. A request line is at most
10 MiB. Malformed lines produce one `{"id":null,"error":"…"}` line and the
process keeps serving; an oversized line ends it, since the stream has no
safe resync point past it.

Usage categories in results are disjoint: `input_tokens` excludes cache
reads and writes, `prompt_tokens` is their complete sum, and compaction
requests are included. Terminal provider failures add `error_details`
(stage, provider metadata, retry decision, bounded diagnostic text) beside
the compatible `error` string; with a saved session the same details append
to a private `.errors.jsonl` sidecar. Nothing is uploaded.

EOF on stdin, or `shutdown`, stops every session, shuts extensions down, and
exits 0. On Unix, SIGTERM and SIGHUP kill every built-in bash process group
before extension shutdown, then exit with status 143 or 129 — detached
children of the shell included.
