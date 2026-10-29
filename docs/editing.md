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

## Lists

A list item is a text object too. `al` is the item with its continuation
lines and everything nested under it, `il` is the item without its sub-items,
and `aL` is every item at the same level, each with its sub-items. So `>al`
nests an item under the one above it and takes its sub-items along, `<al`
brings it back out, `dal` deletes the whole item, and `>aL` indents a whole
nested level at once.

`Alt-j` and `Alt-k` move the item under the cursor past the next or previous
item, its sub-items travelling with it; `3 Alt-j` moves it past three. An
item never leaves its list this way: at the first or last item the move is
refused, and `<al` is how it moves out.

Numbered lists stay in sequence: when an item moves, or moves into or out of
a `1.` `2.` `3.` list, the numbers of each list it touched are rewritten, in
the same undo step. Bullet lists are never changed. Turn this off with `list_renumber = false`
in `[editor]`.

In Insert mode, Tab and Shift-Tab do the same as `>al` and `<al` on an item's
line, so Enter, Tab, and typing starts a sub-item.

An indent that would break the list is refused with a message and changes
nothing (see Details). Each command is one undo step and `.` repeats it.

## Registers

`"ayy` yanks into register `a` and `"ap` pastes from it; `"A` appends to it.
`"0` holds the last yank, which a delete never overwrites, and `"1`–`"9` are
the delete ring, newest first. `"+` is the system clipboard — see
[Clipboard](clipboard.md).

## Insert mode

Enter continues a list marker or a quote prefix, and ends the list when the
item is empty. On a list item's line, Tab nests the item under the one above
and Shift-Tab brings it back out, sub-items included, the same as `>al` and
`<al`; anywhere else Tab types a tab. `list_tab = false` in `[editor]` makes
Tab a tab on list items too. A terminal paste goes in exactly as copied: no
auto-indent, no list continuation, and a `jk` in the pasted text stays text.

## Details

- Which indents are refused: `>al` on the first item of a list or sub-list
  (there is no item above it to nest under), `<al` and `<aL` on a top-level
  item (nowhere to go), and `>aL` when it would put a top-level list 4 or
  more columns in — other Markdown tools, GitHub's included, read that as a
  code block. In a file that nests by 4, a top-level level does not indent
  at all; a nested one always can.
- How far an item moves follows the file, not `tab_width`. `<al` puts it on
  its parent's column. `>al` lines it up with the sub-items it joins, or, when
  the item above has none, nests it by the step of the nearest nested list in
  the file (2 for `- a` / `  - b`). `>aL` and `<aL` move a level by that step.
  Only a file where nothing nests yet uses `tab_width`. With `expand_tab = false`
  a step of whole tabs is written as tabs.
- Outdenting an item from the middle of a sub-list keeps the text in order,
  so the items after it become its sub-items, as in org-mode. `>il` works the
  same way the other direction: the item moves in, its sub-items stay put and
  so belong to the item above.
- `Alt-j` needs a terminal that sends Alt as a modifier. In tmux, a quick
  `Esc` then `j` can arrive as `Alt-j` when `escape-time` is long (the
  default was 500 ms before tmux 3.5): `set -sg escape-time 10` fixes it, and
  it is the usual advice for Vim in tmux. On macOS, Terminal.app needs "Use
  Option as Meta key" and iTerm2 needs Option set to "Esc+". Or rebind
  `list_item_down` and `list_item_up` in `keys.conf`. Tab and Shift-Tab are
  `list_indent` and `list_outdent` under `[keys.insert]`.
- Moving swaps the item and the items it passes as two blocks of text; the
  blank lines between them stay where they were.
- Renumbering keeps the number a list starts at, so a list starting at `3.`
  still starts at `3.` after its first item moves away. A sub-list the edit
  creates starts at `1.`. Each item keeps its own `.` or `)`. Only the lists
  the edit touched are renumbered; others in the file stay as written, even
  if their numbers are out of order. Going from `9.` to `10.` pushes that
  item's text one column right; the lines under it are not re-aligned.
- An item is its line, the lines that continue it (including an unindented
  "lazy" line right after it), and everything indented deeper below it, blank
  lines in between included. A blank line after an item is never part of it.
  An item is nested under the nearest less-indented item above it, the rule
  HTML export uses, so `- a` / `   - b` / `  - c` makes `b` and `c` both
  sub-items of `a`. A switch between `-` and `1.` at one depth starts a new
  list. A tab counts as `tab_width` columns.
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
