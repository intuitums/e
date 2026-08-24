# Models

`~/.e/models.json` adds models and corrects built-ins. An entry with a
built-in's provider and id replaces it — the file wins, like themes.

```json
{
  "providers": {
    "local": {
      "base_url": "http://localhost:8080/v1",
      "api": "openai-completions",
      "context_window": 64000,
      "models": [
        "small-model",
        { "id": "big-model", "context_window": 1000000 }
      ]
    }
  }
}
```

- `base_url` is required for a new provider. Entries for built-in providers
  may omit it and inherit that provider's endpoint; e never guesses another
  provider's host.
- `api`: `openai-completions` (default), `openai-responses`, or
  `anthropic-messages`.
- `context_window` may sit on the provider (default for its models) or on a
  model object; it drives the statusline percentage and auto-compaction, so
  set it truthfully. Default: 200000.
- `efforts` on a model object declares its reasoning levels, in cycle order —
  shift+tab walks exactly this list (e.g. `["low", "medium", "high",
  "xhigh"]`). Built-ins carry their own; a file entry without `efforts` has
  no reasoning knob.
- Credentials: `/login <provider>` stores an API key for any provider name.
- Only models whose provider has credentials appear in `/models`; scope a
  cycling shortlist with `/scoped-models` (ctrl+p cycles).

## Credentials

`/login` stores keys in `~/.e/auth.json`. A provider with no stored
credential falls back to its conventional environment variable —
`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, `OPENCODE_API_KEY`,
`OPENCODE_GO_API_KEY` — which is what CI and scripts want. `auth.json`
wins when both exist.

## The catalog is live

Signed-in providers are asked for their model list (`GET {base}/models`)
in the background — at launch, after a sign-in, and when `/models` opens —
so a model a gateway ships today appears today, no e release involved.
Windows the gateway reports win; otherwise new models default to 200k,
correctable here in `models.json` (which always wins a name clash).
