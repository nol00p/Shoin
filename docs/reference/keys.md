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
| `Ctrl-e` `Ctrl-y` | scroll the page a line down / up; the cursor stays put |

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
| `ga` | align the table under the cursor (`editor.table_align = "auto"` also does it on leaving Insert) |
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

To rebind them, the actions are `list_item_down` and `list_item_up` (Normal)
and `list_indent` and `list_outdent` (Insert) — see
[Configuration](../configuration.md#key-bindings).

## Insert mode

| Key | Does |
|---|---|
| `i` `a` `I` `A` `o` `O` | insert / append / at line start / at line end / open below / open above |
| `Esc` `Ctrl-c` `jk` | leave Insert (`jk` is `input.escape_alias`) |
| `Ctrl-s` | save, and keep typing |
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

`move_lines_down` and `move_lines_up` move the selected lines down or up a
line and keep them selected, like Vim's `:'<,'>m '>+1 | gv`. They take a
count, each press is one undo step, and in Normal mode they move the cursor's
line. They have no key until you give them one:

```toml
[keys.visual]
"J" = "move_lines_down"
"K" = "move_lines_up"
```

## Other

| Key | Does |
|---|---|
| `u` `Ctrl-r` | undo / redo |
| `.` | repeat the last change |
| `Ctrl-s` | save, as `:w` does |
| `/` `?` `n` `N` | search forward / back, repeat / reverse |
| `:` | the command line — see [Commands](commands.md) |
| `Ctrl-^` `Ctrl-6` | back to the previous buffer |

## Read-only

With `shoin -R`, `:view` or `Space v`. Every line is concealed and there is no caret.
See [Reading](../reading.md#read-only).

| Key | Does |
|---|---|
| `j` `k` | scroll a line down / up |
| `Ctrl-d` `Ctrl-u` | half a page down / up |
| `gg` `G` | top / bottom |
| `/` `?` `n` `N` | search |
| `q` | quit |
| `i` | switch to editing |
| `Space v` | switch to editing (it also turns read-only on) |

An edit is refused with "read-only — press i to edit". Yanking, panels, `gf`
and `:w` still work.

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
| `Space v` | read-only on / off, as `:view` |
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

## Action names

The names a binding in `keys.conf` can use, under `[keys.normal]`,
`[keys.insert]` or `[keys.visual]`. The second column lists other spellings
of the same name. Default key is where the action already is, if anywhere.
See [Configuration](../configuration.md#key-bindings) for how bindings merge.

### Grammar

These are pieces of the Vim grammar, not finished commands, so a key bound to
one keeps its counts, text objects and doubled form: `"s" = "operator_delete"`
gives `ss`, `siw` and `2s3w`.

| Name | Also | Default key | Does |
|---|---|---|---|
| `operator_delete` | `delete` | `d` | delete over a motion or object |
| `operator_change` | `change` | `c` | delete over a motion or object, then Insert |
| `operator_yank` | `yank` | `y` | copy over a motion or object |
| `operator_indent` | `indent` | `>` | indent over a motion or object |
| `operator_outdent` | `outdent` | `<` | outdent over a motion or object |
| `operator_lowercase` | `lowercase` | `gu` (`u` in Visual) | lowercase over a motion or object |
| `operator_uppercase` | `uppercase` | `gU` (`U` in Visual) | uppercase over a motion or object |
| `inner_object` | | `i` | the inside of a text object (`diw`) |
| `around_object` | | `a` | a text object with its surroundings (`daw`) |
| `register` | | `"` | name the register for the next command |
| `replace_char` | | `r` | replace the character under the cursor |
| `find_char` | | `f` | to the next given character on the line |
| `find_char_back` | | `F` | to the previous given character |
| `till_char` | | `t` | to just before the next given character |
| `till_char_back` | | `T` | to just after the previous given character |
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
| `insert_line_start` `append_line_end` | | `I` `A` | Insert at the line's start / end |
| `open_below` `open_above` | | `o` `O` | a new line below / above, in Insert |
| `normal_mode` | `escape` | `Esc` | back to Normal |
| `visual` `visual_line` | | `v` `V` | Visual charwise / linewise |
| `delete_char` | | `x` | delete the character under the cursor |
| `join_lines` | | `J` | join the next line onto this one |
| `paste_after` | `paste` | `p` | paste after the cursor |
| `paste_before` | | `P` | paste before the cursor |
| `move_lines_down` | `move_line_down` | | move the selected lines (the cursor's line in Normal) down, keeping them selected |
| `move_lines_up` | `move_line_up` | | the same, up |
| `undo` `redo` | | `u` `Ctrl-r` | undo / redo |
| `repeat` | | `.` | repeat the last change |
| `list_item_down` `list_item_up` | | `Alt-j` `Alt-k` | move the list item past the next / previous one |
| `list_indent` `list_outdent` | | `Tab` `Shift-Tab` (Insert) | indent / outdent the list item |

### Writer verbs

In Normal mode on the word under the cursor, in Visual on the selection.

| Name | Also | Default key | Does |
|---|---|---|---|
| `toggle_bold` | `bold` | `gb`, `Space b` (Visual) | toggle `**bold**` |
| `toggle_italic` | `italic` | `gi`, `Space i` (Visual) | toggle `*italic*` |
| `toggle_highlight` | `highlight` | `gh` | toggle `==highlight==` |
| `toggle_code` | `code` | `gk` | toggle `` `code` `` |
| `insert_link` | `link` | `gl` | wrap as a link, cursor in the URL |
| `toggle_task` | `task` | `gt` | toggle a task checkbox |
| `align_table` | `table` | `ga` | align the table under the cursor |
| `heading_1` … `heading_6` | | `g1` … `g6` | set the heading level |
| `clear_heading` | | `g0` | strip the heading |

### Files, search and commands

| Name | Also | Default key | Does |
|---|---|---|---|
| `save` | `write` | `Ctrl-s`, `Space w` | save |
| `save_stay_insert` | | `Ctrl-s` (Insert) | save without leaving Insert |
| `save_quit` | `write_quit` `wq` | | save and quit |
| `quit` | | `Space q` | quit, refusing with unsaved changes |
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
| `cycle_focus` | `focus` | `Space F` | cycle focus mode |
| `toggle_typewriter` | `typewriter` | `Space t` | typewriter scrolling on / off |
| `toggle_conceal` | `conceal` | `Space c` | concealment on / off |
| `toggle_zen` | `zen` | `Space z` | zen mode on / off |
| `toggle_numbers` | `numbers` `number` | `Space n` | line numbers on / off |
| `cycle_align` | `align` | `Space a` | cycle text alignment |
| `toggle_view` | `view` `read_only` | `Space v` | read-only mode on / off, as `:view` |
| `help` | `shortcuts` | `Space hl` | the shortcuts page |

### Panels and panes

| Name | Also | Default key | Does |
|---|---|---|---|
| `file_tree` | `file_explorer` `tree` | `Space fe` | file tree in this file's folder, or the launch folder with `tree.root = "cwd"` |
| `file_tree_home` | `file_explorer_home` | `Space fE` | file tree over `$HOME` |
| `find_file` | `fuzzy_find` `files` | `Space ff` | fuzzy finder in this file's folder, or the launch folder with `tree.root = "cwd"` |
| `find_file_home` | `fuzzy_find_home` | `Space fF` | fuzzy finder over `$HOME` |
| `file_tree_cwd` | `file_explorer_cwd` | | file tree in the folder Shoin was launched from |
| `find_file_cwd` | `fuzzy_find_cwd` | | fuzzy finder in the folder Shoin was launched from |
| `find_buffer` | `buffers` `switch_buffer` | `Space fb` | switch buffer |
| `alternate_buffer` | `alternate` `alt_buffer` | `Ctrl-^` | back to the previous buffer |
| `split_vertical` | `vsplit` | `Space sv` | split beside, or close the split |
| `split_horizontal` | `split` | `Space ss` | split below, or close the split |
| `close_pane` | | `Ctrl-w q` | close this pane |
| `only_pane` | `only` | `Space so` | keep only this pane |

### Unbinding

| Name | Also | Default key | Does |
|---|---|---|---|
| `none` | `nop` `""` | | nothing: removes a binding, shipped or built in |
