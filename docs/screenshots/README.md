# Screenshots

`README.md` embeds nine images from this directory. **None of them exist yet**,
which is why the landing page currently shows broken images — it is the one
outstanding piece of work on the project.

This file is the brief, so whoever takes them does not have to reverse-engineer
it from the README's `<img>` tags.

| File | What it shows |
|---|---|
| `hero.png` | A document open: centered measure, wide margins, one line raw and the rest concealed. Sits directly under the badges, so it is the one that has to be good. |
| `splash.png` | The start screen — the bonsai plus its five hint lines. |
| `conceal.png` | Close-up of the core invariant: the cursor's line showing raw Markdown while its neighbours show it finished. |
| `writer-verbs.png` | Before/after of `gb` / `gt` / `g2` on a line. |
| `panels.png` | The file tree on the left, a vertical split on the right. |
| `focus-mode.png` | `:focus paragraph` — surrounding paragraphs dimmed. |
| `transclusion.png` | A composition file under `:embed full`. |
| `images.png` | `![[photo.png]]` drawn inline in a terminal that supports it, with the fallback box beside it. |
| `theme.png` | A custom `[theme]` against the default. |

## Taking them

- Width **1600px or wider**; the README displays them at 800.
- Use the shipped Tokyo Night theme so they match what a new reader gets, and a
  Nerd Font so the tree and fence glyphs render.
- Run from **outside the repository**, or pass `--config`: config discovery finds
  `./shoin/` before `~/.config/shoin/`, so running here loads the repo's own
  `.conf` files rather than a reader's defaults.
- `examples/` has material worth shooting — `formatting.md` for block and inline
  styling, `transclusion/main.md` for the embed shot, `images/file.md` for
  pictures.
