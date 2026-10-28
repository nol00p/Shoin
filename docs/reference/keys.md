# Keys

Every key Shoin answers to, by mode. `Space` is the leader (`[input].leader`).
Inside the editor, `Space hl` or `:help shortcuts` shows the same map.

## Motions

Motions take a count, such as `3w`.

| Key | Moves |
|---|---|
| `h` `j` `k` `l`, arrows | left / down / up / right |
| `w` `b` `e` | next word / previous word / end of word |
| `W` `B` `E` | the same, by whitespace-delimited word |
| `0` `^` `$` | line start / first non-blank / line end |
| `{` `}` | previous / next paragraph |
| `gg` `G` | first / last line (`G` with a count: that line) |
| `f{c}` `t{c}` | find / till a character forward; `F` `T` backward |
| `;` `,` | repeat / reverse the last `f` or `t` |
| `*` | search for the word under the cursor |
| `H` `M` `L` | top / middle / bottom of the visible screen |
| `Ctrl-d` `Ctrl-u` | half a page down / up |
| `Ctrl-f` `Ctrl-b` | a full page down / up |

## Operators

An operator takes a motion or a text object; doubled, it acts on the whole
line.

| Key | Does |
|---|---|
| `d` `c` `y` | delete / change / yank — `dd` `cc` `yy` for the line |
| `>` `<` | indent / outdent (`>>`, `>ip`); `>al` `<al` move a list item in or out with its sub-items |
| `gu` `gU` | lowercase / uppercase (`guiw`, `gUU`) |
| `D` `C` `Y` | delete / change / yank to the end of the line |
| `x` `X` | delete the character under / before the cursor |
| `r{c}` | replace the character under the cursor |
| `~` | toggle case |
| `J` | join with the next line |
| `p` `P` | paste after / before |

Counts multiply: `2d3w` deletes six words.

## Text objects

After an operator, or in Visual mode: `diw`, `ca(`, `vip`.

| Object | Selects |
|---|---|
| `iw` `aw` | a word, without / with the space after it (`iW` `aW` by whitespace) |
| `is` `as` | a sentence |
| `ip` `ap` | a paragraph (linewise) |
| `i"` `i'` `` i` `` | inside quotes; `a"` and so on include the quotes |
| `i(` `i[` `i{` `i<` | inside brackets; `a(` and so on include them (`b` = `()`, `B` = `{}`) |
| `il` `al` | a list item, without / with its sub-items (linewise) |
| `aL` | every item at the same level of a list, each with its sub-items (linewise) |

Brackets may span lines. `>` and `<` over a list object refuse a shift that
would break the list — see [Editing](../editing.md#lists).

## Registers

| Key | Register |
|---|---|
| `"a{cmd}` | use register `a`: `"ayy`, `"ap`, `"add` |
| `"A{cmd}` | append to register `a` |
| `"0` | the last yank — a delete never overwrites it |
| `"1`–`"9` | the delete ring, newest first |
| `"+` `"*` | the system clipboard: `"+yy`, `"+p` — see [Clipboard](../clipboard.md) |

## Writer verbs

In Normal mode these act on the word under the cursor; in Visual mode, on the
selection. Each is one undo step.

| Key | Does |
|---|---|
| `gb` `gi` `gh` `gk` | toggle **bold**, *italic*, `==highlight==`, `` `code` `` |
| `gt` | toggle a task checkbox (adds one to a plain list item) |
| `gl` | wrap as a `[link](url)`, cursor in the URL |
| `g1`–`g6` `g0` | set the heading level / strip it |
| `gp` | start the next paragraph |
| `gf` | follow the link under the cursor — see [Notes](../notes.md) |
| `gx` | open the link with the desktop |

## Lists

In Normal mode. See [Editing](../editing.md#lists).

| Key | Does |
|---|---|
| `Alt-j` `Alt-k` | move the list item past the next / previous one, with its sub-items (a count moves it past that many) |
| `>al` `<al` | indent / outdent the item with its sub-items |
| `>aL` `<aL` | indent / outdent every item at that level |
| `Tab` `Shift-Tab` | in Insert mode, the same as `>al` `<al` |

## Insert mode

| Key | Does |
|---|---|
| `i` `a` `I` `A` `o` `O` | insert / append / at line start / at line end / open below / open above |
| `Esc` `Ctrl-c` `jk` | leave Insert (`jk` is `input.escape_alias`) |
| `Enter` | continue a list marker or quote; on an empty item, end the list |
| `Tab` `Shift-Tab` | on a list item's line, indent / outdent the item with its sub-items (`editor.list_tab`); elsewhere `Tab` types a tab |

## Visual mode

| Key | Does |
|---|---|
| `v` `V` | start a charwise / linewise selection |
| drag | charwise selection, from where the button went down |
| `d` `c` `y` `x` | operate on the selection |
| `o` | swap ends |
| `>` `<` `u` `U` | indent / outdent / lowercase / uppercase |
| `i{obj}` `a{obj}` | select a text object (`viw`, `vap`) |
| `Space b` `Space i` | toggle bold / italic |

## Other

| Key | Does |
|---|---|
| `u` `Ctrl-r` | undo / redo |
| `.` | repeat the last change |
| `/` `?` `n` `N` | search forward / back, repeat / reverse |
| `:` | the command line — see [Commands](commands.md) |
| `Ctrl-^` `Ctrl-6` | back to the previous buffer |

## Leader keys

These are the shipped defaults in `keys.conf`, and they're built in. Remap
them under `[keys.normal]` and `[keys.visual]`, or set one to `"none"` to
remove it.

| Key | Does |
|---|---|
| `Space hl` | the shortcuts page |
| `Space w` `Space q` | save / quit |
| `Space F` | cycle focus mode |
| `Space t` | typewriter scrolling |
| `Space z` | zen mode |
| `Space c` | concealment on / off |
| `Space n` | line numbers |
| `Space a` | cycle text alignment |
| `Space fe` `Space fE` | file tree in this file's folder / in `$HOME` |
| `Space ff` `Space fF` | find a file in this file's folder / in `$HOME` |
| `Space fb` | switch buffer |
| `Space sv` `Space ss` | split beside / below — again to close |
| `Space so` | keep only this pane |

## Panes

| Key | Does |
|---|---|
| `Ctrl-w` `h` `j` `k` `l` | move between panes |
| `Ctrl-w` `v` `s` | split beside / below |
| `Ctrl-w` `q` `o` `w` | close / keep only this / cycle |
| `Ctrl-w` `>` `<` `+` `-` | wider / narrower / taller / shorter (takes a count) |
| `Ctrl-w` `=` | equal sizes again |

## File tree

| Key | Does |
|---|---|
| `j` `k` | move |
| `l` `Enter` `o` | open a file or expand a folder |
| `h` | collapse |
| `g` `G` | top / bottom |
| `R` | refresh |
| `-` `=` | move the root up a level / into the selected folder |
| `H` | show / hide dotfiles |
| `a` | new file (end with `/` for a folder) |
| `r` `m` `d` | rename / move / delete |
| `q` | close the tree |

## Fuzzy finder

| Key | Does |
|---|---|
| type | narrow the list |
| `Ctrl-n` `Ctrl-p`, arrows | next / previous match |
| `Ctrl-u` | clear the query |
| `Enter` `Esc` | open / close |

## :diff view

| Key | Does |
|---|---|
| `n` `p` | next / previous difference |
| `Tab` | cycle this difference: live → file → both |
| `l` `f` `b` | set every difference to live / file / both |
| `w` | commit the merge and write it |
| `q` `Esc` | abort |
