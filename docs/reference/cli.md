# Command line

Open, print, or export Markdown from the shell with `shoin`.

```text
A no-clutter terminal Markdown editor

Usage: shoin [OPTIONS] [FILE]

Arguments:
  [FILE]  File to open. Created on first save if it does not exist. `-`, or no file with text piped in, reads stdin into an unnamed buffer

Options:
      --config <PATH>    Use this config file instead of the discovered one
  -R, --read-only        Open read-only: every line concealed, edits refused, `q` quits. `i` after the refusal message, or `:view`, switches to editing
      --zen              Start with every chrome affordance hidden
      --print-config     Print the resolved config and exit
      --init-config      Write the shipped config into your config directory and exit. Refuses a directory that already holds one unless `--force` is given
      --force            Let `--init-config` replace a config that is already there
      --export <FILE>    Export this file's finished text and exit, without starting the TUI. Writes `<name>.<format>` unless `--out` or `--stdout` says otherwise
      --format <FORMAT>  What `--export` writes: `md`, `txt`, `html` or `pdf`. [default: md]
      --justify          Stretch every wrapped line but a paragraph's last to fill the measure. Only `--format html` and `--format txt` have a layout of their own to stretch
      --out <PATH>       Where `--export` writes
  -p, --render [<FILE>]  Print this file as the editor shows it, styled, and exit — a reader's view without opening the editor. Plain text when stdout is not a terminal, or when `NO_COLOR` is set. `-`, or no FILE at all, reads stdin: `curl …/README.md | shoin -p`
      --pager            With `--render`, page the output through `$PAGER` (`less -R` when unset) when it is taller than the terminal
      --stdout           Print the exported document instead of writing it, so `shoin --export notes.md --stdout | pandoc …` stays a one-shot pipeline. Text formats only — a PDF is not something to print to a terminal
  -h, --help             Print help
  -V, --version          Print version
```

## Rendering

`--render` draws the page the editor would, with every line concealed, at
the terminal's width, and prints all of it. Front matter is left out, every
`![[embed]]` is expanded in full, and pictures are drawn in a terminal that
can show them (the same detection as the editor's). When stdout isn't a
terminal, or `NO_COLOR` is set, it prints plain text instead, the same as
`--export FILE --format txt --stdout`.

`--pager` hands a page taller than the terminal to `$PAGER` (run through
`sh`, so flags work), or `less -R` when it's unset, and sets `LESS=R` if
`LESS` isn't set. A pager can't show pictures, so a paged page draws their
boxes as outlines. Output that fits on screen is printed as usual.

## Reading stdin

`FILE` can be `-`, and with text piped in it can be left out:

```sh
curl -s https://example.com/README.md | shoin -p    # print it
git show HEAD:notes.md | shoin                      # open it in the editor
```

Piped text opens as an unnamed buffer, so `:w name.md` saves it and `:q`
leaves without asking: it hasn't been changed, and it still exists wherever
it came from. The editor reads keys from the terminal, not the pipe. Links
and embeds resolve from the working directory, as on a blank page.

## Read-only

`shoin -R notes.md` opens a page to read rather than to edit. Every line is
concealed, the cursor's too, and there's no caret: `j`/`k`, `Ctrl-d`/`Ctrl-u`,
`G`/`gg` and search scroll the page, as in `less`. `q` quits. Yanking,
panels, `gf` and `:w` still work. Press `i` as your first command to start
editing. After moving or running another command, press `i` after the
"read-only — press i to edit" message. `:view` toggles read-only. The keys
are in the [key reference](keys.md#read-only), and
[Reading](../reading.md#read-only) walks through it.

## Details

With `--init-config`, `--config` sets the destination. The default is
`$XDG_CONFIG_HOME/shoin` or `~/.config/shoin`. See
[Configuration defaults](config.md) for the settings it writes.

Without `--out` or `--stdout`, `--export` writes `<name>.<format>` beside
the file, or `<name>-export.<format>` when that is the file itself: Markdown
from `notes.md` goes to `notes-export.md`. With no `FILE` and nothing piped in, Shoin opens a blank page with
the splash screen.
`SHOIN_CONFIG` names a config like `--config` does, and
`SHOIN_IMAGE_PROTOCOL=kitty|iterm|none` overrides image detection. See
[Configuration](../configuration.md#where-config-is-found) and
[Export](../export.md).
