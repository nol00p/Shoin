# Files

Shoin saves atomically where it can, never autosaves unless you ask, and
never quietly overwrites changes made by something else. If it crashes or is killed with
unsaved work, that work goes to a recovery file. When a file changes on disk while
you have unsaved work, `:diff` lets you merge the two.

## Saving

`:w` writes atomically: the text goes to a temporary file beside the target, is
fsynced, and is renamed over it, so a crash mid-save leaves the original
intact. If the file changed on disk since Shoin read it, the write is refused —
`:w!` is how you say you meant it.

A few files are written in place instead, because a rename would harm them or
cannot reach them: a file with several hard links, someone else's file, and a
file in a folder you cannot write to. Where the folder allows it, a backup of
the original sits beside the file until the new text is safely written.

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
- A file opens in one buffer however it is named: `n.md`, `./n.md`,
  `sub/../n.md`, the tree's path and a symlink to it all switch to the same
  buffer. Renaming or moving it in the tree takes the buffer along.
- `:w other.md` writes a copy and keeps you on this file, as in Vim: the next
  `:w` still saves here. `:saveas other.md` moves the buffer to the new file,
  and its styling follows the new extension. Neither overwrites an existing
  file without `!`.
- A typed path starting with `~/` is in your home folder, in `:e`, `:w`,
  `:saveas`, `:export` and the tree's create and move prompts.
- `:wq other.md` saves the buffer as `other.md` and quits, like `:saveas`.
- Text piped in (`git show HEAD:notes.md | shoin`) opens with no filename
  until `:w name.md` gives it one. See [Reading](reading.md#from-a-pipe).
- A buffer with no filename yet is skipped by autosave, and the autosave clock
  restarts on every write.
- A `touch` that changes no bytes is ignored, a file still being written is
  left for the next check, and a file that was deleted leaves the buffer alone
  — `:w` recreates it.
- `:set autoreload off` stops taking outside changes automatically; `:revert`
  still does it on request.
- In read-only mode, outside changes and `:revert!` still come in, and
  `:diff` still shows the two versions, but its `w` writes nothing. The
  view stays open with your choices set; `W` leaves read-only mode and
  writes them.
- Written in place, every hard link sees the new text, and someone else's
  file keeps its owner. A file you cannot write is still replaced when its
  folder allows it. If an in-place write fails, the error names the backup
  (`.<name>.<n>.swbak`) that holds the original; in a folder you cannot
  write to there is no backup, and a crash mid-write can leave the file
  half written.
- `trim_on_save` strips trailing whitespace, and `final_newline`
  (`preserve`, `always`, `never`) decides the file's last newline.
- In a Markdown file (`.md`, `.markdown`, judged by the name being saved
  to), `trim_on_save` leaves lines inside a fenced code block alone, and a
  hard line break (two or more spaces after text, with text on the next
  line) keeps two spaces.
- An autosave in Insert mode leaves the line you are typing alone when the
  cursor is in or after its trailing whitespace.
- A save in read-only mode writes the text as it stands, untrimmed.
- Without a final newline, a save drops only the last one: blank lines at
  the end of the text stay.
- When a save trims a line or changes the last newline, the buffer changes
  the same way, as one step: `u` brings the spaces back. After `u`, while
  there is something to redo, the save changes only the file, so the redo
  is kept and the buffer stays marked modified.
- A recovery file is written on a crash, `SIGTERM` or `SIGHUP` only. A
  normal quit, `:q!` included, writes none. A buffer with no filename, or one
  whose folder refuses the write, goes to
  `$XDG_STATE_HOME/shoin/recover-<pid>-<n>.md` (`~/.local/state/shoin/` when
  `XDG_STATE_HOME` is unset). Recovery files are readable by you only.
- Only a newline ends a line. A form feed, a lone `\r` or a Unicode line
  separator (U+2028) stays inside its line and is saved as it was. A form
  feed, a lone `\r` and other control characters show as nothing on screen.
