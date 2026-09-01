# Models

`~/.e/models.json` adds models and corrects built-ins. An entry with a
built-in's provider and id replaces it — the file wins, like themes.

```json
{
  "providers": {
    "local": {
      "base_url": "http://localhost:8080/v1",
      "api": "openai-completions",
      "responses_mount": "platform",
      "context_window": 64000,
      "supports_tools": true,
      "models": [
        "small-model",
        {
          "id": "big-model",
          "context_window": 1000000,
          "image_input": false,
          "pricing": {
            "input_per_million": 1.0,
            "output_per_million": 4.0,
            "cache_read_per_million": 0.1
          }
        }
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
- `responses_mount` explicitly selects `platform` (default,
  `{base_url}/responses`) or `codex` (`{base_url}/codex/responses` plus the
  ChatGPT account headers). It only affects a Responses dialect and is never
  inferred from whether the stored credential happens to be a key or OAuth.
- `catalog` controls only live model discovery and is independent from
  `api`: `openai` (default, `GET /models` + `data[].id`), `anthropic`
  (`GET /v1/models` + x-api-key), `google` (`models[].name` + x-goog-api-key),
  or `none`. This separation matters for gateways that accept one inference
  dialect but expose another provider's catalog shape.
- `context_window` may sit on the provider (default for its models) or on a
  model object; it drives the statusline percentage and auto-compaction, so
  set it truthfully. Default: 200000.
- `max_output` may sit on the provider or a model object; it caps the
  reply-token ceiling for models whose real limit is below the dialect's own
  default (e.g. a small Anthropic model). Only the Anthropic dialect reads
  it today. Default: the dialect's own constant.
- `efforts` on a model object declares its reasoning levels, in cycle order —
  shift+tab walks exactly this list (e.g. `["low", "medium", "high",
  "xhigh"]`). Built-ins carry their own; a file entry without `efforts` has
  no reasoning knob.
- `supports_tools` (default `true`) and `image_input` (default `false`) are
  capabilities, set at provider or model level. A model declared without tool
  support is sent no schemas and cannot execute a tool even if it emits one.
  Live-discovered ids inherit the provider-level defaults, never an arbitrary
  declared sibling model's override.
- `pricing` declares USD rates per million input, output, and optionally
  cache-read tokens. e shows a turn estimate and includes `cost_usd` in
  the `e rpc` response. If cache pricing is omitted, cache reads use the normal
  input rate. Pricing is optional because it changes independently of the
  wire protocol; use the provider's current published rates.
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
