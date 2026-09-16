# Keybindings

`~/.e/keybindings.json` overrides the composer's line-editing keys — the
same file-backed pattern as themes and skills. A missing or malformed file
falls back to e's built-in bindings untouched.

```json
{
  "ctrl+j": "none",
  "alt+d": "kill_word"
}
```

- A chord is `[ctrl+][alt+][shift+]<key>`, modifiers in any order,
  case-insensitive. `<key>` is `enter`, `backspace`, `delete`, `left`,
  `right`, `up`, `down`, `home`, `end`, or a single character — `+` and `-`
  included (`ctrl+-`, `ctrl++`): modifiers are read off the front and
  whatever remains is the key. A capital letter is spelled with its
  modifier, `shift+a`, since that is how the terminal reports it.
- The value is an action name — `enter`, `newline`, `backspace`, `delete`,
  `left`, `right`, `up`, `down`, `word_left`, `word_right`, `home`, `end`,
  `kill_to_end`, `kill_to_start`, `kill_word` — or `"none"` to unbind a
  built-in chord (the key is swallowed, not typed as a literal character).
- ctrl+g opens the draft in an external editor: the `editor` setting in
  `~/.e/settings.json` (`"editor": "code --wait"`), else `$VISUAL`, else
  `$EDITOR`, else `vi`. Save and quit to bring the text back; a non-zero
  exit leaves the draft unchanged.
- ↑ on an empty composer recalls earlier prompts, including those from
  previous sessions: the newest thousand are kept in `~/.e/history.jsonl`,
  private to your user. A prompt identical to the last one is not repeated.
- Only chords not already claimed by e's application-level shortcuts
  (ctrl+c, ctrl+o, ctrl+p, tab, shift+tab, menu navigation) reach this
  keymap — binding one of those here has no effect, since the app-level
  handler runs first.
- An extension's declared shortcut (`docs/extensions.md`, Shortcuts) runs
  after this keymap: a chord bound here, or by the composer's built-in
  bindings, never reaches the extension. Unbind it here (`null`) to hand it
  over. Extensions may only declare ctrl or alt chords.
- The chord that moves focus between the conversation and a side pane
  (`ctrl+t` by default) is set in `~/.e/layout.json`, not here — see
  `docs/layout.md`.

Apply instantly with `/reload` (or after closing `/settings`).

## Full transcript

`Ctrl+O` opens the review screen at the latest output. Tool details wrap with
the reference's `│` rails. The footer has a navigation row, a blank row, and
the usual model and context status. The screen has two depths:
Review folds each tool detail to three lines behind a `→ to expand` hint;
Full shows every row.

- `Up`/`Down` scroll one row; the mouse wheel scrolls three.
- `PageUp`/`PageDown` scroll a page. `Home`/`End` jump to the ends.
- `←`/`→` switch between the Review and Full depths.
- Scrolling up pauses following new output. Returning to the bottom resumes it.
- `Ctrl+O` or `Esc` closes the reader. `Ctrl+C` closes it and retains e's global
  cancellation behavior. Typing and pasting in the reader leave the draft alone.

The reader uses the alternate terminal screen, so it does not replace normal
scrollback with expanded tool output. Closing returns to the previous view and
preserves the draft.

Set `transcript_hint` in `~/.e/settings.json` to override the footer wording.
It applies on the next open, at both depths. The defaults are
`Review · ←/→ switch · ctrl o close · PgUp/PgDn scroll · Esc close` and
`Full detail · ←/→ switch · ctrl o close · PgUp/PgDn scroll · Esc close`.
The rail connector renders in the theme's `muted` tone.

## Pasted text

Long pastes collapse into a marker such as
`[Pasted text #1, 42000 chars]`, in the same `dim` grey as image attachments.
Characters count Unicode codepoints after CRLF and standalone CR normalize
to newlines. The label does not count source lines or wrapped screen rows.

Numbers identify collapsed pastes in the current draft, not every clipboard
operation in the session. Existing numbers stay stable. New pastes use the
next number after the highest remaining one, restarting at `#1` when none
remain or when you submit or clear the draft.

Deleting or replacing any part of a marker removes the whole marker and its
stored text. Clearing or replacing the draft also discards its attachments.
Retyping a deleted marker cannot bring the text back. History navigation
preserves the unsent draft and its attachments until you return, submit, or
clear it. Submission expands each surviving attachment once.

These preferences in `~/.e/settings.json` take effect in a new editor:

- `paste_placeholder`: collapse pastes above this codepoint count. Default
  `1000`; `0` disables collapsing.
- `paste_label`: default
  `"[Pasted text #{id}, {chars} chars]"`.
  Optional `{lines}` and `{plural}` fields count source lines. `{plural}` is
  empty for one line and `s` otherwise. An empty label inserts
  the full text rather than creating an invisible attachment.

  (ctrl+c, ctrl+p, ctrl+v or Command+V for clipboard image/text paste, tab,
  shift+tab, menu navigation) reach this keymap — binding one of those here has no effect,
  since the app-level handler runs first.

Apply instantly with `/reload` (or after closing `/settings`).

## Global cancellation and trust navigation

Ctrl+C works in every panel. The first press cancels active work and sign-in,
clears the draft and any held launch prompt, closes trust and queue navigation,
and arms exit. Press it again within 1.5 seconds to quit.
Quitting at the trust question does not record a trust decision, and neither
does its last row: declining exits, and the next launch asks again.

Long trust questions and choices wrap. If they exceed the terminal height,
PgUp/PgDn scroll the text without changing the choice; Up/Down change the choice
and reveal its label. The scrolling hint can be overridden with
`"trust_scroll_hint"` in `~/.e/settings.json`.

Pastes normalize CRLF and standalone CR to one newline each. Terminal control
characters in a draft display as replacement characters, while tabs display
as spaces. The underlying draft retains those characters for submission.
Up/Down preserve display columns across wide and combining characters.

## Shell composer

Typing `!` as the first character replaces the first `┃` gutter with a green
`!`, using the theme's `bashMode` token. Command text keeps its normal color;
wrapped lines keep neutral rails. Deleting the leading `!` restores the normal
composer. The draft and submitted command retain the original prefix. When
that prefix is `! `, its space remains editable in the gutter, with its own
cursor and selection highlight.
