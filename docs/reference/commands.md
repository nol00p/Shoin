# Commands

Type `:` and the command, then `Enter`.

## Files

| Command | Does |
|---|---|
| `:w` `:write [path]` | save, optionally to a path |
| `:w!` | save over a file that changed on disk, or an existing file `:w <path>` refuses |
| `:wq` `:x` | save and quit; `:wq!` even if the file changed |
| `:q` | close this buffer; the last one quits |
| `:q!` | the same, discarding changes |
| `:Q` `:qa` | quit every buffer at once; `:Q!` `:qa!` discard changes |
| `:e <path>` | open a file in a new buffer |
| `:diff` | merge against the file on disk — see [Files](../files.md#merging-with-diff) |
| `:revert` `:revert!` | re-read the file from disk; `!` discards unsaved changes |
| `:export [md\|txt\|html\|pdf] [justify]` | write the finished document — see [Export](../export.md) |

## Editing

| Command | Does |
|---|---|
| `:u` `:un` `:und` `:undo` | undo, like `u` |
| `:red` `:redo` | redo, like `Ctrl-r` |

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

## Settings

`:set <key> [value]` changes a setting for this session.

| Command | Does |
|---|---|
| `:set measure=72` | text width in columns (`:set measure` shows it) |
| `:set line_spacing=1` | blank rows between lines, 0–4 |
| `:set autosave [on\|off]` | write modified named buffers on a timer |
| `:set autosave_interval=3` | minutes between autosaves, 1–5 |
| `:set autoreload [on\|off]` | take outside changes when nothing is unsaved |
| `:set clipboard [on\|off]` | plain `y` `d` `p` use the system clipboard (`:set cb`) |
| `:set conceal\|typewriter\|mouse\|focus\|numbers\|code_syntax [on\|off]` | toggle these |

## Config and help

| Command | Does |
|---|---|
| `:e!` `:reload` | re-read the config |
| `:help [topic]` | help — topics: `shortcuts`, `bindings`, `commands`, `writer`, `config` |
