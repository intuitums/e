# Layout

`~/.e/layout.json` says where the regions of e's frame go and what the
status row reads — the same file-backed pattern as themes and keybindings.
A missing or malformed file falls back to e's built-in layout untouched.

```json
{
  "split_min": 110,
  "focus": "ctrl+t",
  "banner": true,
  "panes": {
    "*":    { "side": "right", "width": 40 },
    "diff": { "side": "left",  "width": 50 }
  },
  "status": {
    "left":  ["{model} / {effort}", "{context}"],
    "right": ["{status}"]
  }
}
```

Every key is optional; what is shown above is the default, except the
`diff` entry, which is an example.

## Panes

An extension opens a side pane with `ui.pane` (`docs/extensions.md`) and
may propose a side. `panes` outranks it: an entry named after the pane's
id sets its `side` (`left` or `right`) and `width` (its share of the
terminal, 30–70 percent); `*` is the default for every pane not named.
Below `split_min` columns the pane and the conversation cannot sit side by
side: the focused one fills the screen, and the status row says how to
reach the other.

`focus` is the chord that moves focus between the conversation and the
pane. Any ctrl or alt chord e does not already use works; the grammar is
the keybindings one (`docs/keybindings.md`).

## The status row

`status.left` and `status.right` are lists of segments. Each segment's
`{tokens}` expand; a segment whose tokens all came up empty is dropped, and
so is a ` / ` part around an empty token, so `"{model} / {effort}"` reads
just the model for a model without an effort knob. The first left segment
paints accent-bright, the rest muted after ` · `; the right side sits
right-aligned and gives way to a transient notice (the armed-exit hint, a
clipboard read, a hidden pane).

| Token | Value |
| --- | --- |
| `{model}` | the current model, compact |
| `{effort}` | its selected reasoning effort |
| `{context}` | the context in use, as a percent (blank under 1%) |
| `{cwd}` | the working directory, shortened like the tab title |
| `{session}` | the session's name, once it has one |
| `{status}` | every extension's `ui.status` slot, ` · ` joined |
| `{status:<name>}` | one extension's slots |

## The banner

`"banner": false` leaves out the `𝑒 <version> · Run /help for commands`
line at the top of a session.

Apply changes with `/reload` (or after closing `/settings`).
