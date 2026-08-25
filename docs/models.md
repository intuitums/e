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
- `api`: `openai-completions` (default), `openai-responses`, `codex-responses`,
  `anthropic-messages`, or `google-generative-ai`. The short aliases
  `completions`, `responses`, `anthropic`, and `google` are accepted too;
  any other name is a load error.
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
`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `XAI_API_KEY`,
`GROQ_API_KEY`, `MISTRAL_API_KEY`, `DEEPSEEK_API_KEY`, `CEREBRAS_API_KEY`,
`OPENROUTER_API_KEY`, `TOGETHER_API_KEY`, `FIREWORKS_API_KEY`,
`OPENCODE_API_KEY`, `OPENCODE_GO_API_KEY`, `AI_GATEWAY_API_KEY` — which is
what CI and scripts want. `auth.json` wins when both exist.

Local backends (Ollama on `localhost:11434`, LM Studio on `localhost:1234`)
need no credential at all: they are always signed in, and their models
appear as soon as the local server answers `/models`.

## The catalog is live

Signed-in providers are asked for their model list (`GET {base}/models`)
in the background — at launch, after a sign-in, and when `/models` opens —
so a model a gateway ships today appears today, no e release involved.
Windows the gateway reports win; otherwise new models default to 200k,
correctable here in `models.json` (which always wins a name clash).
