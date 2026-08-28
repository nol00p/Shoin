# Transclusion example

`main.md` is a **composition**: a short file whose body is mostly `![[…]]`
links to notes in `fragments/` and `reference/`.

```
main.md              the composition
fragments/           notes written on their own, for their own sake
  attention.md         embedded whole
  switching-cost.md    embedded whole, and quoted one block at a time (^restart)
  practice.md          embeds reference/sources.md itself — one level deeper
reference/
  glossary.md          embedded one section at a time (#Transclusion)
  sources.md           reached by bare name from inside fragments/practice.md
```

## Try it

Open the composition. Embeds expand in place by default:

```sh
shoin examples/transclusion/main.md
```

`:embed [mode]` chooses how much to show — a ladder, each step showing more:

| Mode | What you see |
|---|---|
| `:embed none` | the `![[…]]` links, unexpanded |
| `:embed short` | each target's own text, in a labelled box (the default) |
| `:embed rec` | …and whatever those targets embed, recursively |
| `:embed full` | the same, with no boxes — one continuous document |

`practice.md` is the one to watch: it embeds `reference/sources.md` itself, so
in `short` you see the nested link and in `rec` you see the reading list. A bare
`:embed` toggles off and back to whichever mode you last chose.

`:embed full` shows exactly what `:export` writes, which is the point of it.

`:export pdf` needs `pandoc` and a layout engine on `PATH`
(`brew install pandoc typst` is the smallest pair that works). The YAML front
matter at the top of `main.md` becomes the PDF's title block.

Move the cursor onto any `![[…]]` line — it collapses back to its source, the
same rule that shows you the `**` around bold text you are editing. Move away
and it expands again.

Write the finished document out — `:export md`, `:export txt`, `:export html`
or `:export pdf` opens a dialog with a suggested filename you can edit. Or from
the shell, without opening the editor:

```sh
shoin --export examples/transclusion/main.md --format txt --stdout
```

## What to look for in the output

- **Headings are demoted.** Each fragment is written as an `# H1` in its own
  file, because on its own that is what it is. Embedded under this document's
  `# H1` it becomes an `## H2`, so the exported outline holds together.
- **Front matter is kept once.** `main.md` keeps its own; the fragments' is
  dropped.
- **Block ids disappear.** `^restart` and `^rule` are addresses, not prose.
- **`practice.md` embeds `sources.md` by bare name**, with no path — resolution
  searches the whole example folder when a relative path does not hit. Compile
  expands it; live preview stops one level down and shows the link.
- **The broken link becomes a visible placeholder**, never a silent gap.
