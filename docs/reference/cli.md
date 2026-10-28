# Command line

```
shoin [FILE]
cat notes.md | shoin [-p]

  --config <PATH>     use this config instead of the discovered one
                      (with --init-config, write to THIS path instead)
  --print-config      print the resolved config and exit
  --init-config       write the shipped config to ~/.config/shoin and exit
  --force             let --init-config replace a config already there
  --zen               start with every chrome affordance hidden
  -R, --read-only     open read-only: every line concealed, q quits
  -p, --render [FILE] print the file as the editor shows it, and exit
                      (no FILE, or -, reads stdin)
  --pager             with --render, page it through $PAGER when it is
                      taller than the terminal (default: less -R)
  --export <FILE>     export without starting the editor
  --format <FORMAT>   md | txt | html | pdf          (default: md)
  --justify           stretch html/txt to fill the measure (md, pdf: refused)
  --out <PATH>        where --export writes
  --stdout            print the export instead of writing it (text formats only)
  -h, --help          print help
  -V, --version       print the version
```

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
panels, `gf` and `:w` still work; anything that would change the text is
refused with "read-only — press i to edit", and `i` then switches to
editing. `:view` turns the mode on and off from inside the editor.

## Other options

Without `--out` or `--stdout`, `--export` writes `<name>.<format>` beside
the file. With no `FILE` and nothing piped in, Shoin opens a blank page with
the splash screen.
`SHOIN_CONFIG` names a config like `--config` does, and
`SHOIN_IMAGE_PROTOCOL=kitty|iterm|none` overrides image detection. See
[Configuration](../configuration.md#where-config-is-found) and
[Export](../export.md).
