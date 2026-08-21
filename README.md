<div align="center">

# 𝑒

**A coding agent for your terminal.**

One small, fast Rust binary — its own agent loop, tools, and sessions.
Grayscale, keyboard-driven, nothing phones home.

</div>

## Start

```sh
cargo build --release
ln -s "$PWD/bin/e" ~/.local/bin/e     # optional
```

Then sign in and go:

```
/login        account or API key
e             open a session here
e -c          continue the last one
```

## Keys

| | |
|:--|:--|
| `/` `@` `$` | commands · files · skills |
| `Enter` while streaming | steer the running turn |
| `Esc` | interrupt |
| `Ctrl-C` ×2 | exit |

## Commands

`/login` · `/model` · `/resume` · `/new` · `/copy` · `/settings` · `/help`

## Tools

The model runs these itself, mid-turn, no gate:
`read` · `write` · `edit` · `ls` · `grep` · `bash` · `skill`

## Home

Everything lives under `~/.e/`, all optional:

| | |
|:--|:--|
| `settings.json` | preferences, incl. a `system_prompt` override |
| `auth.json` | credentials, written by `/login` |
| `AGENTS.md` | instructions for the agent — empty by default, yours to fill |
| `sessions/` | one JSONL log per conversation |
| `skills/` | `SKILL.md` directories |

## Develop

```sh
cargo test      # the byte-pinned visual + behavioral suite
cargo run
```

More in [DESIGN.md](DESIGN.md).

---

<div align="center">
<sub>Interface based on <b><a href="https://github.com/vercel-labs/fx">fx</a></b> by Vercel · <a href="LICENSE">MIT</a></sub>
</div>
