# Commands

Commands manage files, panes, and settings from the command line. Type `:`,
the command, then `Enter`.

`Tab` completes the word at the end of the line: a command's name, a path
after `:e`, `:w`, `:saveas` and `:wq`, an option and then its value after
`:set`, a topic after `:help`, an open buffer after `:b`, and the values of
`:focus`, `:align`, `:embed`, `:number` and `:export`. See
[Completing with Tab](#completing-with-tab).

## Files

| Command | Does |
|---|---|
| `:write [path]` `:w` | save; with a path, write a copy there and keep editing this file. An unnamed buffer takes the name. Add `!` to overwrite a changed or existing file |
| `:saveas <path>` `:sav` | save to a path and edit that file from now on. Add `!` to overwrite an existing file |
| `:wq [path]` `:x` | save and quit. A path saves under that name. Add `!` to force the write. A failed forced write keeps the editor open |
| `:q` | close this buffer; the last one quits |
| `:q!` | the same, discarding changes |
| `:qall` `:qa` `:Q` | quit every buffer. Add `!` to discard changes |
| `:edit <path>` `:e` | open a file in a new buffer. `~/` is your home folder |
| `:diff` `:diffsplit` | merge against the file on disk. See [Files](../files.md#merging-with-diff) |
| `:revert` `:revert!` | re-read the file from disk; `!` discards unsaved changes |
| `:export [md\|txt\|html\|pdf] [justify]` | write the finished document — see [Export](../export.md) |

## Editing

| Command | Does |
|---|---|
| `:undo` `:u` `:un` `:und` | undo, like `u` |
| `:redo` `:red` | redo, like `Ctrl-r` |
| `:fixlist` | re-indent the list under the cursor, or every list a Visual selection or a range touches, so other tools read it as Shoin shows it — see [Editing](../editing.md#lists) |

## Lines

A line address is a number, `.` for the cursor's line or `$` for the last,
then any offsets: `+3`, `-2`, a bare `+` or `-` for one. `:+3` counts from
the cursor's line. A range is two addresses with a comma, `:3,7` or
`:.,+4`, or `%` for every line. `:` from Visual starts the line with
`'<,'>`, the selected lines.

| Command | Does |
|---|---|
| `:42` | go to line 42, keeping the column, like `42G`. `:0` is the first line, and a number past the end is the last |
| `:$` | go to the last line, like `G` |
| `:+3` `:-2` `:.+3` `:$-1` | go to the line that address names. A range goes to its last line |
| `:delete [x] [count]` `:d` `:de` `:del` | delete the lines into the register, as `dd`. The cursor goes to the first non-blank of the line after them |
| `:yank [x] [count]` `:y` `:ya` `:yan` | yank the lines, as `yy`. The cursor stays where it is |
| `:>` `:<` | indent or outdent the lines one level, as `>>` and `<<`. `:>>` goes two levels |
| `:move {line}` `:m` `:mo` `:mov` | move the lines below `{line}`. `:m 0` moves them to the top, `:m $` to the end |
| `:copy {line}` `:co` `:cop` `:t` | copy the lines below `{line}` |
| `:{range}w <path>` | write only those lines to `path`. Add `!` to overwrite an existing file |

Each command takes a range before its name, as in `:3,7d`, `:%y` or
`:'<,'>m 0`, and works on the cursor's line without one. `:fixlist` takes a
range too.

## Buffers and panes

| Command | Does |
|---|---|
| `:buffers` `:ls` | list open buffers (▸ marks the current one) |
| `:buffer [name or number]` `:b` | switch by number or name fragment, preferring a whole name. Bare lists buffers |
| `:bnext` `:bn` | switch to the next buffer |
| `:bprevious` `:bp` `:bprev` | switch to the previous buffer |
| `:bdelete` `:bd` | close this buffer. Add `!` to discard changes. The last buffer stays open |
| `:split` `:sp` | split below |
| `:vsplit` `:vs` `:vsp` | split beside |
| `:close` `:clo` | close the focused pane |
| `:only` `:on` | keep only the focused pane |

## View

| Command | Does |
|---|---|
| `:embed [none\|short\|rec\|full]` | how much of each `![[embed]]` to show; bare toggles |
| `:focus [off\|paragraph\|sentence]` | dim all but the paragraph or sentence; bare cycles |
| `:number [off\|relative\|absolute]` `:nu` | set line numbers. Bare toggles off and back |
| `:align [left\|center\|right\|justified]` | how lines sit in the column; bare cycles |
| `:typewriter` `:tw` | typewriter scrolling |
| `:zen` | hide all chrome |
| `:view` | read-only mode on or off — see [Reading](../reading.md#read-only) |

## Settings

Settings changed with `:set` apply to the current session.

| Option | Also | Values | Default | Does |
|---|---|---|---|---|
| `measure` | `width` | 8–65535 | 72 | text width in columns |
| `line_spacing` | `spacing` | 0–4 | 0 | blank rows between lines. Higher values clamp to 4 |
| `tab_width` | `ts` | 1–16 | 4 | cells per tab for display and indentation. Changes reclassify open documents |
| `autosave` |  | on / off | off | write modified named buffers on a timer |
| `autosave_interval` | `autosave_min` | 1–5 | 3 | minutes between autosaves |
| `autoreload` |  | on / off | on | take outside changes when nothing is unsaved |
| `clipboard` | `cb` | on / off | off | use the system clipboard for plain `y`, `d` and `p` |
| `code_syntax` | `syntax` | on / off | on | highlight recognized fenced code |
| `conceal` |  | on / off | on | hide Markdown markers away from the cursor |
| `typewriter` | `tw` | on / off | off | keep the cursor line at a fixed height |
| `mouse` |  | on / off | on | capture mouse clicks, drags and scrolling |
| `hints` |  | on / off | on | show the key-hint bar |
| `focus` |  | off / paragraph / sentence | off | dim text outside the focus |
| `numbers` | `number` `nu` | off / relative / absolute | off | set the line-number gutter |
| `align` |  | left / center / right / justified | left | align the text inside the column |
| `embed` |  | none / short / rec / full | none | choose how embedded files expand |

Use a space or `=` before a value, as in `:set ts 4`. With no value,
numeric options show the current value. `align` and `focus` cycle.
`embed` and `numbers` toggle off and back. Boolean options toggle.

## Config and help

| Command | Does |
|---|---|
| `:e!` `:reload` | re-read the config |
| `:set <key> [value]` | change a setting for this session |
| `:help [topic]` `:h` | open help. Topics include `basics`, `navigation`, `editing`, `writing`, `view`, `files`, `config`. `Tab` and `Shift-Tab` switch tabs |

Give `:help` a key, command, setting or section to open it directly, such as
`:help gb`, `:help view`, `:help measure` or `:help tree`. Inside help,
`/` searches every tab and `n` and `N` move between matches.

## Completing with Tab

`Tab` puts the first match in place of the word being typed. `Tab` again
puts the next, `Shift-Tab` the one before. Past the last match the line goes
back to what you typed, and the next `Tab` starts the list again. Any other
key keeps the line as it is and ends the list; `Enter` runs it, `Esc`
cancels it.

With hints enabled, the bottom row lists the matches while you cycle.
Brackets mark the current match.

| Typed after `:` | `Tab` gives |
|---|---|
| `bu` | `buffer`, then `buffers` |
| `e no` | `e notes/`, then `e notes.md` |
| `set au` | `set autoreload`, `set autosave`, `set autosave_interval` |
| `help ed` | `help edit`, `help editing`, `help edition` |
| `set focus ` | `set focus off`, `set focus paragraph`, `set focus sentence` |
| `set autosave=` | `set autosave=off`, `set autosave=on` |
| `align ` | `align left`, `align center`, `align right`, `align justified` |
| `export ` | `export md`, `export txt`, `export html`, `export pdf` |
| `export html ` | `export html justify` |
| `b no` | each open buffer whose name holds `no` |

## Details

- The force forms are `:write!` / `:w!`, `:saveas!` / `:sav!`, `:wq!` / `:x!`, `:qall!` / `:qa!` / `:Q!`, `:bdelete!` / `:bd!`,
  `:q!` and `:revert!`. `:e!` reloads the config.

- A line address, alone, opens a fold it lands in, as `42G` does.
- A number after a space adds to the address before it, as in vim: `:3 2`
  is line 5.
- An address past the end is the last line, and one before the start is
  the first, where Neovim refuses some of them. A range given backwards,
  `:7,3`, is turned round without asking.
- `[x]` is a register, as `"x` names one before `dd`: `:d a` deletes into
  `a`, and `:y A` appends to it. A count counts lines from the range's last
  line: `:d 3` deletes the cursor's line and the two below, and `:5,6d 2`
  deletes lines 6 and 7. `:>` and `:<` take a count too.
- A closed fold at either end of a range is taken whole, as `dd` takes it.
- `:m` can't put lines below one of themselves: `:2,4m 3` is refused and
  changes nothing.
- `'<,'>` names the lines of the selection `:` was typed from, a
  character or line selection alike. Visual ends when the command runs.
  Without one, `'<` and `'>` are an error: they are not marks.
- `.` does not repeat a `:` command: after `:d` it repeats the change made
  before it, as in vim.
- In read-only mode `:d`, `:>`, `:<`, `:m`, `:t` and `:fixlist` are refused
  like any edit. Line addresses, `:y` and a ranged `:w` work.
- `:w` with a range and no path is an error, and so is a range written to
  this buffer's own file. Lines written before the last end in a newline.
- Each line command is one undo step. `u` and `Ctrl-r` put the cursor where
  Neovim's do.
- Names, paths and topics are in alphabetical order, ignoring case. Folders
  and files are mixed, and a folder ends in `/`.
- Values are in the order the command lists them, `off` before `on`.
  Buffers are in the order they were opened.
- Command names complete to their full name: `wr` gives `write`, and
  nothing offers `w`. The short names still run.
- A path is relative to the folder Shoin was started in, as `:e` reads it.
  `~/` is your home folder, and `:e ~` completes to `:e ~/`.
- Hidden files and folders are offered only when the typed name starts with
  `.`.
- A path may hold spaces: everything after the command is the word, so
  `:e my no` completes to `:e my notes.md`.
- When there is only one match, `Tab` takes it and the list ends. On a
  folder, the next `Tab` lists what is inside it.
- `Shift-Tab` before any `Tab` starts at the last match.
- `Tab` on an empty line goes through every command.
- With no match, `Tab` does nothing and says nothing.
- After `:set`, a value completes once the option is followed by `=` or a
  blank. A toggle offers `off` and `on`. An option that takes a number,
  such as `measure` or `tab_width`, offers nothing.
- Boolean values `on`, `true`, `1` and `yes` turn an option on. Any
  other nonempty value turns it off. The match is case-sensitive.
- Value aliases for `align` are `l`, `centre`, `c`, `r`, `justify` and
  `j`. For `focus` they are `none`, `para` and `sent`. For `embed` they
  are `off`, `false`, `no`, `on`, `true`, `yes`, `recursive` and
  `long`. For `numbers` they are `none`, `false`, `rel`, `rnu`, `abs`,
  `nu`, `number`, `on` and `true`.
- A value completes ignoring case: `:align C` gives `:align center`.
- `:b` matches anywhere in a buffer's name, ignoring case, as `:b` itself
  does: `:b a.m` offers `data.md` and `a.md`. A whole name goes to its own
  buffer first, so `:b a.md` opens `a.md` even when `data.md` was opened
  before it. Two buffers with the same name are offered once and `:b` goes
  to the first; reach the other by its number from `:ls`, as in `:b 3`.
- `:export` offers `justify` after `txt` or `html`, the formats it applies
  to. Typed first, `justify` is followed by those two formats only.
  The same modifier accepts `justified`. Format aliases are `markdown`,
  `text`, `htm` and `web`.
- The `:` line has no cursor to move: `Tab` always completes the last word.
