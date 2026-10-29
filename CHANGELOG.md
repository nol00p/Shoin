# Changelog

## Unreleased

**Fixes**

- Ctrl and Alt chords arrive intact in terminals with the kitty keyboard
  protocol (kitty, Ghostty, WezTerm, foot, Alacritty, recent iTerm2):
  `Alt-j`/`Alt-k` no longer split into Esc and a letter over SSH or on a slow
  link, and `Ctrl-h` is no longer read as Backspace. Other terminals behave
  as before, and `Ctrl-[` still leaves Insert everywhere.
- A crash no longer leaves the shell with mouse reporting or bracketed paste
  switched on.

## 0.2.0

Shoin now works the moment it's installed, with no config to write first,
and it reads as well as it edits: print a note, page it, pipe one in, or open
it read-only.

**Reading**

- `shoin -p notes.md` (`--render`) prints a file as the editor shows it —
  concealed, styled, in the theme's colours, embeds expanded and pictures
  drawn — and exits, so a note can be read without opening the editor.
  `--pager` sends a page longer than the screen to `$PAGER` (`less -R` by
  default). Piped or with `NO_COLOR` set, it prints plain text.
- Shoin reads stdin. `cat notes.md | shoin` (or `shoin -`) opens the text
  in an unnamed buffer — `:w name.md` saves it, `:q` just quits — and
  `curl -s …/README.md | shoin -p` prints it.
- `shoin -R notes.md` (or `:view`) opens a page read-only: every line
  concealed, no caret, `j`/`k` and `Ctrl-d`/`Ctrl-u` scroll as in `less`, and
  `q` quits. An edit is refused with a message, and `i` then switches to
  editing.
- A new [Reading](docs/reading.md) page covers all three.

**Setup**

- The leader bindings (`Space fe`, `Space ff`, …) work with no config, so
  `shoin --init-config` is no longer a required first step. The shipped
  `keys.conf` is built in and your own `[keys.*]` merge over it: a partial
  config keeps every binding it doesn't mention, and `"<key>" = "none"`
  removes one. If you had deleted a shipped binding from your `keys.conf`,
  it is back; set it to `"none"`.

**Lists**

- List items are text objects: `al` is an item with everything nested under
  it, `il` the item alone, and `aL` every item at its level. `>al` and `<al`
  indent and outdent an item together with its sub-items, `dal` deletes it
  whole. An indent that would break the list (the first item, an outdent at
  the top level, a top-level list 4 columns in) is refused with a message
  instead of applied. (#29)
- Numbered lists stay in sequence when `>al` or `<al` moves an item into or
  out of them: both lists are renumbered, keeping the number each starts at,
  in the same undo step. `[editor] list_renumber = false` turns it off.
- `Alt-j` and `Alt-k` move a list item past the next or previous one, its
  sub-items with it; a count moves it past that many. A numbered list is
  renumbered. The actions are `list_item_down` and `list_item_up`, for
  rebinding where Alt is awkward (tmux with a long `escape-time`, or a macOS
  terminal not sending Option as Meta). (#30)
- In Insert mode, Tab and Shift-Tab on a list item indent and outdent it
  with its sub-items, as `>al` and `<al` do, so Enter then Tab starts a
  sub-item. Anywhere else Tab still types a tab, and `[editor] list_tab =
  false` keeps it a tab on list items too. A `<S-Tab>` binding in
  `keys.conf` now works; the key never reached it before. (#30)

**Fixes**

- In iTerm2 and WezTerm, a picture that moved or was hidden — the cursor
  reaching its `![[…]]` line, a scroll — left pieces of itself on screen.
  Its old cells are now cleared.

## 0.1.9

- Releases now come with prebuilt binaries for macOS and Linux, on x86_64
  and ARM, and a shell installer, attached to each GitHub release. Shoin is
  also published on crates.io as `shoin`, so `cargo install shoin` works
  without a clone or `--git`.
- On a file ending in a newline, `G` now goes to the last line of text instead
  of an empty line past it, as in vim — so `GiX` no longer adds a line, and
  `Go` no longer leaves a blank one. `j`, `{count}G`, `}`, `dd`, `J` and `dap`
  at the end of a file stop at the same line.
- An `![[embed]]` now shows its note as last written. It used to keep what it
  first read until the line itself was edited, so an embed of a note that did
  not exist yet kept saying "no note called …" after `gf` created the note and
  it was saved.

## 0.1.8

- Clipboard support. `"+y` / `"+p` (and `"*`) copy to and paste from the
  system clipboard, through `pbcopy`, `wl-copy`, `xclip` or `xsel`, or an
  OSC 52 escape over SSH. `[editor] clipboard = true` (or `:set clipboard`)
  sends plain `y`, `d` and `p` there too.
- A terminal paste now arrives as one piece of text (bracketed paste) and goes
  in verbatim: it no longer runs as commands in Normal mode, picks up
  auto-indent or list markers, or leaves Insert on a `jk`.
- `dw` on a line's last word no longer takes the line break with it and joins
  the next line on, and `yw` there no longer yanks the newline — vim's `w`
  rule. A `d{count}w` that crosses lines from the start of a line deletes
  whole lines, as in vim.
- More operators and motions now match vim, each checked against it: `cw` on
  a word's last character changes only that character; `db` from a line start
  keeps the line break; `d}` keeps the blank line it stops at; `dl` takes a
  line's last character; `dgg`/`dG` work on the first/last line; `5dG` and
  `3dgg` honour the count typed before the operator; `3p` pastes three times;
  `;` after `t` moves on to the next match; `w` skips leading blanks and
  blank-only lines; `dc` cancels instead of running `c`; `"Ayw` appended to a
  linewise register keeps its lines intact; `d2$` and `d2e` from a line start
  delete whole lines. A linewise paste lands on the first non-blank.
- `:w other.md` no longer overwrites an existing file without `!`, and
  `:wq!` no longer quits when the write fails.
- Saving through a symlink writes the file it points to instead of replacing
  the link, and a reload that only changed line endings is no longer undone
  by the next save.
- `gt` only toggles the checkbox right after the list marker, never a `[x]`
  in link text.
- A clipboard program that hangs is not tried again for the rest of the
  session, and a failed paste after an OSC 52 copy falls back to Shoin's own
  copy.
- `[editor] clipboard_background = true` runs clipboard copies on a
  background thread, for when yanks and deletes feel slow with `clipboard`
  on. A burst of copies runs the clipboard program once, for the newest text.
- `:help` now lists `:set clipboard` and the `[editor]` clipboard settings.

## 0.1.7

- Added a `cycle_align` keybinding action, reaching the same left → center →
  right → justified cycle as a bare `:align` from a single key press. Bound
  by default to `<leader>a`.
- Clarified the `leader` comment in `input.conf`: a modifier or named key
  needs the same `<...>` wrapping a `keys.conf` binding uses (`"<C-x>"`, not
  `"C-x"`), which is easy to miss since a single character needs none.

## 0.1.6

- Added `:align [left|center|right|justified]` (bare form cycles) and a
  matching `layout.text_align` config setting, controlling how each row sits
  *within* the writing column — separate from `layout.align`, which places
  the column itself on screen. Justified stretches every wrapped row to fill
  the measure except a paragraph's last row, which stays ragged; fenced code
  and table rows are always left regardless of the setting.
- `:export [md|txt|html|pdf] [justify]`, and `--justify` on the CLI, carry
  the same justification into `html`/`txt` export — the two formats shoin
  renders itself. `md` and `pdf` refuse the flag, since raw markup and
  pandoc's own template have no layout of their own to stretch.

## 0.1.5

- Fixed the viewport jumping around during ordinary navigation. The scroll
  position was recomputed fresh every frame but never persisted, so it kept
  re-deriving itself from a stale baseline instead of staying put while the
  cursor moved within view.
- Added a line-number gutter: `:number relative` or `:number absolute`, or
  `<leader>n` / a bare `:number` to toggle it off and back to whichever mode
  you last used. Relative numbers read `0` at the cursor's own line; a
  margin too narrow to hold them (`align = "left"`, or a narrow terminal)
  goes without rather than crowding the text.

## 0.1.4

- Dragging with the mouse now selects text, the way `mouse=a` does in Vim: the
  press puts the cursor down, dragging out of it opens a charwise Visual
  selection anchored there, and the selection outlives the release so `y`, `d`
  and the writer verbs act on it. Text panes only — a drag begun in the file
  tree still does nothing. Set `mouse = false` (or `:set mouse off`) to hand
  selection back to the terminal.
- Dragging **while inserting** follows Vim's `-- (insert) VISUAL --`: the
  selection remembers the insert session it came out of, and an operator (`y`,
  `d`, a writer verb) or `Esc` hands typing back rather than dropping you in
  Normal. A second `Esc` leaves Insert as usual. The mode names itself once in
  the status line on the way in, and the caret takes the accent colour for as
  long as the selection is live — no chrome, so zen mode is unaffected.
- `:u` and `:redo` now work. Undo and redo were reachable only as `u` and
  `Ctrl-r`; typing Vim's `:u` fell through to `not a command: u`, which is easy
  to miss in a status line that clears after 1.5s. All of Vim's abbreviations
  land — `:u`, `:un`, `:und`, `:undo`, `:red`, `:redo`. `:r` and `:re` stay
  unclaimed, since `:r` is `:read` in Vim.

## 0.1.3

- Table rows no longer word-wrap — a row wider than the pane stays on one
  line, clipped at the edge, instead of breaking its `|` columns across
  several ragged lines.
- A new `:help shortcuts` page, bound to `<leader>hl`, lists every keyboard
  interaction — leader keys, `:` commands, navigation, and other
  keybindings — grouped by kind, on one page.

## 0.1.2

- The start-screen hint for a config-less first run now says `--init-config`
  is required, not merely offered: without a config, leader bindings like
  `<leader>fe` do nothing.

## 0.1.1

- Initial release. Obsidian-style live concealment with Vim modal editing —
  operators, motions, text objects, counts, registers, dot-repeat, writer
  verbs on the `g` prefix.
- Centered measure with focus and typewriter modes, fenced code highlighting,
  inline images where the terminal supports them, atomic saves with an
  external-modification guard.
- Panels, toggled rather than permanent: file tree, fuzzy finder,
  multi-buffer, pane splits, start screen.
- Transclusion of other notes, link following, and export to Markdown, plain
  text, HTML and PDF.
- Autosave on a timer, auto-reload when something else edits the file, and a
  side-by-side diff view to merge the two versions difference by difference.
