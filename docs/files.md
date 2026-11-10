# Files

Shoin saves atomically, never autosaves unless you ask, and never quietly
overwrites changes made by something else. If it crashes or is killed with
unsaved work, that work goes to a recovery file. When a file changes on disk while
you have unsaved work, `:diff` lets you merge the two.

## Saving

`:w` writes atomically: the text goes to a temporary file beside the target, is
fsynced, and is renamed over it, so a crash mid-save leaves the original
intact. If the file changed on disk since Shoin read it, the write is refused —
`:w!` is how you say you meant it.

## Autosave

Autosave is off by default. Turn it on and it writes every modified, named
buffer on a timer:

```
  :set autosave                     on, every 3 minutes
  :set autosave_interval=1          1 to 5 minutes
  :set autosave off                 back to explicit :w only
```

```toml
[editor]
autosave          = true
autosave_interval = 3     # minutes, 1–5
```

It keeps the same guard as `:w`: a file something else has written is left
alone and named on the status line, never overwritten while you weren't
looking.

## When something else edits the file

Shoin watches the files you have open, when the terminal regains focus and
once a second besides. What happens depends on one question: **do you have
unsaved changes?**

- **You don't.** The file wins and the buffer takes its changes, as a single
  undo step, so `u` gives back what was on screen.
- **You do.** Nothing is touched. The filename gets a ⚠ on the status line and
  waits for you:

```
  :diff      merge the two versions, difference by difference
  :w!        keep mine — write over what changed on disk
  :revert!   take theirs — re-read the file, discarding my changes
```

## After a crash

Shoin writes nothing while it runs that you did not ask for. The one exception
is when it dies with unsaved work: a crash, a `kill`, or the terminal window
closing. Then each modified buffer is written to a recovery file beside its
file, and the paths are printed once the terminal is back:

```
  notes.md                  your file, as last saved
  notes.md.shoin-recover    the text you had not saved
```

Opening `notes.md` again says the recovery file is there. Nothing applies it
for you: compare the two (`diff notes.md notes.md.shoin-recover`), keep what
you want, and delete the recovery file.

## Merging with :diff

`:diff` lines your buffer up against the file, row for row, with a filler bar
wherever one side has no line. Every difference is set to **live**, **file**
or **both**, and the divider points at whichever wins:

```
  live — in the editor             │ file — on disk
  # On attention                   │ # On attention
                                   │
  A draft paragraph                │ A draft paragraph
 ~my edited line                   ◀~their edited line     ← live wins here
 ┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄┄◆+their added line      ← both are kept
  tail                             │ tail
```

```
  n / p    move between differences
  Tab      cycle this one:  live → file → both
  l f b    set every difference to live / file / both
  w        commit the merged text and write it
  q  Esc   abort
```

The losing side is struck through, so the document you're about to get reads
straight down the screen. **Nothing is written until `w`**, so `q` is a real
abort, and the commit lands as a single undo step. `both` keeps the editor's
version and then the file's, in the order shown on screen.

## Details

- Saving through a symlink writes the file it points to rather than replacing
  the link.
- `:w other.md` won't overwrite an existing file without `!`.
- Text piped in (`git show HEAD:notes.md | shoin`) opens with no filename
  until `:w name.md` gives it one. See [Reading](reading.md#from-a-pipe).
- A buffer with no filename yet is skipped by autosave, and the autosave clock
  restarts on every write.
- A `touch` that changes no bytes is ignored, a file still being written is
  left for the next check, and a file that was deleted leaves the buffer alone
  — `:w` recreates it.
- `:set autoreload off` stops taking outside changes automatically; `:revert`
  still does it on request.
- `trim_on_save` strips trailing whitespace, and `final_newline`
  (`preserve`, `always`, `never`) decides the file's last newline.
- A recovery file is written on a crash, `SIGTERM` or `SIGHUP` only. A
  normal quit, `:q!` included, writes none. A buffer with no filename, or one
  whose folder refuses the write, goes to
  `$XDG_STATE_HOME/shoin/recover-<pid>-<n>.md` (`~/.local/state/shoin/` when
  `XDG_STATE_HOME` is unset). Recovery files are readable by you only.
- Only a newline ends a line. A form feed, a lone `\r` or a Unicode line
  separator (U+2028) stays inside its line and is saved as it was. A form
  feed, a lone `\r` and other control characters show as nothing on screen.
