# Export

`:export` writes the finished document — embeds expanded — as Markdown, plain
text, a self-contained HTML page, or a PDF. The same export runs from the
shell without opening the editor.

## From the editor

```
  :export [md|txt|html|pdf] [justify]
```

It opens a save dialog with a suggested name you can change: the document's
name with the format's extension, beside it. The default format is `md`, and
since `notes.md` is the document itself, its Markdown export is named
`notes-export.md`.

## From the shell

```sh
shoin --export notes.md --format html --out notes.html
shoin --export notes.md --format txt --stdout | pandoc …
shoin --export notes.md --format html --justify --out notes.html
```

Without `--out`, the file is written beside the document under the name
`:export` suggests. No terminal is acquired, so it composes into pipelines and build scripts. The
flags are in the [command-line reference](reference/cli.md).

To read a note in the terminal rather than write a file, use `shoin -p`
instead: it prints the page as the editor shows it, styled, with pictures.
`--export` is for files and pipelines. See [Reading](reading.md#print-a-note).

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
- Export is clean by default: math as math, tables as tables, code
  highlighted, whatever your `[markdown]` settings show in the editor. With
  `export_follows_settings = true` under `[markdown]`, `txt` and `html`
  follow those settings instead, so syntax switched off in the editor
  exports as plain text; `math = false` then also reaches `pdf`, where `$`
  stays a dollar sign.
- An unresolved embed exports as a visible placeholder, never a silent gap.
- Export refuses a destination that is the document itself or any note or
  picture it embeds, under any name: through `..`, a symlink or a hard link.
- In `html`, a wiki link becomes a link to `<note>.html`. A wiki link whose
  target has a scheme (`[[https://…]]`) or starts with `//` exports as plain
  text: it names an address, not a note. In `txt` a wiki link is its label.
- In `html`, each heading gets an id from its text; a repeated heading
  gets `-1`, `-2` added, as on GitHub. A line ending in two spaces or a
  backslash breaks the line (`<br>`); in `txt` the line just ends there, and
  the spaces or backslash go. Consecutive `>` lines are one quoted
  paragraph; a bare `>` starts the next.
- Setext headings (`Title` over `=====`), footnotes, indented code blocks
  and raw HTML are not part of Shoin's Markdown. They export as the text
  they are.
- A link or picture title (`[a](url "title")`) becomes the `title`
  attribute in `html`; `txt` drops it.
- A relative picture path is read from the document's folder. `md` written
  to another folder gets its picture paths rewritten to work from there;
  `pdf` hands pandoc absolute paths, so it works from any folder. `txt`
  names a picture as `[image: alt]`. Relative links to other files are not
  rewritten.
