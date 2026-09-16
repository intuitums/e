<p align="center">
  <a href="https://e.intuitum.sh">
    <picture>
      <source srcset="assets/logo-dark.svg" media="(prefers-color-scheme: dark)">
      <source srcset="assets/logo.svg" media="(prefers-color-scheme: light)">
      <img src="assets/logo.svg" alt="e" height="40">
    </picture>
  </a>
</p>
<p align="center">The small, fast coding agent you can put anywhere.</p>
<p align="center">
  <a href="https://github.com/intuitums/e/releases"><img alt="Release" src="https://img.shields.io/github/v/release/intuitums/e?style=flat-square&label=release&labelColor=grey&color=blue" /></a>
  <a href="https://github.com/intuitums/e/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/intuitums/e/ci.yml?style=flat-square&branch=main&label=CI" /></a>
</p>

<br>

**e** is one Rust binary that reads, edits, runs, and explains code with any
model you sign in to. It starts as a terminal app. The same binary is a
headless session server for Slack bots and CI, a Rust library for your own
product, and a host for extensions your team writes in any language.

- **Small and fast.** A single binary, no runtime, byte-pinned rendering.
- **Any model.** Anthropic, OpenAI, Google, and every OpenAI-compatible
  gateway, with your own `models.json` for the rest.
- **Yours to extend.** Executable JSONL extensions add tools, commands,
  hooks, and side panes. Packages share extensions, skills, prompts, and
  themes with `e install`.
- **Goes where your team works.** `e rpc` speaks sessions over stdin and
  stdout; reference Slack and GitHub channels show the shape. `e-sdk`
  embeds the agent in Rust.

## Install

```sh
curl -fsSL https://e.intuitum.sh/install.sh | sh
```

Or with a package manager (available after the first package-enabled
release):

```sh
brew install intuitums/tap/e
npm install -g @intuitums/e
bun add -g @intuitums/e
```

Update with the method you installed with: `e update` for curl or a release
archive, `npm install -g @intuitums/e@latest`, `bun add -g @intuitums/e@latest`,
or `brew upgrade intuitums/tap/e`. `e --version` confirms the build.

The Linux binaries link against glibc 2.39 or newer — Ubuntu 24.04+, Debian 13+,
Fedora 40+, RHEL 10+. Older distributions (Ubuntu 22.04, Debian 12, RHEL 9) need
a build from source, or the published image, which carries its own runtime:

```sh
docker run --rm --entrypoint e ghcr.io/intuitums/e-slack:latest --version
```

## Start

```sh
e                                          # open a session in this directory
e "why is this function 400 lines long"    # start with a prompt
e -p "summarize the last commit"           # one headless turn, reply on stdout
e help
```

Sign in from inside e with `/login <provider>`, or set the provider's usual
environment variable (`ANTHROPIC_API_KEY` and friends) for scripts and CI.

## Put it anywhere

| Surface | What it is | Guide |
|---|---|---|
| Terminal | The interactive app: transcript, composer, side panes, `/` commands | `e help` |
| `e -p` | One headless turn; `--json` streams every event | [Automation](docs/automation.md) |
| `e rpc` | A JSONL session server: concurrent sessions, streaming events, extension questions relayed to your client | [Automation](docs/automation.md) |
| Channels | Reference Slack bot and GitHub Actions workflow built on `e rpc` | [Channels](docs/channels.md) · [`channels/`](channels/) |
| e-sdk | The agent as a Rust library: sessions, turns, one event stream (`cargo add intuitums-e-sdk`) | [SDK](docs/sdk.md) |
| Extensions | Tools, commands, hooks, and UI from a subprocess in any language | [Extensions](docs/extensions.md) |
| Packages | Share extensions, skills, prompts, and themes from git or npm | [Packages](docs/packages.md) |

## Safety

- Model-directed tools run as your user without a permission prompt by
  default.
- Directory trust controls context loading; it does not sandbox execution.
- Use a container, VM, or OS sandbox when work needs containment. See
  [sandboxing](docs/sandboxing.md) and [SECURITY.md](SECURITY.md).

## Documentation

Every guide is in [`docs/`](docs/) and built into the binary: `e docs` lists
the topics and `e docs <topic>` prints one.

[Extensions](docs/extensions.md) · [Automation](docs/automation.md) ·
[Channels](docs/channels.md) · [SDK](docs/sdk.md) · [Packages](docs/packages.md) ·
[Skills](docs/skills.md) · [Instructions](docs/instructions.md) ·
[Prompt templates](docs/prompt-templates.md) · [Models](docs/models.md) ·
[Themes](docs/themes.md) · [Layout](docs/layout.md) ·
[Keybindings](docs/keybindings.md) · [Sandboxing](docs/sandboxing.md) ·
[Architecture](docs/architecture.md) · [Compatibility](docs/compatibility.md)

## Development and preview builds

Run `./x dev /path/to/project` from a checkout, or install a published preview
with `npm install -g @intuitums/e@dev` and run `e-dev`. Beta uses `@beta` and
`e-beta`. Curl and brew support separate beta installations; dev uses npm or
bun. Preview channels have separate state and stay on their channel when
updating. See [releases and testing](docs/releases.md) for all installers and
PR builds.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md), the
guide an agent editing this repository follows. `./x check` is the whole
bar: format, lint, tests, and the security-surface guard.

---

<p align="center">
  <sub>Made by <a href="https://intuitum.sh">Intuitum</a> · <a href="mailto:support@intuitum.sh">support@intuitum.sh</a> · MIT</sub>
</p>
