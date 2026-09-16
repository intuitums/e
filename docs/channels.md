# Channels

A channel puts e where a team already works: a Slack thread, a Linear
issue, a GitHub pull request. e does not ship channels inside the binary.
Each one is a small program of its own that spawns `e rpc`, maps the
platform's conversations to e sessions, and relays events back — the same
way the terminal frontend is a consumer of the core, not part of it. This
keeps the binary every developer installs free of Slack tokens, webhooks,
and bot frameworks, and lets a company write its channel in whatever
language its glue code already uses.

The protocol is [automation.md](automation.md). Reference channels live
under [`channels/`](../channels/) in the repository:

```
channels/slack/     a Slack bot: one thread, one session (TypeScript)
channels/github/    a GitHub Actions workflow answering `/e` on issues and PRs
```

## The shape of a channel

Every channel does the same five things.

1. **Spawn `e rpc`** with the pipes kept open, and say `hello`. When relaying
   extension questions, use one process per conversation: `ask` lines do
   not carry a session ID. A client without interactive extensions can
   share one process across sessions. Pass `--no-save` or `--no-tools` if
   the deployment wants them; those bounds hold for every session.
2. **Map a conversation to a session.** A Slack thread, a Linear issue, or
   a PR is one `session.create` with the repository's checkout as `cwd`,
   `save: true` so the conversation survives a restart, and a `name` the
   team recognizes. Keep session IDs in memory and save the mapping from
   thread ID to log path. After a restart, `session.create` with `resume`
   brings the thread back with its history and a new session ID.
3. **Turn a message into a prompt.** `session.prompt` with the text. While
   the turn runs, the `tool_batch` and `tool_end` events are what a person
   wants to see in the thread ("Reading src/main.rs", "Ran tests"); `text`
   deltas accumulate into the reply. The response line carries the final
   text, usage, and cost.
4. **Relay questions.** With `hello {ask: true}`, an extension's
   `ui.confirm` or `ui.select` arrives as an `ask` line. Post it as a
   message with buttons, and answer with `ask.reply` when someone clicks.
   Until then the tool waits.
5. **Stop.** `session.interrupt` for a cancel reaction, `session.close`
   when a thread is archived, and stdin EOF or `shutdown` when the channel
   process stops.

A channel never parses terminal output and never touches `~/.e` itself.
Everything it needs — models, sessions, history, an HTML export to attach —
is a method.

## Slack

`channels/slack/` is the reference: a Bolt app in socket mode, so it needs
no public URL. It answers when mentioned in a channel and continues in the
thread; each thread owns one process and session against the checkout in
`E_CWD`. Tool
progress is posted as it happens, the reply when the turn ends, and
extension questions become a message with buttons. See its README for the
app manifest and the three Slack credentials, and how to run it.

The channel publishes with e's releases as `@intuitums/e-slack`, so
`npx @intuitums/e-slack` runs it against any checkout without cloning this
repository. Its version matches the release it came from.

On a server, `channels/slack/Dockerfile` builds an image carrying e from the
release and the bot from the repository; each release publishes it as
`ghcr.io/intuitums/e-slack`, so the usual case needs neither Node nor a
checkout. Mount the checkout at `/work` and e's home on a volume, and record the
trust decision once (`e trust`): a channel has no terminal, so nothing else can
answer the panel that gates the repository's own instructions.

## GitHub

`channels/github/e.yml` is a workflow that runs on issue and pull-request
comments containing `/e`. It first verifies that the commenter has repository
write, maintain, or admin permission. Only then does it check out the repository,
install e, and run
`e -p --json` with the comment as the prompt, posting the reply as a comment.
One turn per comment is the right shape for CI: nothing long-lived, the
checkout is the working directory, and the provider key is a repository
secret. A run that needs the conversation to continue across comments can
use `e rpc` with `save: true` and cache `~/.e/sessions` between runs.

## Linear and the rest

A Linear channel is the Slack channel with a webhook instead of a socket:
an issue is a session, a comment is a prompt, and the reply is a comment.
Nothing in e distinguishes the platforms; the difference is entirely in
the adapter. Write it against `channels/slack/` as the pattern.
