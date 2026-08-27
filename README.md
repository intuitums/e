<div align="center">

# 𝑒

**A coding agent for your terminal.**

One small, fast binary — extend it with your own tools, commands, themes, and skills.

```sh
cargo install --git https://github.com/intuitumxyz/e
```

</div>

> [!NOTE]
> 𝑒 is under active development — expect breaking changes and rough edges.

`e` keeps the harness small: four normalized provider dialects, one ordered
agent event stream, five built-in tools, append-only branchable sessions, and
executable JSONL extensions. Provider definitions and model capabilities are
data, while authentication, live catalogs, and wire adapters stay separate.

```sh
e                                      # interactive session
e --read-only --no-save "review this" # safe ephemeral session (`--ro --ns`)
e ask --json "one turn"               # one machine-readable result
e rpc                                  # persistent JSONL automation
e doctor                               # paste-safe provider/runtime report
```

Images, token/cost reporting, reasoning effort, session rewind/branching,
streamed built-in and extension tools, themes, skills, prompts, and model
overrides ship in the core product. MCP and delegated agents are concrete
extensions under `docs/extensions/`, keeping those orchestration policies out
of the harness itself.

<div align="center">

· · ·

<sub>Interface based on <b><a href="https://github.com/vercel-labs/fx">fx</a></b> by Vercel · <a href="LICENSE">MIT</a></sub>

</div>
