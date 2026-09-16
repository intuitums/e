---
title: Models & providers
description: models.json: extra models, context windows, dialects
order: 1
---

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
            "cache_read_per_million": 0.1,
            "cache_write_5m_per_million": 1.25,
            "cache_write_1h_per_million": 2.0
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
  `chatgpt` (the ChatGPT backend's picker: `models[].slug` with the `-wm`
  suffix stripped, work-mode entries only, `max_tokens` as the context
  window), or `none`. This separation matters for gateways that accept one
  inference dialect but expose another provider's catalog shape.
- `context_window` may sit on the provider (default for its models) or on a
  model object; it drives the statusline percentage and auto-compaction, so
  set it truthfully. Default: 200000.
- `max_output` may sit on the provider or a model object; it caps the
  reply-token ceiling for models whose real limit is below the dialect's own
  default (e.g. a small Anthropic model). Only the Anthropic dialect reads
  it today. Default: the dialect's own constant.
- `effort` on a model object declares its reasoning levels, in cycle order —
  shift+tab walks exactly this list (e.g. `["low", "medium", "high",
  "xhigh"]`). A model entry without `effort` inherits its provider default,
  then what models.dev states for the id, then its built-in declaration;
  otherwise it has no reasoning knob. Levels are the exact strings sent as
  `reasoning_effort` (or the dialect's equivalent), so they must match what
  the backend accepts — e.g. opencode-go's `glm-5.3-flash` takes `["low",
  "high", "max"]` (no `medium`), a set the gateway's own list does not
  advertise.
- `supports_tools` (default `true`) and `image_input` (default `false`) are
  capabilities, set at provider or model level. A model declared without tool
  support is sent no schemas and cannot execute a tool even if it emits one.
  Live-discovered ids take what models.dev states for them, else the
  provider-level defaults, never an arbitrary declared sibling model's
  override. An explicit provider `image_input` setting wins over feed facts
  for discovered ids too.
- `pricing` declares USD rates per million uncached input and output tokens.
  Optional cache-read, five-minute cache-write, and one-hour cache-write rates
  keep prompt caching priced separately. An omitted cache rate falls back to
  ordinary input rather than dropping those tokens. e shows a turn estimate
  and includes `cost_usd` in the `e rpc` response. Pricing is optional because
  it changes independently of the wire protocol; use the provider's current
  published rates.
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

Most of those lists carry nothing but ids. The facts come from
[models.dev](https://models.dev), the community catalog opencode and pi
generate their provider files from: it is fetched in the same refresh,
trimmed to e's providers, and cached in `~/.e/models-dev.json`. For every
model it knows — built-in seed or freshly discovered id — it sets the
context window, the effort levels, whether reasoning is adaptive or a
token budget (the Anthropic thinking shape), image and tool support, and
pricing. It never adds ids: which models a provider serves is the
provider's word. It does not set `max_output`.

Precedence, lowest to highest: the built-in seed, the models.dev facts, a
window the gateway itself reports, and `models.json`. A seed is only the
offline fallback; a wrong fact is fixed upstream, not pinned in e. An
explicit `models.json` value is final and survives every refresh and e
update, and a partial entry inherits the facts for what it leaves unsaid,
including when the model has no built-in seed.
