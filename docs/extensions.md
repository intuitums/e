# Extensions

An extension is any executable file in `~/.e/extensions/`. Any language —
shell, Python, Rust, Node — the process boundary is the API. e starts each
one at launch, keeps it running for the whole session, and speaks a line
protocol over stdin/stdout: one JSON object per line.

Extensions can:

- **add tools** the model calls — and override a built-in by using its name
- **add slash commands** that show up in the `/` picker
- **rewrite or swallow a submitted line** with the `input` hook
- **name the session** from a command or tool result (shown in `/resume`)
- **gate tool calls** with the `tool_call` hook (return a block + reason)
- **observe events** (`turn_end`) and **notify** the transcript at any time
- **handle startup arguments** and request a same-binary relaunch in another directory

## Wire protocol (version 1)

e → extension, requests (each carries an `id` to answer with):

```
{"id":1,"method":"initialize","params":{"protocol":1,"e_version":"0.4.0-dev","cwd":"/path","extensions_config":{…}}}
{"id":2,"method":"hook.startup","params":{"cwd":"/path","argv":["--worktree","feature"]}}
{"id":3,"method":"tool_call","params":{"name":"greet","arguments":{...}}}
{"id":4,"method":"command","params":{"name":"ping","args":"rest of the line"}}
{"id":5,"method":"hook.tool_call","params":{"name":"bash","arguments":{...}}}
{"id":6,"method":"hook.input","params":{"text":"a submitted line"}}
```

e → extension, notifications (no `id`, no reply):

```
{"method":"event","params":{"name":"turn_end","extra":{"aborted":false}}}
{"method":"shutdown"}
```

extension → e:

```
{"id":1,"result":{...}}                        answer a request
{"id":2,"error":"what went wrong"}             or fail it
{"method":"notify","params":{"message":"hi"}}  a transcript notice, any time
```

## Results by method

**initialize** → the manifest. `tools`, `commands`, `flags`, and `hooks` are
all optional; `parameters` is a JSON Schema object. `extensions_config` in
initialize params carries every `~/.e/settings.json` entry under
`"extensions"`, namespaced by extension name — your config, without
squatting on a top-level key:

```json
{"name":"my-ext","version":"1.0",
 "tools":[{"name":"greet","description":"say hi","parameters":{"type":"object","properties":{}}}],
 "commands":[{"name":"ping","description":"check the extension"}],
 "flags":[{"name":"-w, --worktree","description":"run in a fresh worktree"}],
 "hooks":["tool_call","input"]}
```

`flags` are for discoverability only — e does not parse them (the startup
hook sees raw argv); they appear under **extension flags** in `e --help`.

**tool_call** → `{"content":"text the model sees","is_error":false,
"session_name":"optional new session name"}`

**command** → `{"notice":"line for the transcript"}` and/or
`{"prompt":"text submitted as the user"}`, and optionally
`{"session_name":"name shown in /resume"}`.

**hook.tool_call** → `{"block":true,"reason":"why"}` to stop the call
(the model sees the reason as an error result), `{"block":false}` to allow.

**hook.input** → decide what happens to a submitted line, in input-hook
order; the first extension to consume or replace wins:

```json
{"consume":true,"notice":"swallowed, with a notice"}
{"replace":"the rewritten line"}
{"consume":false,"replace":null}
```

An empty result allows the line through untouched. A pasted API key is
handled before the hook and never reaches it.

**hook.startup** → rewritten arguments and optional process changes:

```json
{"argv":["-c"],
 "env":{"REMOVE_ME":null},
 "relaunch":{"cwd":"/path/to/worktree","env":{"BOOTSTRAPPED":"1"}}}
```

Startup hooks run in extension filename order before e parses subcommands,
`-c`, `-r`, or the initial prompt. `argv` feeds the next hook. `env` changes
the current process. `relaunch` replaces the current process with the same e
binary in `cwd`; extensions cannot choose another executable. The first
relaunch ends the chain.

## Rules of the road

- The initialize answer must arrive within 5 s or the extension is skipped.
- Runtime hooks have 5 s and **fail open**: a slow or broken tool gate never
  blocks the agent. Startup hooks are different: an advertised startup hook
  that errors or times out stops launch, rather than leaking a consumed flag
  or branch name into the initial prompt.
- Tool calls have 300 s, commands 60 s.
- On quit e sends `shutdown`, waits a beat, then kills the process.
- A crashed or missing extension is reported in the transcript and skipped;
  it is never a reason e can't run.

## Examples

Two runnable examples ship in `examples/extensions/`:

- **`hello.mjs`** — every surface at once (command, tool, config, input
  hook, session naming) in ~90 lines of JavaScript.
- **`worktree.mjs`** — a minimal startup-hook launcher: `e -w` creates a
  Git worktree and relaunches e there.

Copy either to `~/.e/extensions/`, `chmod +x`, and restart e.

## A complete extension, in shell

`~/.e/extensions/ping.sh` (make it executable — `chmod +x`):

```sh
#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"id":%s,"result":{"name":"ping","version":"1","commands":[{"name":"ping","description":"are you there"}]}}\n' "$id" ;;
    *'"command"'*)
      printf '{"id":%s,"result":{"notice":"pong"}}\n' "$id" ;;
    *'"shutdown"'*) exit 0 ;;
  esac
done
```

Restart e, type `/ping`, get `pong`.

## What startup hooks are for

Because a startup extension sees raw argv and can relaunch the same binary in
a new cwd, it can implement things like managed Git worktree launches (`-w`
creating `<root>/<repo>/<branch>` and continuing there), project profiles, or
scratch-directory routing — all in any language the line protocol speaks,
with nothing hardcoded in e itself.
