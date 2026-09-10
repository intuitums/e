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
- Only chords not already claimed by e's application-level shortcuts
  (ctrl+c, ctrl+p, ctrl+v for clipboard images, tab, shift+tab, menu
  navigation) reach this keymap — binding one of those here has no effect,
  since the app-level handler runs first.

Apply instantly with `/reload` (or after closing `/settings`).

## Global cancellation and trust navigation

Ctrl+C works in every panel. The first press cancels active work and sign-in,
clears the draft, and arms exit. Press it again within 1.5 seconds to quit.
Quitting at the trust question does not record a trust decision.

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
composer. The draft and submitted command retain the original prefix.
