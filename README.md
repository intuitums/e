<div align="center">

# 𝑒

**A coding agent for your terminal.**

One small, fast binary—extend it with your own tools, commands, themes, and
skills.

</div>

> [!NOTE]
> e is under active pre-1.0 development. Persisted formats and extension
> compatibility are versioned, but documented breaking changes can still
> occur.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/intuitums/e/main/install.sh | sh
```

`install.sh` selects the matching binary, verifies its checksum, and installs
it under `~/.local/bin`. Published releases support:

| Operating system | Architectures | Status |
|---|---|---|
| Linux (glibc) | x86-64, ARM64 | release-built; native-runner smoke test |
| macOS | Apple Silicon, Intel | release-built; native-runner smoke test |
| Windows | — | under consideration; not currently supported |

After this one manual install, e updates itself (`e update`, or the
launch-time background check; /reload switches to a downloaded update in
place). Or install the pinned source with Rust:

```sh
cargo install --locked --git https://github.com/intuitums/e
```

Release assets also include a CycloneDX SBOM and signed GitHub build
provenance. See [release verification](docs/releases.md).

## Start

```sh
e                           # open the terminal interface
e "inspect this project"    # open with an initial request
e ask "explain src/main.rs" # one non-interactive turn
e doctor --no-network       # redacted local support report
```

Run `/login` to connect a provider, `/help` for commands, `/settings` for
preferences, and `/resume` to reopen a session. [Model configuration](docs/models.md),
[skills](docs/skills.md), [prompt templates](docs/prompt-templates.md), and
[themes](docs/themes.md) are file-backed under `~/.e/`.

Extensions are ordinary executable child processes speaking versioned JSONL.
They can contribute tools, commands, hooks, and startup behavior without an
embedded scripting runtime. See [extensions](docs/extensions.md).

## Safety model

e is a local coding agent. Model-directed tools—including shell commands—run
as your user without a permission prompt by default. Directory trust controls
whether repository instructions, skills, and prompts enter model context; it
does not sandbox execution. Use a container, VM, or OS sandbox when the work
needs containment. Read the complete [security policy](SECURITY.md) before
using e on untrusted repositories.

`e doctor` reports versions, paths, terminal capabilities, configuration
formats, and provider state. It does not launch extensions, makes no network
requests, and configuration values and tokens are never printed. The legacy
`--no-network` spelling remains accepted for script compatibility.

## Compatibility

The CLI, persisted sessions/configuration, and extension wire protocol are
the supported surfaces. The Cargo library target is internal and is not a
stable Rust SDK. The precise policy and retained release fixtures are in
[compatibility](docs/compatibility.md).

## Contributing

The repository pins Rust and exposes one development entry point:

```sh
./x test                  # behavioral suite
./x check                 # format, lint, tests, security guard
./x bench                 # portable performance budgets
```

Start with [CONTRIBUTING.md](CONTRIBUTING.md) and the
[architecture map](docs/architecture.md).

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
