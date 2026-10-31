# Panels

Shoin has no permanent interface. The file tree, the fuzzy finder, the buffer
list and splits appear when you call them and leave when you're done — each
opens and closes on the same key. `Space` is the leader.

| | |
|---|---|
| `Space fe` / `Space fE` | file tree — this file's folder, or all of `$HOME` |
| `Space ff` / `Space fF` | fuzzy finder — this file's folder, or all of `$HOME` |
| `Space fb` | switch buffer |
| `Ctrl-^` / `Ctrl-6` | back to the previous buffer — press again to return |
| `Space sv` / `Space ss` | split beside / below |
| `Ctrl-w` + `h j k l` | move between panes |
| `:e <path>` | open another file |
| `:q` / `:Q` | close this buffer (the last one quits) / quit every buffer |

## File tree

`j` and `k` move, `l` or `Enter` opens a file or expands a folder, `h`
collapses, and `q` closes the tree.

| In the tree | |
|---|---|
| `-` / `=` | move the root up a level / into the selected folder |
| `H` | show / hide dotfiles |
| `a` | new file in this folder (end the name with `/` for a folder) |
| `r` / `m` / `d` | rename / move / delete |
| `g` `G` `R` | top, bottom, refresh |
| `Ctrl-w h` / `Ctrl-w l` | back and forth between the tree and the editor |

## Fuzzy finder

Type to narrow the list. `Ctrl-n` and `Ctrl-p` (or the arrows) move,
`Enter` opens, `Esc` closes, and `Ctrl-u` clears the query.

## Buffers and splits

`:ls` lists the open buffers, `:b <name>` or `:b <n>` switches, `:bn` and `:bp`
go round the list, and `:bd` closes one. `:sp` and `:vs` split, `:close`
closes a pane and `:only` keeps just this one. `Ctrl-w` also splits (`v`, `s`),
closes (`q`), cycles (`w`), resizes (`>` `<` `+` `-`) and evens out (`=`)
panes.

## Details

- Deleting asks first, and says how much a folder holds.
- Renaming or moving a file you have open carries the buffer with it.
- In a tree prompt, `Ctrl-u` clears it and `Esc` abandons it.
- Dotfile visibility is remembered; the default is `tree.show_hidden`.
- Opening a file hands focus back to the editor and leaves the tree open for
  the next one. `tree.close_on_open = true` closes it instead.
- The leader keys are built in. Remap them under `[keys.normal]` (see
  [Configuration](configuration.md#key-bindings)).
