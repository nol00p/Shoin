# Editing

<p align="center">
  <img src="media/editing.gif" alt="Editing a draft with ciw, dot-repeat, dap, ci( and list continuation" width="800">
</p>

Shoin's editing is Vim's: modes, operators that take motions and text objects,
counts, registers, undo and `.` repeat. If you know Vim, your hands already
know Shoin; the full list is in the [key reference](reference/keys.md).

## Modes

- **Normal** — move around and run operators. `Esc` always returns here.
- **Insert** — type text. Enter it with `i` `a` `I` `A` `o` `O`; leave with
  `Esc`, `Ctrl-c` or `jk`.
- **Visual** — select text: `v` by character, `V` by line, or drag the mouse.
- **Command** — the `:` line. **Search** — `/` forward, `?` backward.

## Getting around

```
  h j k l / arrows   move             i / a        insert / append
  w b e              by word          o / O        open a line below / above
  0 ^ $              line start/end   x            delete a character
  gg / G             top / bottom     dd yy p      delete / yank / paste
  { }                by paragraph     .            repeat the last change
  u / Ctrl-r         undo / redo      / ? n N      search
```

## Operators, objects and counts

Operators take motions, text objects and counts the way you'd expect: `d2w`,
`ciw`, `ya(`, `>ip`, `gUiw`, `2d3w` (counts multiply, so that deletes six
words). Doubling an operator acts on the whole line: `dd`, `cc`, `yy`.

Text objects work after an operator or in Visual mode: `iw`/`aw` for a word,
`is`/`as` a sentence, `ip`/`ap` a paragraph, `i"` inside quotes, `i(` `i[`
`i{` `i<` inside brackets (which may span lines).

## Registers

`"ayy` yanks into register `a` and `"ap` pastes from it; `"A` appends to it.
`"0` holds the last yank, which a delete never overwrites, and `"1`–`"9` are
the delete ring, newest first. `"+` is the system clipboard — see
[Clipboard](clipboard.md).

## Insert mode

Enter continues a list marker or a quote prefix, and ends the list when the
item is empty. A terminal paste goes in exactly as copied: no auto-indent, no
list continuation, and a `jk` in the pasted text stays text.

## Details

- Shoin's operators and motions are checked against Vim's behaviour, down to
  edge cases such as `dw` on a line's last word, `cw` on a word's last
  character and `d}` stopping at a blank line. The [Changelog](../CHANGELOG.md)
  lists each one.
- The escape alias is `input.escape_alias` (default `jk`); set it to `""` to
  turn it off.
- With the mouse on, a drag selects text the way `mouse=a` does in Vim.
  Dragging while in Insert mode gives Vim's `-- (insert) VISUAL --`: an
  operator or `Esc` hands typing back. `:set mouse off` gives selection back
  to the terminal.
- `:u`, `:undo`, `:red` and `:redo` work as well as `u` and `Ctrl-r`.
- Every key can be remapped, including whole verbs — see
  [Configuration](configuration.md#key-bindings).
