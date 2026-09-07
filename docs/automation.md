# Automation

For headless use, `e rpc` speaks sequential JSONL over stdin and stdout: one
request object per input line, exactly one response object per line out.
Extensions are initialized once and reused; each request line gets a fresh
agent. Requests are memory-only unless `save` is explicitly true. Add
`--no-tools` (`--nt`) for a no-tool policy and `--no-extensions` (`--ne`)
when process startup must be hermetic.

A request stays active through automatic compaction and continuation. The
response describes the completed run, not an intermediate context pause.
If compaction cannot finish safely, the response contains an error and
`final_output` is empty. Background tool handles belong to that request's
agent and cannot be queried by a later request.

Tool batches run in bounded waves. `tool_concurrency` in `~/.e/settings.json`
defaults to 8 and accepts values from 1 to 64. Calls naming the same file run
in provider order, including symlinks and Unix hard links. Bash and extension
tools have opaque effects; dependent commands should be sent in separate
batches or combined into one command.
Cancellation skips waves that have not started and records their calls as
cancelled. A checkpoint is installed only if its source history still matches;
messages committed during summarization are preserved and compaction reports
an error instead of replacing them with a stale summary.

```json
{"id":"one","prompt":"summarize this repository","model":"openai/gpt-5.5","effort":"high","tool_mode":"none","save":false,"images":[]}
```

Fields:

- `id` is any JSON value and is copied to the response.
- `prompt` is required and non-empty.
- `model` and `effort` override process defaults from `-m` / `--ef`.
- `tool_mode` is `all` or `none`.
- `tools` is a positive built-in allowlist. `null` is the full built-in and
  extension set. A list advertises and executes only those built-ins, and an
  unknown name makes the request fail. The system prompt gets a generic policy
  suffix naming the allowed tools. `tool_mode: "none"` still takes precedence.
- `save` defaults to false.
- `images` is a list of PNG, JPEG, GIF, or WebP paths, up to ten files,
  20 MiB each, and 40 MiB total. The selected model must declare image input
  support.

Every non-empty input line produces exactly one output line carrying the
accumulated output, terminal error/abort state, warnings, token usage,
optional estimated cost, and tool counts, plus the request's `id`:

```json
{"id":"one","output":"...","final_output":"...","model":"provider/model","effort":"high","aborted":false,"error":null,"warnings":[],"usage":{"input_tokens":1200,"output_tokens":80,"cache_read_tokens":900},"cost_usd":null,"tools":{"calls":2,"failures":0},"session":null}
```

`session` is the saved turn's JSONL path when `save` was true (the whole
transcript — every tool call and its output — lives there, so a caller that
needs more than `final_output` can read it), or `null` for a memory-only run.

Malformed requests also produce one line (`{"id":null,"error":"..."}`) and
do not terminate the process. EOF shuts down extensions and exits cleanly.
On Unix, SIGTERM and SIGHUP kill every built-in bash process group before
extension shutdown, then exit with status 143 or 129. This includes detached
children of the shell, not only the direct bash process.

The protocol is sequential by design: response order is input order, so a
caller never needs an out-of-band event channel or stream correlation.
