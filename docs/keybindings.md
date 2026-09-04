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

- A chord is `[ctrl+][alt+][shift+]<key>`, any order, case-insensitive.
  `<key>` is `enter`, `backspace`, `delete`, `left`, `right`, `up`, `down`,
  `home`, `end`, or a single character.
- The value is an action name — `enter`, `newline`, `backspace`, `delete`,
  `left`, `right`, `up`, `down`, `word_left`, `word_right`, `home`, `end`,
  `kill_to_end`, `kill_to_start`, `kill_word` — or `"none"` to unbind a
  built-in chord (the key is swallowed, not typed as a literal character).
- Only chords not already claimed by e's application-level shortcuts
  (ctrl+c, ctrl+p, tab, shift+tab, menu navigation) reach this
  keymap — binding one of those here has no effect, since the app-level
  handler runs first.

Apply instantly with `/reload` (or after closing `/settings`).
