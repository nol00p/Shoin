# Changelog

## Unreleased

**Editing**

- `:42` goes to line 42 and `:$` to the last line. Like `42G`, it opens a
  fold it lands in and puts the line on top in read-only mode (#176).
- `G`, `gg`, `42G`, `:42`, `H`, `M` and `L` keep the cursor's column, as
  `j` and `k` do, and so do `dd` and `>>` / `<<`: Neovim's default. They
  went to column 0 or to the first non-blank. Undo and redo put the cursor
  back where Neovim does, a column remembered on a tab is the tab's last
  cell, and the cursor never lands on a combining accent (#176).
- Line addresses and ranges on the `:` line, as Neovim: `:+3`, `:.-2`,
  `:$-1`, `:3,7`, `%`, and `'<,'>`, which `:` types for you from Visual.
  They work with `:d`, `:y`, `:>`, `:<`, `:m`, `:t` / `:co`, `:fixlist`
  and `:w path` (only those lines); without a range each works on the
  cursor's line (#182).
- A linewise yank or case change leaves the cursor where its range
  starts, as Neovim: `yk`, `ygg` and `Vky` keep the column, `guu` and
  `gUU` land on the first non-blank, `Vgu` and `VU` at the line's start.
  Undo and redo of a case change come back to the same place, and `2gUU`
  on the last line does nothing (#180).
- `gu` and `gU` work in Visual mode (#180).
- `/` and `?` search with Vim patterns, as Neovim: `\<word\>`, `\(a\|b\)`,
  `\{n,m}`, `\v`, `\c`, `\zs`/`\ze`, `\n` across lines (#185).
  Back-references inside a pattern and look-arounds give an error.
- `:s/pattern/replacement/flags` with ranges (`:%s`, `:'<,'>s`), the
  flags `g c i I n e &`, `&`, `\1`, `~`, `\u \U \l \L`, `\r` in the
  replacement, and the repeats `:s`, `:&&`, `&` and `g&`, as Neovim. One
  `u` undoes it (#185).
- `g~` toggles case over a motion or object, `g~~` over lines, and in
  Visual as `~`; `.` repeats it (#186).
- `2dd`, `2yy`, `2cc`, `2>>`, `2<<`, `2guu`, `2g~~` and `2Y` on the last
  line change nothing, as Neovim (#186).
- `f`, `t`, `F`, `T` find a tab with `Tab`. `r` then `Tab` replaces with a
  tab (spaces to the next stop with `expand_tab`), `r` then `Enter` splits
  the line. `r` counts a letter and its combining accents as one
  character (#186).
- After `$`, `j`, `k`, `G`, `gg`, `:N`, the paging keys and scrolling stay
  at the end of each line, as Neovim (#187).
- In Visual mode `$`, `l`, and `j`/`k` onto a shorter line reach the line
  break, as Neovim, so `v$d` and `v$p` take it too, and a selected line
  break is drawn (#187).
- `d$`, `y$` and `c$` on a line's last character act on it (#187).
- Insert `Tab` with `expand_tab` types spaces to the next tab stop, and
  `Backspace` in a line's indent goes back to the stop before, as
  Neovim's default `smarttab` (#187).
- `>>`, `<<`, `>` over a motion or in Visual, `:>`, `:<` and the list
  shifts rewrite a line's whole indent, as Neovim: all spaces with
  `expand_tab`, else tabs then spaces, so `>>` on a tab-indented line
  gives 8 spaces, not 4 spaces before the tab. List items shifted
  together keep their nesting. Whitespace-only lines shift too; only
  empty lines are skipped (#192).
- A file without a final newline keeps its lines as Neovim does: a new
  empty last line, however it was made (`o`, `A` `Enter`, `cc`, `p`,
  `:t $`), stays a line, also after `:w`, and a save writes what Neovim
  writes with and without `fixeol`. Typing into an empty file gives its
  line a newline (#193).
- The indent `Enter` or `cc` lays down is kept once anything is typed and
  goes again on `Esc`, `Ctrl-c`, another `Enter` or a move to another
  line, as Neovim's `autoindent`. Insert `Ctrl-c` steps back as `Esc`
  does, and Insert `Right` and `Delete` stop at the end of the last line
  (#193).
- `c$`, `C`, `cl`, `ch`, `c0`, `c^` and `ciw` on an empty line, or where
  `h`, `0`, `^` cannot move, enter Insert, as Neovim; `2C` and `2D` act
  as `c2$` and `d2$` (#193).
- Insert `End` keeps to the end of each line `Up` and `Down` reach, as
  `$` does (#193).
- An edit that changes nothing (`gUU` on capitals, `~` on a digit, `x` on
  an empty line) is an undo step that leaves the file unmodified, as
  Neovim; `u` and `Ctrl-r` bring the cursor back to it. `>>` on an empty
  line marks the file modified (#193).

**Math**

- `\mathbb`, `\mathcal`, `\mathscr` and `\mathfrak` draw Unicode letters
  (ℝ, 𝒪, 𝔤) in inline and display math. An argument with no Unicode form
  stays as typed (#179).
- HTML export shows math as MathML, drawn by the browser with no script,
  when `pandoc` is installed. Without it, formulas stay TeX and the export
  says `math left as TeX: no pandoc`; a formula pandoc can't read keeps
  its TeX on its own (#173).
- Display math draws an unknown command with its braced arguments as
  written: `\mathcal{F}` shows `\mathcal{F}`, not `\mathcalF` (#172).

**Display**

- A tab reaches the next tab stop, every `tab_width` cells from the start
  of the line, as Neovim's `tabstop`, instead of being a fixed
  `tab_width` cells wide. The cursor, `j` / `k`, clicks and wrapping count
  it the same way; list nesting is unchanged (#181).
- The cursor on the first character of a wrapped line's second row is
  drawn on that row, not at the end of the row above (#181).
- An embed no longer drops letters or shows `**` and `[` where bold text
  or a link wraps across rows, and `:embed full` uses the whole measure
  (#181).
- In Normal mode the cursor on a tab is drawn on its last cell, as
  Neovim; Insert keeps it on the first (#187).
- Image embeds in `:embed full` use the whole measure when
  `transclude.border` is on (#184).
- A search match or `:s///c` match that takes a line break (`/b\n`) is
  drawn with one highlighted cell after the line's text, also on the
  last line of a file without a final newline (#192).
- Mermaid fences preview as text diagrams, drawn by a local Merman CLI
  you name with `mermaid_renderer` under `[markdown]`, in user config or
  `--config`: a config found in the working directory cannot set it and
  says so. Moving the cursor into a fence, a Visual selection, a search
  or an `:s` match shows its whole source, as does concealment off. A
  diagram that fails, times out or does not fit keeps its source, with a
  status row above it saying why. Previews work in `-R`, `--render` and
  expanded embeds, and `mermaid_graphics = true` draws PNGs on Kitty and
  iTerm2 terminals (#188).
- At most 32 diagrams render at once, shared between split panes; the
  others show their source. A diagram that timed out waits for an edit
  or `:reload`; a missing renderer is tried again every few seconds, so
  installing it needs no restart (#188).
- HTML and PDF export draw Mermaid diagrams as PNGs when
  `mermaid_renderer` is set. A diagram that fails keeps its source, and
  the message gives how many and the first line. Markdown and text
  export keep the source (#188).
- The view keeps the same text on top when the rows around it change
  height or re-wrap: a diagram arriving or going, a resize, a split, a
  new measure (#188).

**Reading**

- Read-only mode (`-R`, `:view`) scrolls through a display math block, an
  embed or a wrapped line taller than the window, and on to the lines
  below it. `j` `k` move a screen row, as in `less`, and `Ctrl-f` `Ctrl-b`
  a page (#174).
- In read-only mode, a search that lands inside a Mermaid diagram shows
  its source with the match highlighted, until the view moves or `Esc`
  (#188).

**Docs**

- A Math page: inline and display math, editing a block, when a block is
  not laid out, export, and the supported commands, with a recording
  (#170).
- A Mermaid page: setting up Merman CLI (tested with 0.11.0), text and
  PNG previews, export and the limits, with demo notes in
  `examples/mermaid/` for seven diagram families and three stress cases
  (#188).

**Fixes**

- Saving a buffer emptied by deletion writes an empty file, as Neovim
  (#184).
- `:w` and `:saveas` to a hard link or symlink of the file being edited
  treat it as that file (#185).
- On 16- and 256-color terminals, selections, `:s///c` matches and
  search matches draw their own text color, as Neovim, so bold, links
  and code under them stay readable, and so does text on an
  `==highlight==` in the highlight's own color. Fallbacks never change a
  palette color you set; truecolor is unchanged (#191).
- A carriage return at a line's end in a file without CRLF endings
  (`abc\r` with no final newline, one CRLF line among LF ones) is part
  of the line, as Neovim: `A` types after it, and `:diff` keeps it
  (#193).
- A recovery file has the file's own end: no final newline when the
  file had none (#193).
- `:revert!` marks the buffer clean when its text already matched the
  file, when only the final newline or line ending changed on disk
  (#193).
- A closing fence indented four or more columns past its list item's
  text, or past the margin, is part of the code, as in CommonMark (#188).
- `shoin --render` no longer prints the hint bar under the page (#188).
- `--export` prints config warnings on stderr (#188).
- `:export html` and `:export pdf` run in the background, so you can keep
  editing. Before writing, an export checks again that its destination
  has not changed since it started and has not become, by a rename, the
  document or a note it embeds (#188).
- `--out` writes every format, PDF too, to a device, a named pipe or a
  descriptor: `--out /dev/stdout | lpr`, `/dev/null`, `>(cmd)`. A named
  pipe nothing reads fails at once instead of hanging, and a directory,
  socket or block device is refused (#188).

## 0.2.5

**Math**

- Display math: a closed `$$` block is laid out over several rows,
  centred: fractions, sums and limits, roots, matrices, `cases` and
  `aligned`. Put the cursor on any of its lines, or select across it, to
  see and edit its TeX. A block too wide for the window keeps the
  one-line Unicode form. `shoin --render` shows the layout; export is
  unchanged (#77, #166, #167).
- Roots in display math are drawn with a slanted stroke and a bar over
  the argument, at any height. Scripts use Unicode letters wherever every
  character has one, inline and in display math: `e^x` shows `eˣ`, `A^T`
  `Aᵀ`, `x_\beta` `xᵦ`. `x_y` no longer shows a Greek gamma (#169).

**Editing**

- Emphasis, `~~…~~` and `==…==` inside a link's text are styled and
  concealed, and `gi` / `gb` strip them there (#122).
- `gf` follows a link inside emphasis (`**see [docs](u)**`) instead of
  doing nothing (#122).
- A link inside emphasis (`**see [docs](u)**`) is drawn as a link, in the
  outer style, its address hidden away from the cursor (#126).
- Code spans, escapes, tags and math inside a link's text are styled and
  concealed as elsewhere, in the link's colour; so is emphasis in a link in
  emphasis. `gi` / `gb` act on the innermost emphasis (#129).
- `.` repeats the last change in whatever document is current. It used
  to repeat a buffer switch (`:bn`, `gf`, `Ctrl-^`, `:e`) made after the
  change (#136).
- Window and buffer keys end a selection, as in vim, and are never the
  change `.` repeats, even when they go nowhere (`Ctrl-w o`, a refused
  `Ctrl-w q`). Opening the finder or the file tree ends a selection too
  (#136).
- `.` after an operator on a selection made with the mouse repeats over
  as much text from the cursor, as vim does. `3.` and `.` in Visual no
  longer leave their own keys in the change (#136).
- A selection never covers the file's final newline: a drag that starts
  or ends on the empty line after it stops at the last character, so
  `gb`, `gl`, `d` and `y` act on the text only (#139).
- A click into another pane while in Insert mode ends the insert there,
  as `Esc` would, and starts a new one in the pane clicked into: `u` in
  each pane takes back that pane's typing only, and `.` repeats the last
  insert (#139).
- A tab is drawn as `tab_width` blank cells wherever it is in a line,
  code blocks included; it took no room. Wrapping, the cursor, clicks,
  selection, indent guides and the embed preview count it the same way,
  so a tab-indented list shows as deep as it nests (#140).
- `j` and `k` keep the screen column across tabs and wide characters, as
  in vim (#140).
- `zt`, `zz` and `zb` scroll the cursor's line to the top, middle or
  bottom of the pane (#148).
- A compact quote such as `>>- a` or `>>text` draws like a spaced one.
  With `layout.stable_gutter` on, its text stays in place as the cursor
  reveals the markers, so a click on a wrapped row lands on the character
  clicked. A click on a bar goes to its own `>` (#149).
- `"` then `Esc` drops a pending count or operator, as `Esc` does after
  `f` or `r`: `3"<Esc>j` moves one line and `d"<Esc>j` deletes nothing
  (#158).
- With `layout.stable_gutter`, only block markers (heading `#`s, quote
  bars, list markers, task boxes, indent guides) shift a revealed line. A
  line that starts with math no longer jumps left out of the text column,
  or to the pane's edge. Leading emphasis, code spans and links don't
  shift either, so `**Note:**` reveals at the text column (#163).

**Lists**

- A tab in a list's indent counts as `tab_width` columns in the
  classifier too, so the indent guides, bullets, `al` / `il`, `>al`,
  `Alt-j` and `:fixlist` agree on which item a line or code block is in.
  A code block 2 columns in under `\t- a` was shown in the item and moved
  as outside it. `\t\t- x` gets its indent guides. Changing `tab_width`
  reclassifies open documents at once (#135).
- `:set tab_width` (`:ts`) sets it from 1 to 16 and reclassifies at
  once; `:set tab_width` alone shows it (#135). A value outside 1–16 in
  the config file is reported on startup (#140).
- A wrapped line of a nested list item hung short of its text when indent
  guides were drawn (#140).
- List commands work on a list inside a blockquote (`> - a`): `>al`,
  `<al` and the other shifts, `Alt-j` / `Alt-k`, renumbering, `:fixlist`,
  the `al` `il` `aL` `iL` objects, Tab in Insert, and Enter, which
  continues `> - a` with `> - `. The `>` markers stay as written, and a
  list ends where its quote does (#144).
- `:fixlist` in Visual re-indents every list the selection touches, as
  one undo step (#144).
- `iL` selects the list item under the cursor through the last item at
  its level (#144).
- A nested list inside a quote draws as it does outside one, after the
  bars: the same bullet per depth, indent guides and hanging indent. A
  tab after `>` counts as the marker's space plus `tab_width` − 1 columns,
  as the list commands count it (#147).
- A wrapped list item indented deeper than the measure allows keeps its
  continuation rows on screen, in a quote or out of one (#149).
- A list item with a tab after its marker wraps under its text, not under
  the tab (#149).

**Folding**

- `za` `zc` `zo` `zM` `zR` fold a list item with its sub-items, or a
  heading's section, to its first line, which ends with ` … N lines`.
  `j` and `k` step over a fold; a search or jump into one opens it; `dd`
  takes it whole. Folds last for the session and never change the file
  (#143).
- Folds are per pane: two panes on one file fold apart, and a new pane
  starts with the folds of the one it was split from (#148).
- On a folded line, `o` and `O` open a line below or above the whole fold,
  and `J` keeps the fold closed, as in vim; Visual `J` joins a closed fold
  whole. Visual `zc` and `zo` fold and unfold every fold in the selected
  lines (#148).
- `Ctrl-d`, `Ctrl-u`, `Ctrl-f` and `Ctrl-b` count a fold as one line. A
  `}` or `{` that lands inside a fold leaves the cursor on the folded
  line, and `}` on a folded line starts after it (#148).

**Pictures**

- A picture cut off by the top or bottom of the screen is drawn in part
  in kitty and Ghostty, sending the file once and only the crop after.
  In iTerm2 and WezTerm its box shows an outline and its caption until
  it is all on screen (#142).
- `![[pic.png|A red barn]]`: the text after `|` is the picture's alt
  text, shown in its caption in place of the file name and written as
  the alt in an export. A size (`|200`, `|200x100`) is still ignored
  (#146).
- A missing picture, embedded or linked, says `no picture called
  "x.png"`, not `no note called` (#158).

**Files**

- `:w` on a file that is a link to a missing file creates that file
  through the link and keeps the link, as vim does. A loop of links, or a
  link into a missing folder, is an error (#123).
- `:w` on a file that was missing when you opened it, and has been created
  since, is refused; `:w!` overwrites it, as vim warns (#123).
- `:w other.md` onto a broken link says it is a broken link; `:w!` creates
  the file the link names and keeps the link (#123).

**Saving and recovery**

- Opening a file names any `.shoin-recover.partial-*` beside it that a
  killed dump left, as cut off part way. It is never deleted, even by a
  later process with the same pid; only a duplicate of a finished recovery
  file is removed (#123).
- A file name too long for the recovery endings gets a shortened recovery
  name beside the file; recovery files 0.2.4 wrote under the full name are
  still found (#123).
- With autosave on, a file someone created after you opened it missing is
  named once, not every interval; `:w` still says it each time (#127).
- A cut-off `.partial` in the state folder (an unnamed buffer, or a folder
  that refused the write) is named, with its folder, once per session. The
  recovery notice leads with its words and names files without their
  folder, writes `$HOME` as `~`, and waits while another message is up. A
  `.partial` dated more than three minutes ahead counts as cut off (#128).

**Export**

- `> > a` then `> b` (or an unmarked `b`) stays one paragraph inside the
  inner quote, as CommonMark reads it (#121).
- A fenced block inside a quote exports as code, nested quotes, `~~~` and
  a fence the quote leaves open included; a tab after `>` counts as the
  marker's space, as in CommonMark (#125).
- HTML and plain-text export count a tab in a list's indent as 4
  columns, as CommonMark does; it was 1 (#135).
- Nested list items inside a quote keep their indent in plain-text
  export and piped `shoin -p`; they were flush with their parent (#150).

**Command line**

- `Tab` and `Shift-Tab` on the `:` line complete a command's name, a path
  after `:e` `:w` `:saveas` `:wq`, an option after `:set` and a topic
  after `:help`, cycling through the matches as vim does; the hint bar
  lists them (#133).
- `Tab` also completes values: a `:set` option's values after `=` or a
  blank, the arguments of `:focus`, `:align`, `:embed`, `:number` and
  `:export` (with `justify`), and open buffers after `:b` (#138).
- `:b <name>` prefers a buffer whose whole name matches (#138).
- `:focus`, `:set focus` and `layout.focus` take their value in any case
  (`Paragraph`, `SENTENCE`), as the other options do (#158).

**Help**

- The hint bar after `<leader>` lists the most used keys first, so at
  80–100 columns it keeps `w save` and `z zen` and drops the rarely used
  toggles instead (#132).
- In read-only view the hint bar reads `i edit` and `<leader> v toggle`,
  not `edit` twice (#132).
- After the leader, `g`, `Ctrl-w` or an operator, the hint bar shows over
  the status line, so the Visual leader hints (`b bold`, `i italic`) show
  right after `v` (#138).
- `:help` lists every `:set` option; it listed 7 of 16 (#146).
- `shoin --help` says when `i` leaves read-only: as the first key or
  right after a refusal; `:view` does at any time (#158).

## 0.2.4

**Math**

- LaTeX math is styled as you write it. Inline `$…$` and display `$$…$$`
  conceal to Unicode symbols away from the cursor: Greek letters,
  operators, fractions, roots, sub- and superscripts. `$` follows pandoc's
  rules, so `$5 and $6` stays plain, and an unclosed `$$` ends at the next
  blank line. Math is on by default; `math = false` under `[markdown]`
  turns it off (#75).
- Long math lines wrap instead of running off the pane. Operator names
  such as `\sin` and `\log` print without the backslash and keep their
  space (`\sin\theta` → `sin θ`); a command is a script's or accent's whole
  argument (`e^\pi`, `a_\text{max}`, `\hat\theta`); escaped specials
  (`\$`, `\%`, `\{`, …) are the character itself (#80).
- HTML export writes math as pandoc does, in `math inline` and
  `math display` elements (#75).

**Lists**

- `:fixlist` re-indents the list under the cursor so that GitHub, pandoc
  and other tools read it as Shoin shows it. After a renumber across `9.`
  to `10.`, the lines under an item can fall out of it elsewhere;
  `:fixlist` moves them to the item's text column (#88).
- A fence, `$$` block, heading or rule inside a list item counts its indent
  from the item's text, so code under a `10.` item shows as code. HTML
  export keeps these blocks, and the text after them, in their item
  instead of splitting the list (#90).
- In Visual mode, `Alt-j` / `Alt-k` move the list items the selection
  touches, with their children, past the next or previous sibling (#82).
- List commands stay fast on long lists: `Alt-j` among 1,000 sub-items
  went from about 0.9 s to under 10 ms (#89).

**Editing**

- In Visual mode `p`, `P`, `~`, `r`, `J`, `D`, `X`, `Y`, `C`, `S`, `R`,
  `I` and `A` act on the selection and end it, as in vim, instead of
  editing at the cursor (#103).
- Counts work on `i`, `a`, `o` and `.`: `3ix<Esc>` types `xxx`, and `3.`
  repeats the last change with a count of 3. `d2aw` and `d2iw` take that
  many words (#108).
- `cc` keeps the line's indent with `auto_indent`. `yy` and `yj` leave the
  cursor where it was (#108).
- `F` and `T` are exclusive, as in vim, so `dFx` no longer deletes the
  character under the cursor. `;` and `,` are motions, so `d;` and `c,`
  work (#105).
- The `:` and `/` lines, prompts and the finder read control keys: `Esc`
  and `Ctrl-C` cancel, `Ctrl-U` clears, `Ctrl-W` deletes a word. Other
  chords are ignored instead of typing their letter (#104).
- `_italic_` and `__bold__` are styled, except inside a word
  (`snake_case_name`). A run of `*` with a space after it stays plain, so
  `2 * 3 * 4` is not italic (#97). `gi` / `gb` strip underscore emphasis
  too, and italic on bold adds italic instead of replacing bold (#119).
- Links may hold brackets in their text (`[![badge](img)](url)`,
  `[a [b] c](url)`), parentheses in their address, a `"title"` and
  `<…>` addresses (#91, #117).
- Search is faster on large documents (#119).

**Files**

- `:w other.md` on a buffer that has its own file writes a copy and keeps
  editing the original, as in vim. `:saveas` (`:sav`) moves the buffer to
  the new file (#107).
- `~` at the start of a path typed in `:e`, `:w`, `:saveas`, `:export` and
  the tree's prompts is your home folder (#107).
- One file is one buffer: opening it by a relative path, an absolute path
  or a symlink no longer gives two buffers, and a tree rename follows it
  (#99).
- `[[2024.01.15 meeting]]` and `[[Dr.Who]]` resolve and `gf` creates them
  as notes; only a real file type counts as an extension. `![[diagram.png]]`
  no longer embeds `diagram.md`, `[[archive/todo]]` no longer opens
  `sub/todo.md`, and `gf` on a missing `[[report.pdf]]` says it is missing
  instead of creating `report.pdf.md` (#93, #99, #102).
- The tree can rename a file to another case of its name on macOS and
  other case-insensitive filesystems (#109).

**Saving and recovery**

- If Shoin crashes or is killed by `SIGTERM` or `SIGHUP`, each unsaved
  buffer is written to `<file>.shoin-recover` beside its file (or under
  `$XDG_STATE_HOME/shoin` for an unnamed buffer), and the paths are
  printed once the terminal is back. A later crash writes `-2`, `-3`, …
  instead of overwriting the first. Opening a file with a recovery file
  beside it says so; nothing is applied for you (#100, #118).
- A save writes over the file in place, after a synced backup, when the
  folder is not writable, the file has other hard links, or it belongs to
  another user. Long file names save, and the folder is synced after a
  rename so the save survives a power cut (#110).
- A save no longer drops blank lines at the end of a file without a final
  newline. `trim_on_save` leaves fenced code and hard breaks (two spaces at
  the end of a line) alone, and the buffer takes the same change as the
  file, so `:diff` finds nothing afterwards (#112).
- A broken symlink counts as taken: `:w`, `:saveas`, the tree's create and
  rename, and `gf` no longer replace it or write through it (#114).

**Export**

- `[markdown] export_follows_settings = true` makes `txt` and `html`
  export follow the editor's `[markdown]` settings, so syntax switched off
  in the editor exports as plain text. Off by default: export writes math,
  tables and code as such whatever the editor shows (#81).
- Bold, italic and links nested inside other markup are exported, not
  escaped as text (#98).
- `--export notes.md` and `:export` to Markdown write `notes-export.md` by
  default instead of failing on the source itself. Export refuses to write
  over the source, an embedded note or a picture it reads (#98, #115).
- Pictures keep working when the export goes to another folder, and PDF
  export works from any folder (#96, #98).
- PDF export through Typst works again with Typst 0.15, and the hint for a
  missing engine points Linux users to typst's releases (#76, #79).
- HTML export: repeated headings get distinct ids, consecutive `>` lines
  make one paragraph, hard breaks become `<br>`, links and pictures keep
  their title, and `[[javascript:…]]` exports as text (#92, #98, #115).
- Embedded notes with CRLF line endings lose their front matter, and a `#`
  line inside fenced code no longer ends a heading embed (#93, #96).
- Plain-text export no longer swallows the first word after a tab-separated
  list marker (#92).

**Speed**

- Typing stays fast in 100,000-line documents: about 0.6 ms a keystroke,
  down from 32–40 ms (#113).
- A very long line (hundreds of KB) draws and searches in one pass instead
  of slowing every frame (#111).

**Fixes**

- Lines break on `\n` only. A U+2028 or U+2029 inside a line used to start
  a new line for some commands and not others; it stays in its line, saves
  back as it was and is drawn as a dim `↵` (#95, #116).
- A read-only buffer reloads when its file changes on disk (#95).
- A file that changes into something Shoin cannot load is reported once,
  not every second (#106).
- `w` in the `:diff` view in read-only mode no longer writes the unmerged
  buffer over the file; the view stays open and names `W` (#99, #119).
- Kitty and Ghostty draw only PNG pictures; a JPEG or GIF there shows its
  box and caption instead of bytes the terminal cannot decode (#100).
- Counts are capped instead of overflowing, and a counted paste over 16 MB
  is refused (#99).
- A mouse drag in an empty pane no longer crashes (#94).
- Visual selections belong to their pane: closing, splitting or leaving
  the pane ends Visual, and `.` after `x` then `2.` no longer crashes (#85).
- The block cache stays right after edits that keep the line count, such
  as moving a numbered item past `10.` (#84).

## 0.2.3

**Fixes**

- Started with no file, Shoin showed no hint bar at all while the start
  screen was up: not after `<leader>`, `g` or `Ctrl-w`, and not in the file
  tree opened from it. Only the plain Normal-mode hints give way to the start
  screen now, since it lists those itself.

## 0.2.2

**Help**

- Help is reorganised by what you want to do, one tab per category: Basics,
  Navigation, Editing, Writing, View, Files and Config. `Tab` / `Shift-Tab`
  or `1`–`7` switch tabs, and keys, descriptions and notes each take a theme
  colour. The Config tab explains every option of every config file, read
  from the shipped `.conf` files themselves.
- `:help <word>` opens help at a key, command, setting or section
  (`:help gb`, `:help view`, `:help measure`, `:help tree`). Inside help,
  `/` searches every tab and `n` / `N` move between matches.
- A hint bar along the bottom shows the keys that matter right now: in each
  mode, in the file tree, the finder and prompts, and after `<leader>`, `g`
  or `Ctrl-w`, what can follow, read from your own bindings. It gives way
  to the status line, hides in zen mode, and sits on the theme's
  `status_bg`, now Tokyo Night's own statusline colour. `[hints] enabled =
  false` or `:set hints off` turns it off (#60).
- The leader keys work from the file tree too, and `<leader>hl` there opens
  help at the tree's keys (#60).
- Help and the docs write leader keys as `<leader>fe` rather than
  `Space fe`, since the leader can be remapped.
- The keys reference lists every action name a binding can use, with its
  other spellings and default key (#59).

**Editing**

- `Ctrl-e` / `Ctrl-y` scroll the page a line without moving the cursor,
  which only follows when it would leave the screen. They take a count, and
  rebind as `scroll_view_down` / `scroll_view_up`. The mouse wheel now
  scrolls the page the same way, three lines a notch, instead of dragging the
  cursor (#54).
- `move_lines_down` / `move_lines_up` move the lines of a Visual selection
  (or the cursor's line) down or up and keep them selected. They take a
  count and are unbound: bind them to `J` / `K` under `[keys.visual]` (#57).
- `<leader>v` turns read-only mode on and off, as `:view` does. It is the
  `toggle_view` action, so it can be rebound (#58).

**Files and panels**

- `tree.close_on_open = true` closes the file tree once it opens a file,
  instead of leaving it beside the editor (#55).
- `tree.root = "cwd"` opens `<leader>fe` / `<leader>ff` on the folder Shoin
  was launched from instead of the edited file's folder, so a vault of notes
  in subfolders shows whole. The `file_tree_cwd` / `find_file_cwd` actions
  go there whatever the setting (#56).
- `background = "none"` in the theme, now documented, uses the terminal's
  own background, so a transparent terminal shows through.

**Fixes**

- On Linux the config no longer reloads over and over while the editor
  runs. Reading the config counted as a change to it, so each reload set off
  the next, and a config with a warning flashed that warning forever (#53).
- Setting the bindings under a prefix to `"none"` no longer warns when the
  prefix itself is bound: `"<leader>fe" = "none"` frees `<leader>f` for your
  own binding.

## 0.2.1

**Writing**

- Tables line up as you write them: leaving Insert after an edit in a table
  pads every cell to its column's width in the file, redraws the delimiter
  row to match and keeps `:--:` alignment markers. `ga` does the same on
  demand; `editor.table_align = "manual"` leaves it to `ga` alone (#50).

**Fixes**

- Typing in a table's header row no longer turns it into plain text until
  the file is reloaded.
- Ctrl and Alt chords arrive intact in terminals with the kitty keyboard
  protocol (kitty, Ghostty, WezTerm, foot, Alacritty, recent iTerm2):
  `Alt-j`/`Alt-k` no longer split into Esc and a letter over SSH or on a slow
  link, and `Ctrl-h` is no longer read as Backspace. Other terminals behave
  as before, and `Ctrl-[` still leaves Insert everywhere.
- A crash no longer leaves the shell with mouse reporting or bracketed paste
  switched on.
- Indenting a list item follows how the file already nests instead of always
  using `tab_width`: a list nested by 2 stays nested by 2, an item lines up
  with the sub-items it joins, and Shift-Tab / `<al` puts an item on its
  parent's column, so it can no longer skip a level.

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
