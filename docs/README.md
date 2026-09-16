# Writing e's documentation

These folders are the only copy of e's guides. Three readers render them:

- **GitHub** — the files themselves, as you see them here.
- **`e docs <topic>`** — the binary embeds the `.md` files, so a guide ships with
  the release it documents and the agent can read it without a network.
- **e.intuitum.sh/docs** — the website fetches this folder from `main` at build
  time and renders each file as a page. A push to `main` that touches this folder
  pings the site's deploy hook, so a guide goes live without a change over there.

Write once, and all three follow. Never paste a guide's text into the website,
the README, or an issue: link to it.

## Layout

```
docs/
  README.md            this file — GitHub only, never a topic and never a page
  start/               one folder per nav group
    README.md          the group's label and order, and nothing else
    models.md
  usage/
    README.md
    sessions.md
  customize/
    README.md
    examples/          assets a guide links to (code, images); not topics
  extend/
    README.md
contributing/          the repository's own documentation: architecture,
                       rendering, releases. Never on the website, never in
                       `e docs`.
```

- **The folder is the nav group**, and its README.md's front matter names it
  and orders it: `docs/usage/` is “Usage”, second in the sidebar. Nothing is
  numbered, so renaming a group is renaming a folder.
- **The file stem is the `e docs` topic.** `docs/customize/themes.md` is
  `e docs themes`. Stems are unique across the whole folder tree.
- **A folder may hold assets** beside its guides — an example, an image. They
  are copied with the group so links keep working; only `.md`/`.mdx` become
  pages.
- **`README.md` in any folder is repository-only.** The one at the root is this
  guide; a group's carries that group's front matter.

## Front matter

Every guide starts with it, and it is the only metadata:

```md
---
title: Themes
description: theme JSON format; file wins over a built-in name
order: 5
---
```

- `title` — the label in the website's navigation and the page's heading.
- `description` — one line, no trailing period needed. It is the website's
  meta description and the blurb `e docs` prints when listing topics.
- `order` — position inside the group. Gaps are fine; ties fall back to the
  file name. In a group's README.md the same key orders the *group*.

A group README carries the same three keys and no guide content; the website
uses it for the sidebar label and position.

Only those three keys, one line each, no nested YAML. `build.rs` and the
website both parse them with a few lines of string handling, deliberately not a
YAML dependency.

## Markdown, MDX, and the heading

- **`.md` is the default** and the only thing the binary embeds.
- **`.mdx` is for a page that needs a website component** — an interactive
  installer, tabs, a diagram. It is skipped by `e docs` and by the topic list;
  write one only when the alternative is worse, because the binary and the
  release archive will not carry it.
- **Keep the `# Title` heading.** GitHub needs it; the website renders the
  front matter's `title` and drops the duplicate heading.

## Links

- **Another guide:** link it relatively — `[themes](customize/themes.md)` from
  this folder, or `themes.md` from beside it. GitHub resolves either, and the
  website rewrites it to the page's route. Do not write repository-absolute
  paths like `/docs/customize/themes.md`: they break on GitHub.
- **Anything outside `docs/`** — `contributing/`, `src/`, an example file —
  link it relatively too. The website points those at GitHub, since they are
  not pages.
- **Fragments work** (`extensions.md#results-by-method`) and the website keeps
  them.

## Checking your work

`./x test docs` covers this folder: every guide has complete front matter, the
topic names are unique, every relative link resolves, and `e docs` serves every
topic. No network, no build.

To see the website's rendering, run the web repository's dev server with
`E_DOCS_PATH` pointing at this checkout; it reads the files from disk instead of
fetching `main`.

## Adding a guide

1. Put it in the group it belongs to, and name the file after its topic.
2. Add front matter with `title`, `description`, and `order`.
3. Link it from a nearby guide — the navigation follows the folders, so a new
   page appears without another list to edit.
4. Run `./x test docs`, then `./x check`.

A new group is a new folder with a `README.md` whose front matter gives its
`title` and `order`; the sidebar and the topics list pick it up from there.
