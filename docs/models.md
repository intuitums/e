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

- `api`: `openai-completions` (default), `openai-responses`, or
  `anthropic-messages`.
- `context_window` may sit on the provider (default for its models) or on a
  model object; it drives the statusline percentage and auto-compaction, so
  set it truthfully. Default: 200000.
- Credentials: `/login <provider>` stores an API key for any provider name.
- Only models whose provider has credentials appear in `/models`; scope a
  cycling shortlist with `/scoped-models` (ctrl+p cycles).
