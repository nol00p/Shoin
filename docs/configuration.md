# Configuration

Config is a folder of TOML files, `~/.config/shoin/*.conf`, merged together so
you can split settings by category. Every key is optional, and saving a file
updates the running editor.

```sh
shoin --init-config      # write the annotated defaults to ~/.config/shoin/
shoin --print-config     # print the whole resolved config
```

The annotated defaults also live in this repo's [`shoin/`](../shoin/) folder,
one file per category: `editor`, `glyphs`, `input`, `keys`, `layout`, `theme`
and `tree`.

## Common settings

```toml
[layout]
measure    = 72           # text width in columns  (:set measure=N to change live)
typewriter = false        # keep the cursor line centered
focus      = "off"        # "paragraph" or "sentence" dims everything else
numbers    = "off"        # "relative" or "absolute" line-number gutter

[theme]
# The default is Tokyo Night (dark). Override individual colors, or point
# `name` at ~/.config/shoin/themes/<name>.toml.
background = "#1a1b26"
text       = "#c0caf5"

[input]
leader = " "

[editor]
autosave          = false # write modified named buffers on a timer
autosave_interval = 3     # minutes between them, 1–5
autoreload        = true  # take a file's changes back when it is edited
                          # elsewhere — only if you have nothing unsaved

[tree]
show_hidden = false       # dotfiles in the file tree (H toggles it live)

[keys.normal]
"<leader>w" = "save"
```

| Section | What it holds |
|---|---|
| `[layout]` | measure, alignment, focus, numbers, typewriter, conceal, line spacing |
| `[theme]` | every color, from the background to syntax highlighting and diff |
| `[input]` | leader, mouse, the Insert-mode escape alias |
| `[editor]` | indenting, pairs, saving, autosave, autoreload, clipboard |
| `[tree]` | file-tree options |
| `[transclude]` | how embeds resolve and display |
| `[markdown]` | which syntax is styled: tags, wiki links, highlight, tables, code |
| `[glyphs]` | bullets, task boxes, bars and icons; `nerd_fonts = false` for plain fallbacks |
| `[status]` | whether the status line appears, for how long, and what it shows |
| `[cursor]` | cursor shape per mode, and blink |
| `[splash]` | the start screen picture: built-in, `"none"`, or a path to your own |
| `[keys.normal]` `[keys.insert]` `[keys.visual]` | key bindings |

## Key bindings

A binding may name a whole **verb**, not just an action, so the Vim grammar
moves with it: `"s" = "operator_delete"` keeps counts, text objects and the
doubled form (`ss`) all working.

The leader keys are bindings in `keys.conf` like any other — remap them, or
add your own. The shipped `keys.conf` is built in, and your `[keys.*]` merge
over it: a binding you write replaces the shipped one for that key, and
everything you don't mention stays. `"none"` removes a shipped binding:

```toml
[keys.normal]
"<leader>z" = "none"
```

## Where config is found

The first of these that exists wins:

1. `--config <path>`
2. `$SHOIN_CONFIG`
3. `./shoin.conf` (a single file)
4. `./shoin/` (a folder of `*.conf`)
5. `$XDG_CONFIG_HOME/shoin/`
6. `~/.config/shoin/`

## Details

- A parse error keeps the running config instead of dropping you into a broken
  one. A malformed binding is skipped with a warning, never fatal.
- Bindings match exactly, so one that is also the start of a longer binding
  shadows it. Shoin warns about that at load time.
- `[keys.insert]` takes single keys only.
- `:e!` or `:reload` re-reads the config by hand.
- A leader that is a modifier or named key needs the same `<...>` wrapping a
  binding uses: `"<C-x>"`, not `"C-x"`.
