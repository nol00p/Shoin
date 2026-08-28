---
title: Composing from atomic notes
author: nol00p
---
# Composing from atomic notes

This document is made mostly of other documents. Every section with a
heading you did not read here is a separate note under `fragments/` or
`reference/`, pulled in by reference rather than pasted.

Press `:embed short` to expand the links in place, or `:export` to write
the finished document out as Markdown, plain text, HTML or a PDF. Put the
cursor on any `![[…]]` line and it collapses back to its source, the way
`**bold**` does.

![[fragments/attention]]

![[fragments/switching-cost]]

## One idea, then another

The two notes above were written weeks apart and never for this
document. That is the point of the practice: capture is cheap,
arrangement is where the thinking happens, and the two should not be
the same act.

![[reference/glossary#Transclusion]]

![[fragments/practice]]

## What each link form does

| Written | Pulls in |
|---|---|
| `![[fragments/attention]]` | the whole file |
| `![[reference/glossary#Transclusion]]` | one section of it |
| `![[fragments/switching-cost#^restart]]` | one block |
| `[[fragments/attention]]` | nothing — a plain link, just styled |

Here is that third form on its own, quoting a single sentence rather
than the note that holds it:

![[fragments/switching-cost#^restart]]

And a link that does not resolve, so you can see what a broken
reference looks like rather than finding a hole in your export:

![[fragments/does-not-exist]]
