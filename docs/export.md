# Export

`:export` writes the finished document — embeds expanded — as Markdown, plain
text, a self-contained HTML page, or a PDF. The same export runs from the
shell without opening the editor.

## From the editor

```
  :export [md|txt|html|pdf] [justify]
```

It opens a save dialog with a suggested name you can change. The default
format is `md`.

## From the shell

```sh
shoin --export notes.md --format html --out notes.html
shoin --export notes.md --format txt --stdout | pandoc …
shoin --export notes.md --format html --justify --out notes.html
```

No terminal is acquired, so it composes into pipelines and build scripts. The
flags are in the [command-line reference](reference/cli.md).

## Formats

| Format | What you get |
|---|---|
| `md` | Markdown with every embed expanded |
| `txt` | plain text |
| `html` | one self-contained page; pictures are carried inside it as `data:` URIs |
| `pdf` | through `pandoc` and a layout engine (`typst` is the smallest) |

## Details

- `justify` stretches every wrapped line but a paragraph's last to fill the
  measure. It applies to `html` and `txt` only: `md` and `pdf` have no layout
  of their own to stretch, so they refuse it.
- PDF export needs `pandoc` and a layout engine. If one is missing, the error
  names which.
- `--stdout` works for the text formats only.
- An unresolved embed exports as a visible placeholder, never a silent gap.
