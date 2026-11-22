# Keys

Use these keys to move, edit, and manage files in each mode. `<leader>`
is `Space` unless you remap it with `[input].leader`.
Inside the editor, `<leader>hl` or `:help` shows the same keys by task, with
search.

## Motions

In Normal and Visual mode. Motions take a count, such as `3w`.

| Key | Moves |
|---|---|
| `h` `j` `k` `l` / `Left` `Down` `Up` `Right` | left / down / up / right |
| `w` `b` `e` | next word / previous word / end of word |
| `W` `B` `E` | the same, by whitespace-delimited word |
| `0` `^` `$` | line start / first non-blank / line end |
| `{` `}` | previous / next paragraph |
| `gg` `G` | first / last line (with a count, go to that line) |
| `f` `t` `F` `T` | find / till the next character you type, forward / backward (`Tab` finds a tab) |
| `;` `,` | repeat / reverse the last `f` or `t` |
| `*` `#` | search for the word under the cursor forward / backward |
| `H` `M` `L` | top / middle / bottom of the visible screen |
| `Ctrl-d` `Ctrl-u` | half a page down / up |
| `Ctrl-f` `Ctrl-b` | a full page down / up |
| `Ctrl-e` `Ctrl-y` | scroll the page a line down / up; the cursor stays put until it would leave the screen |
| `zt` `zz` `zb` | scroll the cursor's line to the top / middle / bottom of the pane |

## Operators

In Normal mode, an operator takes a motion or a text object. Doubling it
acts on whole lines.

| Key | Does |
|---|---|
| `d` `c` `y` | delete / change / yank — `dd` `cc` `yy` for the line |
| `>` `<` | indent / outdent. Use `>>` or `<<` for whole lines |
| `>al` `<al` | indent / outdent the item with its sub-items |
| `>aL` `<aL` | indent / outdent every item at that level |
| `gu` `gU` `g~` | lowercase / uppercase / toggle case (`guiw`, `guu`, `gUU`, `g~~`) |
| `D` `C` | delete / change to the end of the line |
| `Y` | yank whole lines, like `yy` |
| `x` `X` | delete the character under / before the cursor |
| `r` | replace the character under the cursor with the next character you type; `Tab` is a tab, `Enter` splits the line |
| `~` | toggle case |
| `J` | join with the next line |
| `p` `P` | paste after / before |

Counts multiply: `2d3w` deletes six words.

## Text objects

After an operator, or in Visual mode: `diw`, `ca(`, `vip`.

| Object | Selects |
|---|---|
| `iw` `aw` | a word, without / with surrounding space |
| `iW` `aW` | a whitespace-delimited word, without / with surrounding space |
| `is` `as` | a sentence |
| `ip` `ap` | a paragraph (linewise) |
| `i"` `i'` `` i` `` | inside quotes |
| `a"` `a'` `` a` `` | with the quotes |
| `i(` `i)` `i[` `i]` `i{` `i}` `i<` `i>` `ib` `iB` | inside brackets. `b` means `()` and `B` means `{}` |
| `a(` `a)` `a[` `a]` `a{` `a}` `a<` `a>` `ab` `aB` | with the brackets |
| `il` `al` | a list item, without / with its sub-items (linewise) |
| `aL` | every item at the same level of a list, each with its sub-items (linewise) |
| `iL` | the list item and every item after it at its level, with sub-items (linewise) |

## Registers

In Normal and Visual mode.

| Key | Register |
|---|---|
| `"` | choose a register before a command |
| `"a{cmd}` | use register `a`: `"ayy`, `"ap`, `"add` |
| `"A{cmd}` | append to register `a` |
| `""` | the unnamed register |
| `"0` | the last yank — a delete never overwrites it |
| `"1`–`"9` | the delete ring, newest first |
| `"+` `"*` | the system clipboard: `"+yy`, `"+p` — see [Clipboard](../clipboard.md) |

## Writer verbs

Inline verbs act on the word under the cursor in Normal and on the
selection in Visual. Each edit is one undo step.

| Key | Does |
|---|---|
| `gb` `gi` `gh` `gk` | toggle **bold**, *italic*, `==highlight==`, `` `code` `` |
| `gt` | toggle a task checkbox (adds one to a plain list item) |
| `ga` | align the table under the cursor (`editor.table_align = "auto"` also does it on leaving Insert) |
| `gl` | wrap as a `[link](url)`, cursor in the URL |
| `g1` `g2` `g3` `g4` `g5` `g6` / `g0` | set the heading level / strip it |
| `gp` | start the next paragraph in Insert |
| `gf` | follow the link under the cursor — see [Notes](../notes.md) |
| `gx` | open the link with the desktop |

## Lists

In Normal and Visual mode. See [Editing](../editing.md#lists).

| Key | Does |
|---|---|
| `Alt-j` `Alt-k` | move the list item past the next / previous one, with its sub-items (a count moves it past that many); in Visual, the items the selection touches |

To rebind them, the actions are `list_item_down` and `list_item_up` (Normal
and Visual: set them under `[keys.normal]` and `[keys.visual]`)
and `list_indent` and `list_outdent` (Insert) — see
[Configuration](../configuration.md#key-bindings).

## Normal mode

| Key | Does |
|---|---|
| `i` `a` | enter Insert before / after the cursor |
| `I` `A` | enter Insert at the first non-blank / at the end of the line |
| `o` `O` | open a new line in Insert below / above this line |
| `v` `V` | start a charwise / linewise selection |
| `u` `Ctrl-r` | undo / redo |
| `.` | repeat the last change |
| `&` `g&` | repeat the last substitution on this line / throughout the document with its flags |
| `Esc` | cancel a count, operator or most pending sequences |

## Insert mode

Enter Insert with `i`, `a`, `I`, `A`, `o` or `O` from
[Normal mode](#normal-mode).

| Key | Does |
|---|---|
| `Esc` `Ctrl-c` `jk` | leave Insert (`jk` is `input.escape_alias`) |
| `Ctrl-s` | save, and keep typing |
| `Enter` | continue a list marker or quote; on an empty item, end the list |
| `Tab` `Shift-Tab` | on a list item's line, indent / outdent the item with its sub-items (`editor.list_tab`); elsewhere `Tab` types spaces or a tab, depending on `editor.expand_tab` |
| `Backspace` `Delete` | delete before / under the cursor |
| `Left` `Right` `Up` `Down` | move the cursor |
| `Home` `End` | start / end of line |
| `PageUp` `PageDown` | move half a page up / down |

## Visual mode

| Key | Does |
|---|---|
| `v` `V` | start a new charwise / linewise selection at the cursor |
| drag | select by character, from where the button went down |
| `d` `x` / `c` `s` / `y` | delete / change / yank the selection |
| `o` | swap ends |
| `Esc` | leave Visual |
| `>` `<` `u` `U` | indent / outdent / lowercase / uppercase |
| `gu` `gU` `g~` | lowercase / uppercase / toggle case, as `u` `U` `~` |
| `i` `a` | select a text object with the next key (`viw`, `vap`) |
| `p` `P` | replace the selection with the register; `p` puts what it replaced in the register, `P` keeps the register |
| `~` `r` | toggle the case / replace every selected character with the next character you type |
| `J` | join the selected lines |
| `g&` | repeat the last substitution with its flags on the selected lines |
| `D` `X` `Y` | delete / delete / yank the selected lines |
| `C` `S` `R` | change the selected lines |
| `I` `A` | insert before / append after the selection |
| `<leader>b` `<leader>i` | toggle bold / italic |

`move_lines_down` and `move_lines_up` move the selected lines down or up a
line and keep them selected, like Vim's `:'<,'>m '>+1 | gv`. They take a
count, each press is one undo step, and in Normal mode they move the cursor's
line. They have no key until you give them one:

```toml
[keys.visual]
"J" = "move_lines_down"
"K" = "move_lines_up"
```

## Normal and Visual mode

| Key | Does |
|---|---|
| `1`–`9` | start a count. `0` can follow another digit |
| `Ctrl-s` | save, as `:w` does |
| `/` `?` `n` `N` | search forward / back, repeat / reverse |
| `:` | the command line — see [Commands](commands.md) |
| `Ctrl-^` `Ctrl-6` | back to the previous buffer |

## Folds

In Normal and Visual mode. A fold shows only the first line of a list
item with sub-items or of a heading's section. See [Editing](../editing.md#folding).

| Key | Does |
|---|---|
| `za` | open the fold under the cursor, or fold the innermost item or section around it |
| `zc` `zo` | fold / unfold; in Visual mode, every fold in the selected lines |
| `zM` `zR` | fold every item and section / unfold everything |

## Command line, search and prompts

These keys work in the `:` line, the `/` and `?` search lines, and
the tree's rename, move and new prompts. The finder shares the editing
keys and handles selection separately.

| Key | Does |
|---|---|
| `Enter` | run, search, or answer |
| `Esc` `Ctrl-c` `Ctrl-[` | cancel |
| `Ctrl-u` | clear the line |
| `Ctrl-w` `Alt-Backspace` `Ctrl-Backspace` | delete the last word |
| `Backspace` `Ctrl-h` | delete the last character. On an empty line, cancel except in the finder |
| `Tab` `Shift-Tab` | in the `:` line, complete — see [Commands](commands.md#completing-with-tab) |

A tree delete prompt takes `y` or `Y` to confirm. Every other key cancels.

## Read-only

With `shoin -R`, `:view` or `<leader>v`. Every line is concealed and there is no caret.
See [Reading](../reading.md#read-only).

| Key | Does |
|---|---|
| `j` `k` | scroll a row down / up |
| `Ctrl-d` `Ctrl-u` | half a page down / up |
| `Ctrl-f` `Ctrl-b` | a page down / up |
| `gg` `G` | top / bottom |
| `/` `?` `n` `N` | search |
| `q` | close the buffer. Quit if it is the last one |
| `i` | switch to editing on entry to read-only or after a refused edit |
| `<leader>v` | switch to editing (it also turns read-only on) |

After moving or running another command, the first edit is refused with
"read-only — press i to edit". Yanking, panels, `gf` and `:w` still work.

## Leader keys

These are the shipped Normal-mode defaults from `keys.conf`. The
[Visual-mode bindings](#visual-mode) have their own leader keys. Remap
them under `[keys.normal]` or `[keys.visual]`, or set one to `"none"` to
remove it.

| Key | Does |
|---|---|
| `<leader>hl` | the shortcuts page |
| `<leader>w` `<leader>q` | save / quit |
| `<leader>F` | cycle focus mode |
| `<leader>t` | typewriter scrolling |
| `<leader>z` | zen mode |
| `<leader>c` | concealment on / off |
| `<leader>n` | line numbers |
| `<leader>a` | cycle text alignment |
| `<leader>v` | read-only on / off, as `:view` |
| `<leader>fe` `<leader>fE` | file tree in this file's folder / in `$HOME` |
| `<leader>ff` `<leader>fF` | find a file in this file's folder / in `$HOME` |
| `<leader>fb` | switch buffer |
| `<leader>sv` `<leader>ss` | split beside / below — again to close |
| `<leader>so` | keep only this pane |

## Panes

In Normal and Visual mode.

| Key | Does |
|---|---|
| `Ctrl-w h` `Ctrl-w j` `Ctrl-w k` `Ctrl-w l` / `Ctrl-w Left` `Ctrl-w Down` `Ctrl-w Up` `Ctrl-w Right` | move between panes |
| `Ctrl-w v` `Ctrl-w s` | split beside / below |
| `Ctrl-w q` `Ctrl-w c` / `Ctrl-w o` / `Ctrl-w w` | close / keep only this / cycle |
| `Ctrl-w >` `Ctrl-w <` `Ctrl-w +` `Ctrl-w -` | wider / narrower / taller / shorter (takes a count) |
| `Ctrl-w =` | equal sizes again |

## File tree

| Key | Does |
|---|---|
| `j` `Down` / `k` `Up` | move down / up |
| `l` `Enter` `o` `Right` | open a file or expand a folder |
| `h` `Left` | collapse or go to the parent |
| `g` `Home` / `G` `End` | top / bottom |
| `R` | refresh |
| `-` `=` | move the root up a level / into the selected folder |
| `H` | show / hide dotfiles |
| `a` | new file (end with `/` for a folder) |
| `r` `m` `d` | rename / move / delete |
| `<leader>hl` | the tree's keys, in `:help` — the leader keys work from the tree |
| `q` `Esc` | close the tree |
| `Ctrl-w l` `Ctrl-w w` `Ctrl-w j` `Ctrl-w k` / `Ctrl-w Right` `Ctrl-w Down` `Ctrl-w Up` | return to the editor |

## Fuzzy finder

| Key | Does |
|---|---|
| type | narrow the list |
| `Ctrl-n` `Ctrl-j` `Down` `Tab` / `Ctrl-p` `Ctrl-k` `Up` `Shift-Tab` | next / previous match |
| `Ctrl-u` | clear the query |
| `Ctrl-w` `Alt-Backspace` `Ctrl-Backspace` | delete its last word |
| `Backspace` `Ctrl-h` | delete its last character |
| `Enter` / `Esc` `Ctrl-c` `Ctrl-[` | open / close |

## :diff view

| Key | Does |
|---|---|
| `n` `p` | next / previous difference |
| `Tab` `Shift-Tab` | cycle this difference through live, file and both (either key) |
| `j` `Down` / `k` `Up` | scroll a line down / up |
| `Ctrl-d` `Ctrl-u` | scroll half a page down / up |
| `g` `Home` / `G` `End` | scroll to the top / bottom |
| `l` `f` `b` | set every difference to live / file / both |
| `w` | commit the merge and write it |
| `W` | in read-only mode: leave it, then commit and write |
| `q` `Esc` | abort |

## Help

| Key | Does |
|---|---|
| `Tab` `Right` `l` / `Shift-Tab` `Left` `h` | next / previous category |
| `1`–`7` | jump to a category |
| `j` `Down` / `k` `Up` | scroll a line down / up |
| `d` `PageDown` / `u` `PageUp` | scroll ten lines down / up |
| `g` `Home` / `G` `End` | scroll to the top / bottom |
| `/` | search all categories. `Enter` submits, `Ctrl-u` clears, `Backspace` deletes, and `Esc` cancels |
| `n` `N` | next / previous search match |
| `q` `Esc` | close help |

## Action names

The names a binding in `keys.conf` can use, under `[keys.normal]`,
`[keys.insert]` or `[keys.visual]`. The second column lists other spellings
of the same name. Default key is where the action already is, if anywhere.
See [Configuration](../configuration.md#key-bindings) for how bindings merge.

### Grammar

A binding for a piece of the Vim grammar keeps its counts, text objects
and doubled form. For example, `"s" = "operator_delete"`
gives `ss`, `siw` and `2s3w`.

| Name | Also | Default key | Does |
|---|---|---|---|
| `operator_delete` | `delete` | `d` | delete over a motion or object |
| `operator_change` | `change` | `c` | delete over a motion or object, then Insert |
| `operator_yank` | `yank` | `y` | copy over a motion or object |
| `operator_indent` | `indent` | `>` | indent over a motion or object |
| `operator_outdent` | `outdent` | `<` | outdent over a motion or object |
| `operator_lowercase` | `lowercase` | `gu` (`u` or `gu` in Visual) | lowercase over a motion or object |
| `operator_uppercase` | `uppercase` | `gU` (`U` or `gU` in Visual) | uppercase over a motion or object |
| `operator_toggle_case` | | `g~` (`~` or `g~` in Visual) | toggle case over a motion or object |
| `inner_object` | | `i` | the inside of a text object (`diw`) |
| `around_object` | | `a` | a text object with its surroundings (`daw`) |
| `register` | | `"` | name the register for the next command |
| `replace_char` | | `r` | replace the character under the cursor |
| `find_char` | | `f` | to the next given character on the line |
| `find_char_back` | | `F` | to the previous given character |
| `till_char` | | `t` | to just before the next given character |
| `till_char_back` | | `T` | to just after the previous given character |
| `repeat_find` | | `;` | the last `f` `F` `t` `T` again (`d;` too) |
| `repeat_find_back` | | `,` | the last `f` `F` `t` `T` the other way |
| `goto_line` | | `G` | the last line, or line N with a count |
| `goto_line_first` | | `gg` | the first line, or line N with a count |

### Motions

| Name | Also | Default key | Does |
|---|---|---|---|
| `move_left` `move_right` | | `h` `l` | a character left / right |
| `move_up` `move_down` | | `k` `j` | a line up / down |
| `word_forward` `word_back` | | `w` `b` | next / previous word start |
| `word_end` | | `e` | end of the word |
| `line_start` | | `0` | start of the line |
| `line_first_non_blank` | | `^` | first non-blank of the line |
| `line_end` | | `$` | end of the line |
| `paragraph_forward` `paragraph_back` | | `}` `{` | next / previous paragraph |
| `half_page_down` `half_page_up` | | `Ctrl-d` `Ctrl-u` | half a page |
| `page_down` `page_up` | | `Ctrl-f` `Ctrl-b` | a page |
| `screen_top` `screen_middle` `screen_bottom` | | `H` `M` `L` | top / middle / bottom of the screen |

### Editing

| Name | Also | Default key | Does |
|---|---|---|---|
| `insert` `append` | | `i` `a` | Insert before / after the cursor |
| `insert_line_start` `append_line_end` | | `I` `A` | Insert at the line's first non-blank / end |
| `open_below` `open_above` | | `o` `O` | a new line below / above, in Insert |
| `normal_mode` | `escape` | `Esc` | back to Normal |
| `visual` `visual_line` | | `v` `V` | Visual charwise / linewise |
| `delete_char` | | `x` (Normal) | delete the character under the cursor |
| `join_lines` | | `J` | join the next line onto this one |
| `paste_after` | `paste` | `p` | paste after the cursor |
| `paste_before` | | `P` | paste before the cursor |
| `move_lines_down` | `move_line_down` | | move the selected lines (the cursor's line in Normal) down, keeping them selected |
| `move_lines_up` | `move_line_up` | | the same, up |
| `undo` `redo` | | `u` `Ctrl-r` | undo / redo in Normal |
| `repeat` | | `.` | repeat the last change in Normal |
| `list_item_down` `list_item_up` | | `Alt-j` `Alt-k` | move the list item, or in Visual the selected items, past the next / previous one |
| `list_indent` `list_outdent` | | `Tab` `Shift-Tab` (Insert) | indent / outdent the list item |
| `fix_list` | `fixlist` | | re-indent the list under the cursor, like `:fixlist` |

### Writer verbs

In Normal mode on the word under the cursor, in Visual on the selection.

| Name | Also | Default key | Does |
|---|---|---|---|
| `toggle_bold` | `bold` | `gb`, `<leader>b` (Visual) | toggle `**bold**` |
| `toggle_italic` | `italic` | `gi`, `<leader>i` (Visual) | toggle `*italic*` |
| `toggle_highlight` | `highlight` | `gh` | toggle `==highlight==` |
| `toggle_code` | `code` | `gk` | toggle `` `code` `` |
| `insert_link` | `link` | `gl` | wrap as a link, cursor in the URL |
| `toggle_task` | `task` | `gt` | toggle a task checkbox |
| `align_table` | `table` | `ga` | align the table under the cursor |
| `heading_1` `heading_2` `heading_3` `heading_4` `heading_5` `heading_6` | | `g1` `g2` `g3` `g4` `g5` `g6` | set the heading level |
| `clear_heading` | | `g0` | strip the heading |

### Files, search and commands

| Name | Also | Default key | Does |
|---|---|---|---|
| `save` | `write` | `Ctrl-s`, `<leader>w` | save |
| `save_stay_insert` | | `Ctrl-s` (Insert) | save without leaving Insert |
| `save_quit` | `write_quit` `wq` | | save and quit |
| `quit` | | `<leader>q` | quit, refusing with unsaved changes |
| `force_quit` | `quit!` | | quit, discarding changes |
| `command` | | `:` | the command line |
| `search_forward` | `search` | `/` | search forward |
| `search_backward` | | `?` | search backward |
| `follow_link` | `follow` `goto_file` | `gf` | follow the link under the cursor |
| `open_external` | `open_url` `xdg_open` | `gx` | open the link with the desktop |

### View

| Name | Also | Default key | Does |
|---|---|---|---|
| `scroll_view_down` | `scroll_down` | `Ctrl-e` | scroll the page a line down |
| `scroll_view_up` | `scroll_up` | `Ctrl-y` | scroll the page a line up |
| `scroll_cursor_top` | | `zt` | scroll the cursor's line to the top of the pane |
| `scroll_cursor_middle` | | `zz` | scroll the cursor's line to the middle of the pane |
| `scroll_cursor_bottom` | | `zb` | scroll the cursor's line to the bottom of the pane |
| `cycle_focus` | `focus` | `<leader>F` | cycle focus mode |
| `toggle_typewriter` | `typewriter` | `<leader>t` | typewriter scrolling on / off |
| `toggle_conceal` | `conceal` | `<leader>c` | concealment on / off |
| `toggle_zen` | `zen` | `<leader>z` | zen mode on / off |
| `toggle_numbers` | `numbers` `number` | `<leader>n` | line numbers on / off |
| `cycle_align` | `align` | `<leader>a` | cycle text alignment |
| `toggle_view` | `view` `read_only` | `<leader>v` | read-only mode on / off, as `:view` |
| `help` | `shortcuts` | `<leader>hl` | the shortcuts page |
| `fold_toggle` | | `za` | fold or unfold here |
| `fold_close` | | `zc` | fold the innermost open item or section |
| `fold_open` | | `zo` | unfold the fold under the cursor |
| `fold_close_all` | | `zM` | fold every item and section |
| `fold_open_all` | | `zR` | unfold everything |

### Panels and panes

| Name | Also | Default key | Does |
|---|---|---|---|
| `file_tree` | `file_explorer` `tree` | `<leader>fe` | file tree in this file's folder, or the launch folder with `tree.root = "cwd"` |
| `file_tree_home` | `file_explorer_home` | `<leader>fE` | file tree over `$HOME` |
| `find_file` | `fuzzy_find` `files` | `<leader>ff` | fuzzy finder in this file's folder, or the launch folder with `tree.root = "cwd"` |
| `find_file_home` | `fuzzy_find_home` | `<leader>fF` | fuzzy finder over `$HOME` |
| `file_tree_cwd` | `file_explorer_cwd` | | file tree in the folder Shoin was launched from |
| `find_file_cwd` | `fuzzy_find_cwd` | | fuzzy finder in the folder Shoin was launched from |
| `find_buffer` | `buffers` `switch_buffer` | `<leader>fb` | switch buffer |
| `alternate_buffer` | `alternate` `alt_buffer` | `Ctrl-^` `Ctrl-6` | back to the previous buffer |
| `split_vertical` | `vsplit` | `<leader>sv` | split beside, or close the split |
| `split_horizontal` | `split` | `<leader>ss` | split below, or close the split |
| `close_pane` | | `Ctrl-w q` `Ctrl-w c` | close this pane |
| `only_pane` | `only` | `<leader>so` | keep only this pane |

### Unbinding

| Name | Also | Default key | Does |
|---|---|---|---|
| `none` | `nop` `""` | | nothing: removes a binding, shipped or built in |

## Details

- `gg`, `G`, `H`, `M`, `L`, `Ctrl-d`, `Ctrl-u`, `Ctrl-f` and `Ctrl-b` keep
  the cursor's column, as `j` and `k` do: on a shorter line it stops at the
  end, and a longer line gets the column back. `dd`, `>>` and `<<` leave
  the cursor in that column too. This is Neovim's default.
- `zt` and `zb` leave `editor.scroll_off` lines at the edge when there is
  room. `zt`, `zz` and `zb` have no effect in typewriter or read-only mode.
- Normal and Visual share motions and prefixes except where a Visual entry
  gives a different meaning. Shipped leader keys apply in Normal unless
  listed in Visual. The tree accepts the Normal leader sequences.
- `heading_1` through `heading_6` also accept a leading `+` or zeroes in
  the numeric suffix, such as `heading_+1` or `heading_01`.
- Brackets can span lines. Indent and outdent refuse a shift that would
  break a list. See [Editing](../editing.md#lists).
- The bottom hint bar shows keys for the current mode. A pending leader,
  prefix or operator can show its continuations over the status line.
  Leader hints put the most used keys first. Use `:set hints off` to hide
  the bar for the session, or set `[hints] enabled = false` in your config.
