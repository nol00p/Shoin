# Live preview

<p align="center">
  <img src="media/tour.gif" alt="The cursor's line showing raw Markdown while the lines around it are rendered" width="800">
</p>

The line under the cursor shows its raw Markdown. Every other line shows the
finished text, with the markers hidden and the styling applied. You always
edit exactly what is in the file, and always read what it will become.

```
  ▍A finished paragraph with bold text and a link.        ← concealed
  ▍
  ▍This is the line I am **editing** right now.           ← raw, cursor here
  ▍
  ▍ Another finished line with inline code.              ← concealed
```

> The active line is always rendered 1:1 with its source.
> Every other line may conceal its markup.

## What gets rendered

Headings, **bold**, *italic*, ~~strikethrough~~, `==highlight==`, `` `code` ``,
links and `[[wiki links]]`, `#tags`, lists with bullets and task boxes, block
quotes, tables, and fenced code. A fence that names its language is
syntax-highlighted, in seventeen languages; `:set code_syntax off` turns that
off. [`examples/formatting.md`](../examples/formatting.md) exercises all of it.

## Details

- In Visual mode the whole selection renders raw, so you never operate on
  markers you can't see.
- An `![[embed]]` on the cursor's line also shows its raw source, which is why
  embedded text can't be edited by accident. See [Notes](notes.md).
- Table rows never wrap. A row wider than the pane stays on one line and is
  clipped at the edge.
- `layout.conceal = false`, `:set conceal off` or `Space c` turns concealment
  off entirely.
- Why the rule is "the cursor's line is raw": [Design notes](design.md#the-cursors-line-is-always-raw).
