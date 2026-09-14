# Extensions

An extension is an executable in `~/.e/extensions/`, or in the
`extensions/` directory of an installed [package](packages.md). It can be a
top-level file such as `foo.mjs`, or the entry point of a directory such as
`foo/` that also contains helper files. Directory entry-point selection checks `index.*`,
a file matching the directory name, then a sole executable. Path order breaks
a tie within one rule.

Extensions can use any language. e starts each process at launch, keeps it
running for the session, and exchanges one JSON object per line over stdin and
stdout.

Extensions can:

- **add tools** the model calls — and override a built-in by using its name
- **add slash commands** that show up in the `/` picker, and **shortcuts**
- **rewrite or swallow a submitted line** with the `input` hook
- **name the session** from a command or tool result (shown in `/resume`)
- **gate tool calls** with the `tool_call` hook (return a block + reason)
- **shape a turn** with the `before_turn`, `tool_result`, and
  `compact_summary` hooks
- **subscribe to the lifecycle**: session, turn, tool, compaction, model
- **show things**: a notice, a block of text, markdown, or a real diff, a
  tool row with its own verbs, a panel, a status slot
- **ask the user**: a pick list, a yes/no, a line of input
- **steer the session**: inject a message, narrow the toolset, switch the
  model or effort, interrupt, compact
- **handle startup arguments** and request a same-binary relaunch in another directory

Everything an extension shows is data that e paints through the user's
theme. An extension never emits terminal bytes and never runs inside e —
that is the difference from pi's in-process API, and the reason a crashed
or hostile extension is a notice rather than a broken terminal. The design
and its limits are recorded in
[decisions/0005](decisions/0005-extension-surface.md).

## Wire protocol (version 1 + capabilities)

e → extension, requests (each carries an `id` to answer with):

```
{"id":1,"method":"initialize","params":{"protocol":1,"capabilities":["tool.update","events","hooks","display","ui","session","shortcuts","pane","widget"],"ui":true,"e_version":"0.0.1","cwd":"/path","extensions_config":{…}}}
{"id":2,"method":"hook.startup","params":{"cwd":"/path","argv":["--project","../app"],"flags":{"project":"../app"}}}
{"id":3,"method":"tool_call","params":{"name":"greet","arguments":{...}}}
{"id":4,"method":"command","params":{"name":"ping","args":"rest of the line"}}
{"id":5,"method":"hook.tool_call","params":{"name":"bash","arguments":{...}}}
{"id":6,"method":"hook.input","params":{"text":"a submitted line"}}
{"id":7,"method":"hook.before_turn","params":{"prompt":"the user's message"}}
{"id":8,"method":"hook.tool_result","params":{"name":"bash","content":"…","is_error":false}}
{"id":9,"method":"hook.compact_summary","params":{"summary":"…"}}
{"id":10,"method":"shortcut","params":{"key":"ctrl+alt+g"}}
```

e → extension, notifications (no `id`, no reply):

```
{"method":"event","params":{"name":"turn_end","extra":{"aborted":false}}}
{"method":"flags","params":{"flags":{…}}}
{"method":"ui.key","params":{"key":"down"}}          while your interactive panel is open
{"method":"ui.panel_closed","params":{}}             the user closed it
{"method":"pane.select","params":{"pane":"diff","section":"files","id":"a.rs"}}   the side pane's cursor moved
{"method":"pane.activate","params":{"pane":"diff","section":"files","id":"a.rs"}} Enter on a pane item
{"method":"pane.key","params":{"pane":"diff","key":"x"}}                          a pane chord e did not use
{"method":"pane.closed","params":{"pane":"diff"}}                                 the user closed the pane
{"method":"shutdown"}
```

extension → e:

```
{"id":1,"result":{...}}                        answer a request
{"id":2,"error":"what went wrong"}             or fail it
{"method":"notify","params":{"message":"hi"}}  a transcript notice, any time
{"method":"tool.update","params":{"id":3,"stream":"stdout","chunk":"working\n"}}
{"id":"q1","method":"ui.select","params":{…}}  ask e something (see below)
```

A request from the extension carries the extension's own `id` — any JSON
value — and is answered with it: `{"id":"q1","result":{…}}` or
`{"id":"q1","error":"…"}`. The two id spaces never meet; direction tells
them apart. `capabilities` lists the families this e speaks; `ui` says
whether a person can answer `ui.*` requests (false under `e rpc`, where
every one is answered `{"error":"no ui"}` at once).

## Results by method

**initialize** → the manifest. Everything but `name` is optional;
`parameters` is a JSON Schema object. `extensions_config` in initialize
params carries every `~/.e/settings.json` entry under `"extensions"`,
namespaced by extension name — your config, without squatting on a
top-level key:

```json
{"name":"my-ext","version":"1.0",
 "tools":[{"name":"greet","description":"say hi","parameters":{"type":"object","properties":{}},
           "label":{"category":"greet","running":"Greeting","completed":"Greeted","target":"who"}}],
 "commands":[{"name":"ping","description":"check the extension"}],
 "flags":[{"name":"project","type":"string","description":"relaunch in this directory"},
           {"name":"plan","type":"boolean","description":"plan mode"}],
 "hooks":["tool_call","input","before_turn","tool_result","compact_summary"],
 "events":["session_start","turn_start","tool_end"],
 "shortcuts":[{"key":"ctrl+alt+g","description":"greet"}]}
```

A tool's `label` gives its transcript row the built-in grammar
(`Greeting bob` while running, `Greeted bob` after): `category` is the
batch tally's noun, the verbs are shown as given, and `target` names the
argument whose value the row shows. Without a label the row reads
`Running greet` / `Ran greet`.

`flags` are **declared** for discoverability and useful so e can *parse*
them. A flag with `"type":"boolean"` (the default) or `"type":"string"`
is recognized in startup argv — booleans match `--name`, `--name=true|false`,
`--no-name`; strings match `--name=value` or `--name value` (a following
`-` token is never consumed as a value). A bare string flag at end-of-argv
parses as `null` (flag present, no value). Last occurrence wins; `--` stops
parsing. A name that isn't a clean `--name` token (e.g. `"-x, --example"`)
appears in `e --help` but is never parsed — those flags still need the
startup hook's raw argv. After every startup hook has seen raw argv, e removes
typed flags and their separated string values before parsing its own
subcommands or constructing the initial prompt.

Parsed flags are sent to **every** extension that declares typed flags as a
`flags` notification right after launch (no reply needed) — so a tool-only
extension reads them from any handler, not just during startup. The
notification carries **only flags actually passed on the command line** — an
absent flag stays absent, so a handler can tell "passed false" from "not
passed". An optional `"default"` on a declaration is the value to use when
the flag is absent: e retains it but never fabricates it into the
notification, the extension applies it itself — the scaffold's `flag(name)`
does exactly that (the passed value, else the declared default, else
undefined), and `flagPassed(name)` is true only when the flag was on the
command line, regardless of default. The raw protocol gets
`{"method":"flags","params":{"flags":{…}}}`.

**tool_call** → `{"content":"text the model sees","is_error":false,
"session_name":"optional new session name","summary":"+12 -3",
"display":"…","format":"diff"}`. `content` is all the model reads.
`summary` is the row's suffix; `display` is what the ctrl+o viewer shows
instead of `content` (a built-in edit does the same with its full diff);
`format` is `text` (default), `markdown`, or `diff` — a unified diff, what
`git diff` prints, is converted to the viewer's row grammar with real line
numbers and coloured markers. Before that final response, an extension may
emit any number of `tool.update` notifications. Their `id`
must be the active tool-call request id, `stream` is `stdout` or `stderr`, and
`chunk` is displayed through the same ordered tool-output stream as built-in
commands. Version-1 extensions remain compatible; they simply never emit an
update. The scaffold passes tool handlers a second `{update}` argument:

```js
async tool({ arguments }, { update }) {
  update("starting\n");
  update("a warning\n", "stderr");
  return { content: "done" };
}
```

A command may declare `"arguments":"<env>"` — shown in the `/` picker, and
picking the command then leaves `/name ` in the composer for the user to
finish — and `"completions":true`, after which typing `/name pre` sends
`{"id":…,"method":"command.complete","params":{"name":"name","prefix":"pre"}}`
and the answer `{"items":[{"value":"prefix-match","label"?,"description"?}]}`
opens a picker whose choice replaces the prefix. Completions have three
seconds; a slow or empty answer shows nothing.

**command** → `{"notice":"line for the transcript"}`, `{"show":{"title":
"diff src/main.rs","body":"…","format":"diff"}}` (a block in the
transcript — see `ui.show`), and/or `{"prompt":"text submitted as the
user"}`, and optionally `{"session_name":"name shown in /resume"}`.

**shortcut** → the same result shape as a command. Sent to the extension
that declared the chord; see [Shortcuts](#shortcuts).

**pane.select / pane.activate / pane.key / pane.closed** are notifications
from the side pane; see [The side pane](#the-side-pane).

**hook.before_turn** → `{"system_suffix":"a paragraph appended to the
system prompt for this turn","message":{"content":"…","internal":true}}`.
The suffix is appended, never a replacement: the system prompt is the
user's file-backed contract with the model. The message is added to the
conversation before the request; `internal` (the default) keeps it out of
the transcript. Runs once per turn, before the first request, with the
prompt that started it.

**hook.tool_result** → `{"content":"what the model should read instead"}`
or `{}` to keep it. Runs after every tool, before the result is shown,
stored, or sent — redaction and trimming live here. Extensions see each
other's rewrites in declaration order. A rewrite also drops the tool's
richer `display` text, so the viewer shows exactly what you let through.

**hook.compact_summary** → `{"summary":"…"}` or `{}`. The generated summary
is about to replace the older conversation; this is the last word on it.

**hook.tool_call** → `{"block":true,"reason":"why"}` to stop the call
(the model sees the reason as an error result), `{"block":false}` to allow.

**hook.input** → decide what happens to a submitted line, in input-hook
order; the first extension to consume or replace wins:

```json
{"consume":true,"notice":"swallowed, with a notice"}
{"replace":"the rewritten line"}
{"consume":false,"replace":null}
```

An empty result allows the line through untouched; `{"notice":"…"}` allows
it and posts the notice. Notices from every extension that allowed the line
reach the transcript, alongside the notice of whichever finally consumed or
replaced it. A pasted API key is handled before the hook and never reaches
it.

**hook.startup** → rewritten arguments and optional process changes, given
`{cwd, argv, flags}` where `flags` are the parsed values of every typed
flag declaration:

```json
{"argv":["-c"],
 "env":{"REMOVE_ME":null},
 "relaunch":{"cwd":"/path/to/project","env":{"BOOTSTRAPPED":"1"}}}
```

Startup hooks run in extension filename order before e parses subcommands,
`-c`, `-r`, or the initial prompt. `argv` feeds the next hook. `env` changes
the current process. `relaunch` replaces the current process with the same e
binary in `cwd`; extensions cannot choose another executable. The first
relaunch ends the chain.

## Events

List the events you want in the manifest's `events`; e sends only those,
as notifications: `{"method":"event","params":{"name":"…","extra":{…}}}`.
A manifest without an `events` field is a version-1 extension and receives
`turn_end` alone.

```
session_start     {reason, path}       reason: startup | reload | new | resume | fork
session_shutdown  {reason}             reason: quit | reload | new | resume | fork
turn_start        {prompt}
turn_end          {aborted}
tool_start        {id, name, arguments}
tool_end          {id, name, outcome, content}
compact_start     {}
compact_end       {summary}
model_change      {model}
effort_change     {effort}
```

There are no per-token events. A pipe per delta is a cost with no
consumer; `tool_end` carries the finished text.

## Asking e: `ui.*` and `session.*`

The extension sends `{"id":<yours>,"method":"…","params":{…}}` and reads
the answer with the same id. Every request is bounded: at most 32
unanswered per extension (more are answered with an error), one modal at
a time across all extensions, text sanitized before paint, and sizes
clipped rather than refused (a long diff still shows, it ends early).

```
ui.notify   {message, tone?}                     → {}            tone: info | warning | error
ui.show     {title?, body, format}               → {}            a transcript block
ui.select   {title, options:[…]}                 → {value, label} | {cancelled:true}
ui.confirm  {title, message?}                    → {confirmed}
ui.input    {title, placeholder?, prefill?, secret?} → {text} | {cancelled:true}
ui.status   {text | null, key?}                  → {}            your slot on the status row (40 columns);
                                                                 `key` keeps several
ui.compose  {text}                               → {}            put text in the composer
ui.panel    {title, lines, interactive?} | null  → {}            a footer panel; null closes yours
ui.widget   {lines | null, key?}                 → {}            rows above the composer; null removes
ui.pane     {id?, title?, side?, hint?, sections} | null → {}    a side pane; null closes yours
```

`select` options are strings or `{label, description?, value?}` objects;
the picker is the same one `/` opens. `confirm` is a Yes/No picker.
`input` takes over the composer until Enter or Esc; `secret` masks it and
the text never reaches input hooks or the model.

`panel` lines are strings, or arrays of `{text, token}` spans painted with
the theme's colour for `token` (`dim`, `accent`, `success`, `warning`,
`error`, `userMessageText`, …; unknown tokens paint plain). At most 200
lines; one panel at a time — another extension's panel replaces yours,
and yours is told with `ui.panel_closed`. An `interactive` panel receives
the keyboard: every key arrives as `{"method":"ui.key","params":{"key":
"down"}}` (chords like `ctrl+x`, `shift+tab`, `escape` never — Esc closes
the panel and ctrl+c stays e's) and you redraw by sending `ui.panel`
again. That is pi's custom component, declaratively: you own the state
and the keys, e owns the frame.

`widget` rows use the same span grammar and sit above the composer, every
extension's together in key order, eight rows at most; `{"lines": null}`
removes one. `status` with a `key` keeps several slots per extension; the
status row's template (`docs/layout.md`) joins them with `{status}` or
picks one extension's with `{status:<name>}`.

### The side pane

`ui.pane` opens a pane beside the conversation — the surface a diff
review, a plan, a test runner, or a log wants. You send content; e owns
the split, focus, scrolling, the cursor, selection, and the mouse, so
every pane navigates alike and none can paint outside its column.

```json
{"id": "diff", "title": "Changes", "side": "right", "sections": [
  {"kind": "list", "id": "files", "selected": "src/main.rs",
   "items": [{"id": "src/main.rs", "label": "src/main.rs", "detail": "+12 -3"}]},
  {"kind": "diff", "id": "patch", "body": "diff --git a/src/main.rs …"}
]}
```

Sections are `list` (selectable rows: `{id, label, detail?, token?}`, or
plain strings), `diff` (a unified diff, painted in e's row grammar),
`text`, `markdown`, and `rows` (the panel's span lines). Lists show eight
rows and scroll; the other kinds share the remaining height. The whole
pane holds 256 KiB; past that the rest is dropped and the last row says
so. Send `ui.pane` again with the same `id` to refresh — the user's place
in every section that kept its `id` is preserved — and `null` to close.

What the user does comes back as notifications:

```
{"method":"pane.select",  "params":{"pane":"diff","section":"files","id":"src/main.rs"}}  the cursor moved to an item
{"method":"pane.activate","params":{"pane":"diff","section":"files","id":"src/main.rs"}}  Enter on an item
{"method":"pane.key",     "params":{"pane":"diff","key":"x"}}                               a chord e did not use
{"method":"pane.closed",  "params":{"pane":"diff"}}                                         the user closed it
```

The keys e uses while the pane has focus: `↑`/`↓` and `j`/`k`, `PageUp`,
`PageDown`, `Home`, `End`, `←`/`→` to scroll a wide diff, `Tab` between
sections, `Enter` (on a list: activate and move to the next section; on
anything else: attach the selected rows to the composer as a snapshot),
`Shift` with a movement or a mouse drag to select rows, `Esc` back to
the first section and then close. The layout's focus chord (`ctrl+t` by
default) moves between the conversation and the pane; on a terminal too
narrow to split, the focused one fills the screen and the status row
says how to reach the other. `side` is a proposal: the user's
`~/.e/layout.json` decides where every pane goes and how wide it is.

```
session.send      {content, internal?, run?, when?} → {}  internal: model sees it, transcript does not;
                                                         run: start (or steer) a turn — default true
                                                         for visible messages, false for internal;
                                                         when: "next_turn" holds an internal message
                                                         until the user's next prompt and sends it
                                                         just ahead of it. While a turn runs only
                                                         run: true (a steer) or when: "next_turn"
                                                         is accepted; run: false is an error then
session.info      {}                          → {path, id, name, cwd, model, effort, running,
                                                  tools, context_tokens, context_window}
session.name      {name}                      → {}
session.model     {model}                     → {} | error   the same path /model takes
session.effort    {effort}                    → {} | error   one of the model's levels
session.tools     {names | null}              → {}           narrow the toolset; null restores
session.interrupt {}                          → {}
session.compact   {focus?}                    → {}
```

`session.tools` is how a plan mode works: `["read","grep"]` and the model
can neither see nor call anything else until `null`. It covers built-in
and extension tools alike and is enforced at execution, not only in what
the request advertises. It resets on `/new` and resume.

Not offered, on purpose: rewriting the provider request or its headers,
replacing the system prompt, custom providers, replacing the session
(`/new`, `/resume`, `/tree` are the user's), and per-token streams. Each
is explained in [decisions/0005](decisions/0005-extension-surface.md).

## Shortcuts

Declare `shortcuts` in the manifest; a chord the user presses arrives as
`{"id":…,"method":"shortcut","params":{"key":"ctrl+alt+g"}}` and is answered
like a command. Chords need `ctrl` or `alt`; bare keys and shift-only
chords are how text gets typed and are refused at the manifest. e keeps
`ctrl+c`, `ctrl+d`, `ctrl+g`, `ctrl+i`, `ctrl+j`, `ctrl+l`, `ctrl+m`,
`ctrl+o`, `ctrl+p`, `ctrl+shift+p`, `ctrl+s`, `ctrl+v`, `ctrl+shift+v`,
`ctrl+x`, and `ctrl+z`. A chord the composer binds (`ctrl+k`, say — see
`docs/keybindings.md`) stays the composer's: a shortcut fires only when the
key would otherwise do nothing, so a user frees a chord for your extension
by unbinding it in `keybindings.json`. First declaration wins between
extensions, with a notice.

## Rules of the road

- The initialize answer must arrive within 5 s or the extension is skipped.
- Runtime hooks have 5 s and **fail open**: a slow or broken tool gate never
  blocks the agent, a silent `before_turn` adds nothing, a silent
  `tool_result` changes nothing. (pi's `tool_call` is fail-closed; e's is
  not, by design — answer `{"block":true}` yourself when unsure.) Startup
  hooks are different: an advertised startup hook that errors or times out
  stops launch, rather than leaking a consumed flag or branch name into the
  initial prompt.
- Your `ui.*` / `session.*` requests have no timeout — a person answers
  them — but a reload, a session switch, or shutdown answers every open one
  with an error rather than leaving you waiting.
- Tool calls have 300 s, commands 60 s.
- On quit e sends `shutdown`, waits a beat, then kills the process.
- A crashed or missing extension is reported in the transcript and skipped;
  it is never a reason e can't run. An exit immediately after a valid initialize
  response still emits one notice, including which runtime hooks now fail open.

## Examples

```
docs/extensions/
  subagent.mjs   bounded delegated e turns as a tool, over e rpc (self-contained)
  hello.mjs      every surface at once, on the optional scaffold helper
  gate.mjs       the tool_call hook as a fail-open guard
  protected.mjs  the tool_call hook denying credential-shaped paths
  project.mjs    a startup-hook directory router (e --project <path>)
  mcp.mjs        one MCP stdio server's tools as extension tools
  scaffold.mjs   an optional wire-protocol helper (not required, never installed)
  plan.mjs       a plan mode on the new surface: session.tools, a shortcut,
                 ui.select, a status slot, and a panel
```

An extension speaks the protocol directly — `subagent.mjs` and the shell
`ping.sh` below are single self-contained files, reading a JSON request per
line and writing a response per line. e installs nothing beside an extension.

## Compiled extensions

Extensions are programs, not scripts only: anything that speaks the line
protocol qualifies, including a compiled binary. A compiled extension lives
in its own repository, like every package, and reaches users as a release
package (`e install release:<owner>/<repo>/<name>`, see
[packages.md](packages.md)); the e repository ships no extensions of its own.
What an extension sends still crosses the line as data: notices are sanitized before paint, so an
extension that wants colour returns a `show` with a `format` rather than
styled bytes.

**`scaffold.mjs`** is an *optional* convenience: the same stdin/stdout framing,
id routing, and a `connect({ manifest, handlers })` wrapper, so you write
handlers instead of a read loop. If you want it, drop it into your extension's
own bundle directory and `import { connect } from "./scaffold.mjs"` — it is
never installed for you, and it is not a thing you have to think about.

- **`hello.mjs`** — every surface at once, on the optional scaffold: command,
  tool, config, input hook, session naming — ~50 lines of handlers.
- **`gate.mjs`** — the `tool_call` hook as a guard, in e's fail-open
  shape: only an explicit block stops a call; a slow or crashed
  extension never blocks the agent.
- **`protected.mjs`** — the `tool_call` hook denying any call (`read`,
  `write`, `edit`, `grep`, `bash`) that touches a credential-shaped path —
  `~/.ssh`, `~/.aws`, `~/.gnupg`, `.env*`, `*.pem`, `*.key` — whether that's
  the tool's `path` argument or a bash command mentioning one. Unlike
  `gate.mjs`'s destructive-command denylist, this one is about what gets
  read into context or written to disk, not just what bash runs. See
  [`docs/sandboxing.md`](sandboxing.md) for e's trust model and where a
  hook like this fits.
- **`project.mjs`.** This startup-hook launcher uses the scaffold.
  `e --project <path>` relaunches e in an existing project directory.
- **`subagent.mjs`.** Its `delegate` tool drives a single-shot `e rpc
  --no-extensions` child with one JSON request line in and one result out. The
  delegated turn is extension-free, so it cannot delegate again. It defines
  `Explore`, `Plan`, and `Build` in the extension and sends each agent's
  `tools` and optional `model` in the RPC request. Core stays generic and does
  not have an agent type.
- **`mcp.mjs`** — a dependency-free bridge from one configured MCP stdio
  server's `tools/list` / `tools/call` surface into e extension tools. It
  forwards MCP progress through the additive `tool.update` capability.

To use one, put it in `~/.e/extensions/` (a top-level executable file, or a
subdirectory bundling it and its helpers), make it executable, and restart e.
To share one, put it in a repository's `extensions/` directory and others
install it with `e install git:<host>/<user>/<repo>` — see
[packages.md](packages.md).
An example that uses the optional scaffold helper needs `scaffold.mjs` beside
it in its bundle; the self-contained ones (`subagent.mjs`, `ping.sh`) need
nothing.

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

## MCP tools

Put `mcp.mjs` in `~/.e/extensions/` with `scaffold.mjs` beside it, then
configure the stdio server e should own in `~/.e/settings.json`:

```json
{
  "extensions": {
    "mcp": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/safe/root"]
    }
  }
}
```

`npx -y` downloads the server on first use, which routinely takes longer
than the 5 s initialize budget — the bridge is then skipped with
`initialize timed out` until the package is cached. Run the `npx` line once
by hand first, or point `command` at an installed binary.

The bridge intentionally maps only MCP tools. Prompts, resources, sampling,
elicitation, and authorization stay out of e's core and out of this example.
It uses the 2025-11-25 initialize/initialized stdio lifecycle supported by
current SDK legacy/default mode, newline-delimited JSON-RPC, paginated
`tools/list`, and `tools/call`. See the [MCP lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle),
[transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports),
and [tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
specifications.

## Delegated turns

Put `subagent.mjs` in `~/.e/extensions/` and restart — it is a single
self-contained file, nothing beside it. The model gains a `delegate` tool. Each
delegation is a single-shot `e rpc` child in the same working directory: the
extension writes one JSON request line, reads one result object, and closes
stdin. The child loads no extensions, so it cannot delegate again. Set `E_BIN`
when the child should use an e binary other than the one on `PATH`.

`timeout_seconds` defaults to 240 seconds. At the deadline the extension sends
SIGTERM, and `e rpc` kills every active built-in bash process group before it
exits. A later SIGKILL remains as a watchdog if graceful shutdown stalls.

The delegation sets `save: true`. The response includes the saved JSONL path,
and the tool result gives that path to the parent. The parent can read it when
the final answer omits a useful tool call or result.

### Agents live in the extension

A delegation can name an `agent` defined in `subagent.mjs`. Each agent chooses
a built-in tool allowlist and an optional model. The child receives the task as
its user message and uses e's normal system prompt with a generic tool-policy
suffix. Core does not have an agent type.

The extension defines these agents:

- `Explore` can use `read` and `grep`.
- `Plan` can use `read` and `grep`.
- `Build` can use every built-in tool.

Each object has `name`, `description`, optional `tools`, and optional `model`.
The shipped `"{provider/model}"` values are placeholders. Until you replace
one, that child uses the model `e rpc` normally resolves from configuration.
A call can also pass `model` to override the selected agent. The core validates
`tools` as built-in names, advertises only those schemas, and enforces the same
list when a provider emits a tool call.

## What startup hooks are for

Because a startup extension sees raw argv and can relaunch the same binary in
a new cwd, it can implement project-directory routing (`--project <path>`),
project profiles, or scratch-directory routing. Any language that speaks the
line protocol can add these behaviors without hardcoding them in e.
