# Commands

Type `:` and the command, then `Enter`.

`Tab` completes the word at the end of the line: a command's name, a path
after `:e`, `:w`, `:saveas` and `:wq`, an option and then its value after
`:set`, a topic after `:help`, an open buffer after `:b`, and the values of
`:focus`, `:align`, `:embed`, `:number` and `:export`. See
[Completing with Tab](#completing-with-tab).

## Files

| Command | Does |
|---|---|
| `:w` `:write [path]` | save; with a path, write a copy there and keep editing this file (an unnamed buffer takes the name) |
| `:saveas <path>` `:sav` | save to a path and edit that file from now on |
| `:w!` | save over a file that changed on disk, or an existing file `:w <path>` refuses |
| `:wq` `:x` | save and quit; `:wq!` even if the file changed |
| `:q` | close this buffer; the last one quits |
| `:q!` | the same, discarding changes |
| `:Q` `:qa` | quit every buffer at once; `:Q!` `:qa!` discard changes |
| `:e <path>` | open a file in a new buffer; `~/` is your home folder |
| `:diff` | merge against the file on disk — see [Files](../files.md#merging-with-diff) |
| `:revert` `:revert!` | re-read the file from disk; `!` discards unsaved changes |
| `:export [md\|txt\|html\|pdf] [justify]` | write the finished document — see [Export](../export.md) |

## Editing

| Command | Does |
|---|---|
| `:u` `:un` `:und` `:undo` | undo, like `u` |
| `:red` `:redo` | redo, like `Ctrl-r` |
| `:fixlist` | re-indent the list under the cursor, or every list a Visual selection touches, so other tools read it as Shoin shows it — see [Editing](../editing.md#lists) |

## Buffers and panes

| Command | Does |
|---|---|
| `:ls` `:buffers` | list open buffers (▸ marks the current one) |
| `:b <name>` `:b <n>` | switch by name fragment or number |
| `:bn` `:bp` | next / previous buffer |
| `:bd` `:bd!` | close this buffer (`!` discards changes) |
| `:sp` `:vs` | split horizontally / vertically |
| `:close` | close the focused pane |
| `:only` | close every pane but this one |

## View

| Command | Does |
|---|---|
| `:embed [none\|short\|rec\|full]` | how much of each `![[embed]]` to show; bare toggles |
| `:focus [off\|paragraph\|sentence]` | dim all but the paragraph or sentence; bare cycles |
| `:number [relative\|absolute]` | line numbers; bare toggles off and back |
| `:align [left\|center\|right\|justified]` | how lines sit in the column; bare cycles |
| `:typewriter` `:tw` | typewriter scrolling |
| `:zen` | hide all chrome |
| `:view` | read-only mode on or off — see [Reading](../reading.md#read-only) |

## Settings

`:set <key> [value]` changes a setting for this session.

| Command | Does |
|---|---|
| `:set measure=72` | text width in columns (`:set measure` shows it) |
| `:set line_spacing=1` | blank rows between lines, 0–4 |
| `:set tab_width=4` | cells a tab counts and is drawn as, 1–16 (`:set ts`) |
| `:set autosave [on\|off]` | write modified named buffers on a timer |
| `:set autosave_interval=3` | minutes between autosaves, 1–5 |
| `:set autoreload [on\|off]` | take outside changes when nothing is unsaved |
| `:set clipboard [on\|off]` | plain `y` `d` `p` use the system clipboard (`:set cb`) |
| `:set conceal\|typewriter\|mouse\|focus\|numbers\|code_syntax\|hints [on\|off]` | toggle these; `hints` is the key-hint bar |

## Config and help

| Command | Does |
|---|---|
| `:e!` `:reload` | re-read the config |
| `:help [topic]` | help, a tab per category — topics: `basics`, `navigation`, `editing`, `writing`, `view`, `files`, `config`. `Tab` and `Shift-Tab` switch tab |
| `:help <word>` | help opened at a key, command, setting or section: `:help gb`, `:help view`, `:help measure`, `:help tree`. Inside help, `/` searches every tab and `n` `N` move between matches |

## Completing with `Tab`

`Tab` puts the first match in place of the word being typed. `Tab` again
puts the next, `Shift-Tab` the one before. Past the last match the line goes
back to what you typed, and the next `Tab` starts the list again. Any other
key keeps the line as it is and ends the list; `Enter` runs it, `Esc`
cancels it.

While you cycle, the hint bar lists the matches, the one on the line in
brackets.

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
- A value completes ignoring case: `:align C` gives `:align center`.
- `:b` matches anywhere in a buffer's name, ignoring case, as `:b` itself
  does: `:b a.m` offers `data.md` and `a.md`. A whole name goes to its own
  buffer first, so `:b a.md` opens `a.md` even when `data.md` was opened
  before it. Two buffers with the same name are offered once and `:b` goes
  to the first; reach the other by its number from `:ls`, as in `:b 3`.
- `:export` offers `justify` after `txt` or `html`, the formats it applies
  to. Typed first, `justify` is followed by those two formats only.
- The `:` line has no cursor to move: `Tab` always completes the last word.
