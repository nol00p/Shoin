# Writing

Writer verbs format text with one key chord, and the writing modes change how
the page looks while you write: focus, typewriter, zen, line numbers and
alignment.

## Writer verbs

The `g` prefix formats the word under the cursor, or the selection in Visual
mode. Each is a single undo step.

```
  gb  **bold**        gt  toggle a task checkbox  - [ ] <-> - [x]
  gi  *italic*        gl  wrap as a [link](url), cursor in the URL slot
  gh  ==highlight==   g1..g6  set the heading level    g0  strip it
  gk  `code`          gp  start the next paragraph

  gf  follow the link under the cursor      gx  open it with the desktop
```

On a plain list item, `gt` adds an empty checkbox. `gl` writes a link and `gf`
walks it — see [Notes](notes.md). In Visual mode, `Space b` and `Space i` also
toggle bold and italic.

## Writing modes

```
  :focus [off|paragraph|sentence]   dim everything but what you're writing
  :typewriter                       keep the cursor line centered
  :zen                              hide every chrome affordance at once
  :number [relative|absolute]       line numbers — bare :number toggles off
                                     and back to whichever mode you last used
  :align [left|center|right|        how each line sits within the column —
         |justified]                bare :align cycles through all four
  :set measure=72                   the text width, in columns
  :set line_spacing=1               blank rows between lines, 0–4
```

Most have a leader key too: `Space F` cycles focus, `Space t` typewriter,
`Space z` zen, `Space n` line numbers and `Space a` alignment. Each also has a
setting under `[layout]` — see [Configuration](configuration.md).

## Details

- Justified alignment stretches every wrapped row to fill the measure except a
  paragraph's last row. Fenced code and table rows are always left-aligned.
- `:align` positions text *within* the column. Where the column itself sits on
  screen is the separate `layout.align` setting (`center` or `left`).
- Relative line numbers read `0` on the cursor's line. A margin too narrow to
  hold the numbers (`align = "left"`, or a narrow terminal) goes without
  rather than crowding the text.
- `shoin --zen notes.md` starts in zen mode.
- To read a page rather than write it, `:view` (or `shoin -R`) conceals every
  line and refuses edits — see [Reading](reading.md#read-only).
