# Ideas / backlog

Features beyond the original `SPEC.md`. The README's numbered build order
implements the SPEC; this file tracks the agreed extension of it.

## Direction (decided 2026-08-13)

Shoin grows into an **nvim-like editor with toggleable panels**. The
reconciling principle with the SPEC's "no interface" ethos is **toggle
symmetry**: every panel is bound to a key that both opens *and* closes it, so
chrome still "appears when you summon it and leaves when you're done" — it is
never permanent, it is *toggled*.

> **No interface.** ... Chrome appears when you summon it and leaves when you're
> done.  — SPEC §1, still the guiding rule; panels honor it by being toggleable.

Concrete bindings (default `<leader>` = space; all rebindable in
`[keys.normal]`):

| Binding | Toggles |
|---|---|
| `<leader>fe` / `fE` | file-system tree on the **left**, at project root / cwd — ✅ shipped |
| `<leader>sv` / `ss` | **split view** — a pane beside / below this one — ✅ shipped |
| `<leader>ff` / `fF` | fuzzy file finder overlay, project root / cwd — ✅ shipped |
| `<leader>fb` | fuzzy buffer switcher overlay — ✅ shipped |
| `:` | spotlight command box (see #1) — ✅ shipped |

This makes #3 (tree), #4 (buffers) and #5 (splits) all **in scope**, not just
their overlay forms. The one hard constraint was #2 (header font size), which
the terminal itself limits — dropped 2026-08-19.

Effort: S (hours) · M (a day or two) · L (several days) · XL (a week+, structural).

---

## 1. Spotlight command box  ·  ✅ SHIPPED (2026-08-13)

A centered floating input box for `:` commands, in the iA-Writer / Spotlight
spirit — rounded border, " command " title, upper-third placement, appears on
`:` and vanishes on `Esc`/`Enter`.

- `render_command_box` in `render/frame.rs` draws it last over a `Clear`ed rect,
  owning no source↔screen mapping; `status_text` no longer echoes the command.
- **Mode cursor shape** wired too: `app.rs` `sync_cursor_shape` pushes a
  `SetCursorStyle` per mode (block / bar / underline from `[cursor]`, bar while
  typing a command), re-emitting only on mode change and restoring the default
  on exit.
- Still TODO when `/` search lands (step 11): route search input through the
  same box.

## 2. Header font size  ·  ❌ DROPPED (2026-08-19)

Making headings physically larger is **not generally possible in a terminal** —
the grid is fixed-size cells. Two escape hatches, both gated on terminal support:

- **DEC double-height/double-width lines** (`ESC # 3` / `# 4` top+bottom halves,
  `ESC # 6` double-width). Supported by xterm and a few others, **not** by most
  modern terminals, and ratatui exposes no API for it (raw writes needed).
- **Kitty text-sizing protocol** — arbitrary scaling, but Kitty-only.

The deeper problem: a double-height heading occupies **two terminal rows for one
logical line**, which breaks the one-cell-per-line coordinate identity that
SPEC §2 and the wrap map rely on. So this is possible only as a capability-gated,
opt-in mode with real plumbing in `render/layout.rs` (the wrap map) and cursor
math. Colored + bold headings (already shipped in step 4) are the robust,
portable stand-in.

- **Recommend:** treat as experimental, terminal-detected, off by default.

**Dropped 2026-08-19.** Not deferred — decided against. Every route out costs
the one-row-per-line identity that SPEC §2 and the wrap map are built on, in
exchange for an effect only some terminals can show at all. Colored and bold
headings carry the level everywhere. `TODO.md` records it under *Deliberately
not done* so it is not rediscovered as a gap.

## 3. File tree  ·  `<leader>fe` / `fE`  ·  ✅ SHIPPED (2026-08-14, file ops 2026-08-18)

A left-hand file-system tree panel, Neo-tree style. `<leader>fe` opens at the
project root (nearest `.git` ancestor, else the file's dir), `<leader>fE` at the
cwd. **Superseded 2026-08-20:** the git-derived root is gone — Shoin edits prose,
and a directory of notes is not a checkout. The tree now always roots at `$HOME`
(`fe` reveals the edited file inside it, `fE` opens plain), `-`/`+` move the root
itself, and the finder takes the file's own directory (`ff`) or `$HOME` (`fF`). Closed → open+focus; open+unfocused → focus; focused → close. Focus moves
into the tree while it's open: `j`/`k` move, `l`/`Enter` expand a dir or open a
file (focus returns to the editor), `h` collapses / jumps to parent, `g`/`G`
top/bottom, `R` refresh, `q`/`Esc` close. Left-click selects a row. Dotfiles are
hidden; dirs sort first.

- `src/tree.rs` (`FileTree`, flattened from an `expanded` set); `frame.rs`
  `render_tree` + a `tree_width`/editor-`ea` offset so the editor renders in the
  remaining width (and `locate_click` / `visible_line_range` account for it);
  `app.rs` holds `tree: Option<FileTree>` and routes input to `on_key_tree` when
  focused. `Action::FileTree { cwd }`.
- **Neo-tree look (2026-08-14):** guide lines (`│ `, `├╴`, `└╴`) and Nerd Font
  file-type icons, colored from the existing theme roles via `tree::IconKind`
  (prose / code / data / media / plain). Each `Entry` carries `last` plus a
  `guides: Vec<bool>` ancestor chain, computed in `push_children` — the
  flattened list has no parent links and rendering only sees the visible window.
  Row assembly is `frame.rs::tree_row`. Icons are `\u{…}` escapes ON PURPOSE:
  the literal PUA characters are invisible in editors and get silently stripped
  by tooling. `[glyphs] folder`/`folder_open` are configurable; the per-type
  table is not. `nerd_fonts = false` falls back to `▸`/`▾` arrows and no file
  icons — the first setting in the codebase to actually honor that flag.
- **NOT the full pane/window tree yet** — this is a single hardcoded left pane.
  When split view (#5) lands, generalize `ea` into a proper pane tree so both
  ride it. Opening a file keeps the old one now that #4 has landed.
- **File operations (2026-08-18):** `a` new (a trailing `/` makes a directory,
  and missing parents are created), `r` rename, `m` move, `d` delete. Each
  opens a prompt in the spotlight box rather than acting at once — `r` and `m`
  come PRE-FILLED with the current name/path, `Ctrl-u` clears, `Esc` abandons.
  `d` is a `y/N` confirmation, and on a directory row it says how many entries
  go with it, because that is the only warning that `d` there is recursive.
  - `src/fs/ops.rs` holds the primitives, and they refuse to clobber: an
    existing destination is a collision to report, never a file to overwrite.
    Deletion is the one operation that removes, and only the path handed to it.
  - `Mode::Prompt(Prompt)` carries the question, the TARGET PATH captured when
    the prompt opened, and what has been typed. Capturing the target up front
    matters: a refresh between asking and answering can move the selection.
  - A rename or move carries any OPEN BUFFER with it, including every buffer
    beneath a moved directory — otherwise the next `:w` would silently recreate
    the old name.
  - The root row is deliberately untouchable by `r`/`m`/`d`: it is the frame of
    reference every other path in the pane is written against. `a` on it is
    fine, and means "in here".
- `<leader>ff` (fuzzy finder overlay) is the lighter-weight sibling — see #3b.

## 3b. Fuzzy file finder  ·  `<leader>ff` / `fF`  ·  ✅ SHIPPED (2026-08-14)

A centered spotlight overlay listing every file under the project root (`ff`) or
the cwd (`fF`), narrowed as you type. Type to filter, `Ctrl-n`/`Ctrl-p` (or the
arrows / Tab) to move, `Ctrl-u` to clear the query, Enter to open, Esc to close.
Matched characters are picked out in the accent color; the header shows
`matches/total`.

- `src/finder.rs` (`Finder`, `score_match`, `walk`); `frame.rs` `render_finder` +
  `result_line`, both on the `spotlight_block`/`spotlight_accent` helpers now
  shared with the command box; `app.rs` holds `finder: Option<Finder>` and routes
  ALL input to `on_key_finder` while it is open. `Action::FindFile { cwd }`.
- Result rows carry the tree's file-type icons (`tree::file_icon` + the shared
  `frame::icon_color`), read from the file NAME rather than the whole relative
  path, and drop away with `nerd_fonts = false` like the tree's do.
- Matching is a subsequence match, tightened by a backward re-scan (so `app`
  prefers `src/app.rs`), then scored: consecutive characters, path-segment and
  word starts, and hits inside the file name all beat a scattered match. Smart
  case — a capital in the query makes it case-sensitive. The walk shares the
  tree's `read_dir_sorted` (dotfiles hidden), skips `target`/`node_modules`, and
  caps at 20k files / 16 levels, which is also what stops a symlink cycle.
- **Not a toggle**, unlike the tree: an open finder is a text field, so its own
  binding arrives as query text. Esc closes it, exactly like the `:` box.
- Opening ADDS a buffer (#4), so nothing is displaced and an unsaved document
  no longer blocks it.

## 4. Multiple buffers  ·  `<leader>fb` switcher  ·  ✅ SHIPPED (2026-08-17)

Hold several open files at once; switch with a fuzzy buffer overlay
(`<leader>fb`) and `:bn`/`:bp`/`:b <name>`. No always-visible tab bar is
required, but an optional toggleable tabline could follow the same pattern later.

- `BufferState` (in `app.rs`) holds what belongs to the FILE — buffer, block
  cache, scroll, render cache, render-dirty line. `App` holds `docs:
  Vec<BufferState>` + `current`, and **derefs to the current one**, which is why
  the ~290 `self.buffer` call sites did not have to change. Switching keeps
  every document's cursor, scroll and parse cache, so returning to one is free.
- Opening (tree, finder, `:e`) now ADDS a buffer instead of replacing, and no
  longer refuses when the current one is modified; opening a file that is
  already open just switches to it. `:q`/`:wq` answer for every document.
- `:ls` · `:b <name|n>` · `:bn` · `:bp` · `:bd[!]` · `:e <path>`, plus the
  `<leader>fb` switcher — the finder overlay with `Kind::Buffers`, listing open
  documents (with the modified dot) instead of walking the disk.
- Still ONE window: the pane tree (#5) is what puts two of these on screen at
  once.

## 5. Pane tree + split view  ·  `<leader>sv`  ·  ✅ SHIPPED (2026-08-17)

A second pane beside (or below) the current one; `<leader>sv` again closes it.
Each pane has its own scroll and its own document, and the centered measure is
computed per pane within its rect.

- `src/render/pane.rs` — `Node` is the tree (`Leaf(Pane)` / `Split { vertical,
  children }`), `Pane { id, doc, scroll }` a view onto `App::docs[doc]`.
  `geometry(area)` divides evenly and hands back a rect per leaf plus the
  divider rects; `neighbor(area, from, dir)` answers `<C-w>h/j/k/l` from the
  geometry, preferring the smallest gap along the move and then the nearest
  near-edge across it (so stepping into a split column lands at its top).
  Splitting inside a split of the same direction EXTENDS it, so three `<C-w>v`s
  give three even columns rather than nested halves.
- `frame::render` no longer has a text area: it asks the tree for rects and
  calls `render_pane` per leaf, each computing its own `Layout` and syncing its
  document's cache at ITS measure. `pane_area` (frame minus sidebar minus the
  status row) is shared with `App` so click routing and `H`/`M`/`L` use the same
  geometry. One status line for the whole window.
- Keys: `<C-w>` + `v` `s` `q`/`c` `o` `w` `h` `j` `k` `l` (vim's own vocabulary,
  and `<C-w>` now passes ANY second key to `Action::Window`), plus the toggling
  `<leader>sv` / `<leader>ss` / `<leader>so` and `:sp` `:vs` `:close` `:only`.
  `<C-w>h` from the leftmost pane focuses the file tree, and any move out of it
  comes back.
- **Sizing (2026-08-17):** `<C-w>> < + -` move an edge, `<C-w>=` restores equal
  shares, and a count multiplies the step (`3<C-w>>`). Every node carries a
  `weight`, so a split shares its space in proportion rather than evenly, and
  the layout still scales with the terminal. A resize rewrites the weights of
  one split's children as their CURRENT cell sizes and then moves the delta
  between two of them, so a press moves the edge by exactly the cells asked for
  — and it floors at 8 columns / 3 rows rather than squeezing a pane away. Steps
  are 4 columns and 2 rows, wider than vim's single cell because a centered text
  measure is worth moving visibly.
- **Per-pane cursor (2026-08-17):** `Pane.cursor` holds where each pane is
  looking. `Buffer::cursor` stays the LIVE one — every editing verb reaches for
  it, so moving it out would touch the whole editor — and the focused pane
  writes its copy back in `sync_after_input`, with `focus_pane_id` restoring the
  target's on the way in. A saved position is CLAMPED when read, never tracked
  through edits: another pane can shorten the document under it, and clamping is
  a line of code where tracking would be a subsystem. Background panes anchor
  their scroll on their own cursor, which is what lets two views of one file sit
  in different places.
- **Still shared**: concealment. The row index lives in the document's render
  cache, so the active line is the focused pane's in every pane showing that
  document. A pane on a non-current document conceals throughout.
- The file tree stays a fixed-width SIDEBAR outside the pane tree: it is chrome,
  not a view onto a document, and folding it in would put a "which kind am I"
  tag on every leaf for one special case.

## 6. Line spacing  ·  ✅ SHIPPED (2026-08-19)

`layout.line_spacing` used to exist as a setting that did nothing, and was
removed in the 2026-08-18 audit rather than left as a lie. Recording the idea
here, with what it would actually cost:

A blank row between paragraphs means **visual rows that belong to no source
line**. Every consumer of the row index assumes `row -> line` is total:
`RenderCache::row`/`locate`/`total_rows`, the `render_pane` draw loop,
`locate_click`, `visible_line_range`, `gutter_shift`. The honest way in is a
`RowSource::Spacer` variant — which is exactly what that enum is for — plus a
`VisualRow::line()` that can answer "none". That is the same surgery v2
transclusion needs, so if it is ever wanted, it should ride along with §14
rather than be paid for twice.

Worth asking first whether it earns its keep: a Markdown source already puts a
blank line between paragraphs, and `line_spacing = 1` would render two.

**Shipped 2026-08-19, and the estimate above was right about the shape and
wrong about the cost.** §14 had already added `RowSource::Embedded`, which
proved the row index can carry rows the rope does not have — so `Spacer` was one
more variant and its arithmetic, not surgery. `VisualRow::line()` did NOT need
to answer "none": a spacer answers with the line it follows, which is what a
click in the gap should select anyway, so every consumer of the row index kept
working untouched. The last paragraph's objection was answered by suppressing
spacing after a blank line (and inside fences, tables and front matter): a
paragraph break then reads one row wider than the gap inside a paragraph, which
is the typographic answer rather than the doubled one.

## 7. Start screen  ·  ✅ SHIPPED (2026-08-18)

`shoin` with no file argument shows a bonsai in the top two thirds, and five
hints below it: the file tree, how to start writing, how to name the file,
`:help`, and `:q`.

- `src/render/splash.rs`. It is **drawing, not buffer content** — nothing is
  ever inserted — and `splash::active(app)` is **recomputed every frame rather
  than stored**, so there is no flag to invalidate and the screen cannot
  survive its own welcome. `render_pane` returns early when it is active, with
  NO caret: there is nothing yet to point at, and a block sitting on the
  artwork would be the only thing in the room.
- The condition is entirely about the DOCUMENT, never about how the editor was
  launched: one doc, unnamed, unmodified, empty; one pane; no overlay; Normal
  mode. So `i` retires it (intent to write), `:vs` retires it (work under way),
  opening a file retires it — and undoing all the way back to a blank page
  brings it back, which is the honest answer since that IS a blank page.
- **Two drawings, not one resampled.** `BIG` is 40x26 and needs about 44x35 to
  sit above the hints; `SMALL` is 29x13 and fits the 80x24 that most readers
  actually have. A shaded-block version was tried and abandoned — dithered art
  does not survive being scaled down, and the canopy, trunk and pot merge into
  one mass, so there are two drawings rather than one resampled.
- **Coloured in two bands**, split at the row each `Art` records as its `pot`:
  needles in `theme.code` (green), earthenware in `theme.bold` (the one warm
  accent). Borrowed from existing roles rather than given `[theme]` keys of
  their own — a start screen is not worth two settings, and this way it follows
  whatever theme is loaded.
- **Degrades in three steps**: the detailed tree gives way to the compact one,
  then the art goes entirely, then the descriptions go and the key column
  remains. Clipping a description mid-word reads as a bug rather than as a
  small screen. The hints are placed below whichever drawing was used, not on
  the two-thirds line, because the detailed tree runs past that line.
- The leader is printed as the reader would press it (`Space f e`), read from
  `input.leader` so a rebound one is not quietly wrong.

---

## Sequencing

Panels (#3, #5) share one **pane/window-tree** foundation; build it once. The
plan, after build-order **step 6 (concealment)** lands:

1. **Spotlight box + mode cursor** (#1) — small, immediate, no dependencies.
2. **Fuzzy finder + buffer switcher overlays** (`<leader>ff` / `<leader>fb`) —
   both done, on one overlay widget (#3b, #4).
3. **Multi-buffer refactor** (#4) — done: `App` → `docs: Vec<BufferState>`.
4. **Pane/window tree** (#5) — done: `render/pane.rs`, and `frame::render`
   draws a pane per leaf.
5. **File tree** (#3) and **split view** (#5) — both shipped. The tree stayed a
   sidebar rather than a pane; see #5 for why.
6. **Header sizing** (#2) — dropped; see above.

Everything in this file is built except #2 (header font size), which was dropped
on 2026-08-19 rather than deferred. #6 (line spacing) shipped the same day, and
it did ride the §14 row-index work exactly as this file predicted. The two SPEC
leftovers — §5.3's fence gutter bar and §6's
scroll hint — closed 2026-08-18, and the audit the same day closed every gap
between what the config promised and what the code did. The SPEC is now fully
implemented, and v2 transclusion (SPEC §14) shipped on 2026-08-19 along with
fenced-code highlighting, line spacing and HTML export — after which this file
became history rather than a plan.

Rationale for order: concealment first (it's the product's defining feature and
the finder/command overlays reuse its overlay-render plumbing); then the cheap
wins; then the multi-buffer + pane-tree structural work that the panels require.
