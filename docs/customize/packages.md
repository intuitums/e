---
title: Packages
description: install shared extensions, skills, prompts, and themes
order: 4
---

# Packages

A package bundles extensions, skills, prompt templates, and themes so they
can be shared. It is an npm package, a git repository, or a directory on
disk laid out like `~/.e/` itself — any subset of these four directories,
nothing else required:

```
extensions/   executables, or bundle directories with an entry point
skills/       <name>/SKILL.md folders
prompts/      <name>.md templates
themes/       <name>.json palettes
```

There is no manifest of e's own. A `package.json` is npm's business: e
reads the directories, and takes only the dependency list from the
manifest. Find packages in the catalog (`site/packages/`, every npm package
carrying the `e-package` keyword) or on npmjs.com.

> **Security:** a package runs with your full permissions. Its extensions are
> executables e starts at launch, and its skills and prompts steer the model.
> Read the source before installing anything, and pin a ref you have read.

## Install and manage

```sh
e install npm:@fschrhunt1/e-diff                # from npm, follows latest
e install npm:@team/e-tools@1.4.0               # pinned to a version
e install git:github.com/fschrhunt/e-diff       # a git repository, default branch
e install git:github.com/fschrhunt/e-diff@v2    # pin a tag, branch, or commit
e install https://github.com/user/repo          # any git URL works
e install git:git@github.com:user/repo@main     # SSH, with your keys
e install ~/src/my-package                      # a local directory, in place

e packages                                      # what is listed, and its state
e remove npm:@fschrhunt1/e-diff                 # forget it, delete the install
e install                                       # make disk match settings
```

Installing puts an npm package under `~/.e/packages/npm/node_modules/<name>`
(that directory is one npm project of e's own; never edit it by hand) and a
git repository under `~/.e/packages/<host>/<path>`, and records the source
in the `packages` list of `~/.e/settings.json`. Restart e or run `/reload`
to pick it up.

npm never runs a package's lifecycle scripts for e — `e install` passes
`--ignore-scripts` every time — so installing runs nothing; loading does. A
git package whose `package.json` declares `dependencies` gets them
installed the same way (`npm ci` with a lockfile, `npm install` without),
so an extension that imports a library works after one `e install`. Both
need `npm` on `PATH` and use its registry and credentials as configured.

Two more ways a package reaches a session:

- `e --package <source> …` (or `-P`) loads a package for this run only: a
  directory in place, a git source into a temporary clone, a release asset
  into a temporary directory. Nothing is recorded; clones are removed at
  exit. Repeat the flag for several. This is how you try a package before
  installing it, or run one from a checkout you are editing.
- A trusted repository's `.e/packages` file — one source per line, `#`
  comments — lists packages the team shares. Trusting the directory offers
  to install the ones you lack; they also install on `e install`, show in
  `e packages`, and are reported at startup when missing, exactly like your
  settings' entries; they are never written into your settings. Only npm,
  git, and release sources count here: a local directory line is ignored,
  because trusting a checkout must not be enough to run code it carries in
  place. Install one yourself with `e install ./dir` if you mean it.

Settings are the source of truth, not the directory:

- `e install` with no source installs every listed package that is missing
  and brings the rest current: an unpinned npm package moves to `latest`, a
  pinned one stays; a pinned git ref is checked out again, an unpinned one
  fast-forwards to its default branch. Copy `settings.json` to a new machine
  and one `e install` restores the set.
- `e install <same package>@<other ref>` moves the pin — one entry, not two.
  Identity ignores scheme, credentials, `.git`, and host case, so the HTTPS
  and SSH spellings of one repository are the same package.
- A listed package that is not on disk is reported once in the transcript at
  startup. Startup itself never touches the network; only `e install` does.
- A local directory is referenced where it is, never copied. `e remove` only
  forgets it.

Git runs as a subprocess, so whatever your `git` can clone, e can install —
private hosts, SSH config, credential helpers included; git never prompts
under e. The same goes for `npm` and its registry settings.

## Loading part of a package

A `packages` entry can be an object instead of a string: the `source`, plus
a glob list per kind naming what loads. Patterns are relative to the
package root; `!` excludes.

```json
{
  "packages": [
    "npm:@fschrhunt1/e-diff",
    {
      "source": "npm:@team/e-tools@1.4.0",
      "extensions": ["!extensions/legacy.mjs"],
      "prompts": ["prompts/review.md", "prompts/r*.md"]
    }
  ]
}
```

A kind with no list loads whole. A list of exclusions alone loads
everything else; once an inclusion appears, only what an inclusion names
loads, minus the exclusions. Names are the top-level entries of the kind's
directory: an extension file or bundle directory, a skill folder, a prompt
or theme file. `e install` moving a pin keeps the filters.

## How package resources load

Every loader reads `~/.e/<kind>/` first, then each installed package's
`<kind>/` in settings order. Skills and prompts go one step further: after
`/trust`, the repository's own `.e/skills/` and `.e/prompts/` too. Extensions
and themes never load from a repository — trusting a checkout must not run
its code or restyle your terminal; install it as a package if you mean it.
On a name clash the closer context wins: a repo skill shadows a global one,
and a global one shadows a package's. A package theme can name a built-in
(`dark`) and replace it, unless `~/.e/themes/dark.json` exists.

- **Extensions** launch like any in `~/.e/extensions/`; their config lives
  under their own name in `settings.json` → `"extensions"`.
- **Skills** show `Package` as their scope in the `$` picker.
- **Prompts** become `/name` commands.
- **Themes** appear in `/settings` → Theme.

## Release packages

A compiled extension installs from a GitHub release:

```sh
e install release:<owner>/<repo>/<name>        # the latest release
e install release:<owner>/<repo>/<name>@v1.2.0 # pinned
```

e downloads `<name>-<target>.tar.gz` for this machine's platform from the
release, checks it against the release's `checksums.txt`, and places the
`<name>` executable under `~/.e/packages/releases/<owner>/<repo>/<name>/extensions/`,
the same shape as every other package. An unpinned release package follows
the latest release on `e install`; `e remove` deletes it. Platforms are the
ones e itself is released for.

To publish one, name the asset `<name>-<target>.tar.gz` with the executable
at its top level, list it in the release's `checksums.txt` (`sha256sum`), and
build one per target e is released for (`e update` names them). The e
repository ships no packages of its own, so nothing about a package needs a
change to e.

## Publishing a package

```sh
e packages init my-package     # the directories, one extension, package.json, README
cd my-package && e --package . # try it in place
npm publish                    # e install npm:my-package for everyone
```

`e packages init` writes a `package.json` with the `e-package` keyword —
that keyword is what lists a package in the catalog — and a `files` list
naming the four directories, so nothing else ships. Add the kinds you ship
as keywords too (`extensions`, `skills`, `prompts`, `themes`) and the
catalog labels the package with them. Keep extensions
executable (`chmod +x`, and commit the mode: npm and git both carry it). Tag
or version releases so users can pin what they read. A git repository with
the same layout is installable as `git:<host>/<user>/<repo>` without any of
this; publishing to npm is what makes it findable.

[fschrhunt/e-diff](https://github.com/fschrhunt/e-diff) shows the shape: one
extension file and one prompt template, installable with a single `e
install`. It is how `/diff` reaches e — a package, not a part of it.
