# Extensions

An extension is any executable file in `~/.e/extensions/`. Any language —
shell, Python, Rust, Node — the process boundary is the API. e starts each
one at launch, keeps it running for the whole session, and speaks a line
protocol over stdin/stdout: one JSON object per line.

Extensions can:

- **add tools** the model calls — and override a built-in by using its name
- **add slash commands** that show up in the `/` picker
- **gate tool calls** with the `tool_call` hook (return a block + reason)
- **observe events** (`turn_end`) and **notify** the transcript at any time

## Wire protocol (version 1)

e → extension, requests (each carries an `id` to answer with):

```
{"id":1,"method":"initialize","params":{"protocol":1,"e_version":"0.3.0","cwd":"/path"}}
{"id":2,"method":"tool_call","params":{"name":"greet","arguments":{...}}}
{"id":3,"method":"command","params":{"name":"ping","args":"rest of the line"}}
{"id":4,"method":"hook.tool_call","params":{"name":"bash","arguments":{...}}}
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

**initialize** → the manifest. `tools`, `commands`, and `hooks` are all
optional; `parameters` is a JSON Schema object.

```json
{"name":"my-ext","version":"1.0",
 "tools":[{"name":"greet","description":"say hi","parameters":{"type":"object","properties":{}}}],
 "commands":[{"name":"ping","description":"check the extension"}],
 "hooks":["tool_call"]}
```

**tool_call** → `{"content":"text the model sees","is_error":false}`

**command** → `{"notice":"line for the transcript"}` and/or
`{"prompt":"text submitted as the user"}`

**hook.tool_call** → `{"block":true,"reason":"why"}` to stop the call
(the model sees the reason as an error result), `{"block":false}` to allow.

## Rules of the road

- The initialize answer must arrive within 5 s or the extension is skipped.
- Hooks have 5 s and **fail open**: a slow or broken hook never blocks the
  agent — only an explicit `"block":true` does.
- Tool calls have 300 s, commands 60 s.
- On quit e sends `shutdown`, waits a beat, then kills the process.
- A crashed or missing extension is reported in the transcript and skipped;
  it is never a reason e can't run.

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
