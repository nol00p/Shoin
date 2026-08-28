<h1 align="center">Shoin</h1>

<p align="center">
  <em>A no-clutter terminal Markdown editor.</em><br>
  Modal editing, Obsidian-style live formatting, an iA&nbsp;Writer surface —
  one centered column of text and nothing else.
</p>

<p align="center">
  <img alt="License: Apache-2.0" src="https://img.shields.io/badge/license-Apache--2.0-blue.svg">
  <img alt="Rust 1.82+" src="https://img.shields.io/badge/rust-1.82%2B-orange.svg">
  <img alt="Tests" src="https://img.shields.io/badge/tests-374%20passing-brightgreen.svg">
</p>

<!-- SCREENSHOT: the editor with a document open — centered measure, wide margins,
     one line raw and the rest concealed. This is the hero shot. -->
<p align="center">
  <img src="docs/screenshots/hero.png" alt="Shoin editing a Markdown document" width="900">
</p>

*Shoin* (書院) is the study alcove of a traditional Japanese house: a writing
desk built into a bay by the window, and nothing else in the room.

---

## Why Shoin

- **No permanent interface.** No menu bar, no always-on file tree, no tab strip,
  no standing status line. Text in a centered measure, wide margins, and that's
  it. Every panel answers to a key that both opens and closes it — chrome
  appears when you summon it and leaves when you're done.
- **Live preview that never lies.** The line your cursor is on shows raw
  Markdown. Every other line shows it finished: markers hidden, styling applied.
  You always edit exactly what's in the file, and you always read what it will
  become.
- **Vim bindings, plus writer verbs.** Normal / Insert / Visual, operators and
  motions, counts, registers, text objects, undo, `.` repeat — and `gb` for
  bold, `gi` for italic, `gt` to tick a task, `g1`–`g6` for headings.
- **Plain TOML config, hot-reloaded.** A `~/.config/shoin/` directory of `*.conf`
  files. Save one and the running editor updates. A broken config never takes
  down the editor.
- **Notes that compose.** `[[note]]` links, `![[note]]` embeds — and
  `![[photo.png]]` embeds a picture, drawn as real pixels in a terminal that
  can. Write atomic ideas as separate files, order the links, and export the
  result as Markdown, plain text, self-contained HTML, or PDF.
- **`.md` and `.txt` only.** Not a code editor — though a fenced block whose info
  string names a language is syntax-highlighted, in seventeen of them.

---

## Install

### From the repository

```sh
cargo install --git https://github.com/nol00p/shoin
```

### From a local clone

```sh
git clone https://github.com/nol00p/shoin
cd shoin
cargo install --path .
```

Both put a `shoin` binary in `~/.cargo/bin` — make sure that's on your `PATH`.
To try it without installing anything, `cargo run -- notes.md` works from a
clone.

Then, optionally, write yourself a config to edit:

```sh
shoin --init-config          # → ~/.config/shoin/*.conf
```

Installing does not do this for you, and neither does first launch. `cargo
install` has no post-install hook, and a build script may only write to
`OUT_DIR` — it also runs in CI, in sandboxes and on cross-compiles, often as
another user. Seeding at startup instead would mean the editor writing to your
home directory without being asked, which is the same rule that keeps it from
autosaving your documents. So it is one command, it refuses to overwrite a
config you already have (`--force` if you mean it), and shoin runs perfectly
well with no config at all.

<!-- Once published to crates.io, `cargo install shoin` becomes the one-liner. -->

### Requirements

| | |
|---|---|
| **Rust** | 1.82 or newer |
| **Terminal** | truecolor (24-bit) support |
| **Font** | a Nerd Font patched font — or set `glyphs.nerd_fonts = false` |
| **PDF export only** | `pandoc` and a layout engine (`brew install pandoc typst`) |

Markdown, text and HTML export need nothing beyond the binary.

---

## Quick start

```sh
shoin notes.md      # open a file (created on first :w if it doesn't exist)
shoin               # a blank page, with a few ways in drawn on it
```

Started with no file, Shoin shows a bonsai and five ways in: the file tree, how
to begin writing, how to name the file, `:help`, and how to leave. It's drawn,
not typed — the buffer underneath is genuinely empty, so the first character you
press is the first character of the file, and the screen is gone that same
frame.

<!-- SCREENSHOT: the splash screen — bonsai plus the five hint lines. -->
<p align="center">
  <img src="docs/screenshots/splash.png" alt="The Shoin splash screen" width="800">
</p>

Then, in order: `i` to type, `Esc` to stop, `:w` to save, `:q` to quit. And
**`:help`** inside the editor explains everything else — `:help bindings`,
`:help commands`, `:help writer`, `:help config`.

---

## The one idea

```
  ▍A finished paragraph with bold text and a link.        ← concealed
  ▍
  ▍This is the line I am **editing** right now.           ← raw, cursor here
  ▍
  ▍ Another finished line with inline code.              ← concealed
```

> The active line is always rendered 1:1 with its source.
> Every other line may conceal its markup.

That restriction is what keeps the editor honest. Live-preview editors normally
need a two-way source↔screen coordinate map that every motion and every click
must translate through — the thing that makes them fragile. Because the cursor
only ever sits on a line that's 1:1, Shoin doesn't need one.

In Visual mode the whole selection renders raw, so you never operate on markers
you can't see. `layout.conceal = false` turns concealment off entirely.

<!-- SCREENSHOT: a close-up of the active-line invariant — cursor line showing
     **raw** markers while the lines above and below are rendered. -->
<p align="center">
  <img src="docs/screenshots/conceal.png" alt="The active line shows raw Markdown" width="800">
</p>

---

## Using it

### Getting around

```
  h j k l / arrows   move             i / a        insert / append
  w b e              by word          o / O        open a line below / above
  0 ^ $              line start/end   x            delete a character
  gg / G             top / bottom     dd yy p      delete / yank / paste
  u / Ctrl-r         undo / redo      .            repeat the last change
  / ? n N            search           :w  :q       save / quit
```

Operators take motions, text objects and counts, the way you'd expect:
`d2w`, `ciw`, `ya(`, `>ip`, `gUiw`, `2d3w`. Registers work too — `"ayy`, `"ap`,
`"0` for the last yank. `jk` leaves Insert mode; Enter continues a list marker
and ends the list when the item is empty.

### Writer verbs

The `g` prefix formats the word under the cursor, or the selection — each as a
single undo step.

```
  gb  **bold**        gt  toggle a task checkbox  - [ ] <-> - [x]
  gi  *italic*        gl  wrap as a [link](url), cursor in the URL slot
  gh  ==highlight==   g1..g6  set the heading level    g0  strip it
  gk  `code`          gp  start the next paragraph
```

<!-- SCREENSHOT: a before/after of gb / gt / g2 applied to a line. -->
<p align="center">
  <img src="docs/screenshots/writer-verbs.png" alt="Writer verbs formatting text" width="800">
</p>

### Panels, when you want them

Everything below opens and closes with the same key. `<leader>` is Space.

| | |
|---|---|
| `<leader>fe` / `fE` | file tree — this file's folder, or all of `$HOME` |
| `-` / `=` (in the tree) | move the root up a level / into the selected directory |
| `H` (in the tree) | show / hide dotfiles |
| `a` `r` `m` `d` (in the tree) | new, rename, move, delete |
| `<leader>ff` / `fF` | fuzzy finder — this file's directory, or all of `$HOME` |
| `<leader>fb` | switch buffer (`:ls`, `:b`, `:bn`, `:bp`, `:bd`) |
| `<leader>sv` / `<leader>ss` | split beside / below |
| `Ctrl-w` + `hjkl` | move between panes (`Ctrl-w q` closes, `=` evens them) |
| `:e <path>` | open another file |
| `:q` / `:Q` | close this buffer (the last one quits) / quit every buffer at once |

<!-- SCREENSHOT: file tree open on the left, a vertical split on the right. -->
<p align="center">
  <img src="docs/screenshots/panels.png" alt="File tree and a vertical split" width="900">
</p>

### Writing modes

```
  :focus [off|paragraph|sentence]   dim everything but what you're writing
  :typewriter                       keep the cursor line centered
  :zen                              hide every chrome affordance at once
  :set measure=72                   the text width, in columns
  :set line_spacing=1               blank rows between lines, 0–4
```

<!-- SCREENSHOT: focus mode — surrounding paragraphs dimmed. -->
<p align="center">
  <img src="docs/screenshots/focus-mode.png" alt="Focus mode dims all but the current paragraph" width="800">
</p>

---

## Compose notes into a text

Write atomic ideas as separate files; compose them by ordering links.

```markdown
# On attention

![[fragments/attention-is-scarce]]
![[fragments/the-cost-of-switching]]
![[fragments/what-i-actually-do]]
```

`[[note]]` is a plain link; `![[note]]` embeds its content.
`![[note#Heading]]` takes one section, `![[note#^blockid]]` one block. It's
Obsidian syntax, so vaults stay portable — and moving a section becomes `dd` `p`.

`:embed [none|short|rec|full]` chooses how much to expand on screen; `full`
shows the finished document with no seams. `:export [md|txt|html|pdf]` writes it
out through a save dialog.

<!-- SCREENSHOT: a composition file with :embed full, showing the assembled text. -->
<p align="center">
  <img src="docs/screenshots/transclusion.png" alt="Transcluded notes expanded in place" width="800">
</p>

### Pictures

`![[photo.png]]` embeds an image the same way `![[note]]` embeds a note — png,
jpg, gif, webp and bmp, resolved by the same rules, so a bare `![[diagram]]`
finds `diagram.png` (and prefers `diagram.md` if both exist, since that's the
one you can edit).

An image embed reserves a box the shape of the picture, captioned with its name
and pixel size. **In a terminal that can draw pixels — kitty, Ghostty, WezTerm,
iTerm2 — the picture itself is drawn there.** Everywhere else the box and the
caption are the picture, which is also what you get inside tmux, where a
half-drawn image is worse than none. `SHOIN_IMAGE_PROTOCOL=kitty|iterm|none`
overrides the detection.

`:export html` carries the image *inside* the page as a `data:` URI, so a
self-contained page really is one file.

<!-- SCREENSHOT: a document with ![[photo.png]] drawn inline in kitty, and the
     same document's placeholder box in a plain terminal, side by side. -->
<p align="center">
  <img src="docs/screenshots/images.png" alt="An image embedded in a document" width="800">
</p>

An embed on the cursor's line shows its raw source, exactly as `**bold**` does —
which is also why embedded text can never be edited by accident: the cursor is
never inside it. A cycle is a hard error naming the loop, and an unresolved link
leaves a visible placeholder rather than a silent hole in your export.

There are worked examples in [`examples/images/`](examples/images/) — one
picture embedded four ways — and in [`examples/transclusion/`](examples/transclusion/),
covering every link form including one that deliberately doesn't resolve.

### Exporting from the shell

```sh
shoin --export notes.md --format html --out notes.html
shoin --export notes.md --format txt --stdout | pandoc …
```

No terminal is acquired, so it composes into pipelines and build scripts.

---

## Configuration

Config lives in **`~/.config/shoin/`** — a conf.d directory whose `*.conf` files
are merged, so you can split settings by category. `shoin --init-config` writes
the annotated defaults there; they also live in this repo's
[`shoin/`](shoin/) directory.

Every key is optional. Edits hot-reload live, and a parse error keeps the
running config rather than dropping you into a broken one. See the whole
resolved config with `shoin --print-config`.

```toml
[layout]
measure    = 72           # text width in columns  (:set measure=N to change live)
typewriter = false        # keep the cursor line centered
focus      = "off"        # "paragraph" or "sentence" dims everything else

[theme]
# The default is Tokyo Night (dark). Override individual colors, or point
# `name` at ~/.config/shoin/themes/<name>.toml.
background = "#1a1b26"
text       = "#c0caf5"

[input]
leader = " "

[tree]
show_hidden = false       # dotfiles in the file tree (H toggles it live)

[keys.normal]
"<leader>w" = "save"
```

A binding may name a whole **verb**, not just a discrete action, so the grammar
itself moves with it: `"s" = "operator_delete"` keeps counts, text objects and
the doubled form (`ss`) all working. Bindings resolve on exact match, so one
that's also the prefix of a longer binding shadows it — and Shoin says so at
load time rather than leaving the longer one silently dead.

Lookup order, first that exists wins: `--config <path>`, `$SHOIN_CONFIG`,
`./shoin.conf`, `./shoin/`, `$XDG_CONFIG_HOME/shoin/`, `~/.config/shoin/`.

<!-- SCREENSHOT: a custom theme side by side with the default. -->
<p align="center">
  <img src="docs/screenshots/theme.png" alt="A custom theme" width="800">
</p>

---

## Command-line options

```
shoin [FILE]

  --config <PATH>     use this config instead of the discovered one
                      (with --init-config, seed THIS path instead)
  --print-config      print the resolved config and exit
  --init-config       write the shipped config to ~/.config/shoin and exit
  --force             let --init-config replace a config already there
  --zen               start with every chrome affordance hidden
  --export <FILE>     export without starting the editor
  --format <FORMAT>   md | txt | html | pdf          (default: md)
  --out <PATH>        where --export writes
  --stdout            print the export instead of writing it (text formats only)
```

---

## License

Licensed under the **Apache License, Version 2.0**. See [LICENSE](LICENSE), or
<https://www.apache.org/licenses/LICENSE-2.0>.

Unless you state otherwise, any contribution you intentionally submit for
inclusion in this work is licensed as above, per section 5 of the License, with
no additional terms.

Every dependency is permissively licensed — MIT, Apache-2.0, ISC, Zlib,
Unlicense or CC0 — so nothing in the tree conflicts with distributing Shoin
under Apache-2.0.
