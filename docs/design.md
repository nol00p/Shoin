# Design notes

Why Shoin works the way it does. None of this is needed to use it.

## The cursor's line is always raw

> The active line is always rendered 1:1 with its source.
> Every other line may conceal its markup.

That restriction is what keeps the editor honest. Live-preview editors
normally need a two-way map between source and screen coordinates that every
motion and every click must translate through — the thing that makes them
fragile. Because the cursor only ever sits on a line that is 1:1, Shoin
doesn't need one. The same rule is why a Visual selection renders raw, and why
embedded text can never be edited by accident: the cursor is never inside it.

## No permanent interface

No menu bar, no always-on file tree, no tab strip, no standing status line.
Every panel answers to a key that both opens and closes it, so chrome appears
when you call it and leaves when you're done.

## The config is one command, not automatic

Installing doesn't write a config, and neither does the first launch. `cargo
install` has no post-install hook, and a build script may only write to
`OUT_DIR` — it also runs in CI, in sandboxes and on cross-compiles, often as
another user. Writing a config at startup instead would mean the editor
writing to your home folder without being asked, which is the same rule that
keeps it from autosaving your documents. So it is one command, and it refuses
to overwrite a config you already have.

It is also optional. The shipped key bindings are compiled in and sit under
whatever config you have, so a fresh install works without writing anything.

## No writes you didn't ask for

Autosave is off by default, because a writing tool shouldn't make writes you
didn't ask for. When you turn it on, it keeps the same guard as `:w` and never
overwrites a file something else has changed.

## Never pick between two versions

When a file changes on disk and you have unsaved changes, Shoin does nothing
at all until you choose. An editor must never quietly pick between two
versions of your work. `:diff` exists so that choosing is easy rather than
skipped.

## gf writes the note to disk

In a vault you write the link first and the note second. Creating the note on
disk, rather than in a scratch buffer, means every other note that points at
that name resolves from then on. Only notes are created this way; a dead link
to a PDF is reported, not invented.

## No pictures in tmux

Inside tmux, an image embed shows its box and caption rather than the picture,
because a half-drawn image is worse than none.

## Markdown and text only

Shoin edits `.md` and `.txt` files. It is a writing tool, not a code editor, so
fenced code is highlighted for reading but there's no language tooling.
