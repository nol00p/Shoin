# Shoin

A no-clutter terminal Markdown editor. This file exercises the block styling —
headings, lists, quotes, tables and fenced code. Open it to see concealment at
work: the line your cursor is on shows its raw Markdown, every other line shows
the finished text.

## Headings cascade through six colors

### Third level
#### Fourth level

## Lists and tasks

- a plain bullet
- adding a new one
- another one
    - a nested item, indent-guided
* a star bullet
1. first ordered
2. second ordered

- [ ] an open task
- [x] a finished task

## Inline styling

This paragraph has **bold**, *italic*, ***both***, ~~struck~~, ==highlit==,
and `inline code`. A [labelled link](https://example.com), a [[Wiki Page]],
a #project tag, a bare https://example.com/path, and an escaped \*star\*.

## Quotes

> The active line is always rendered 1:1 with its source.
> Every other line may conceal markup.

## Code

```rust
fn main() {
    // a fence that names its language is highlighted
    let greeting: &str = "hello";
    println!("{greeting}, {}", 1 + 1);
}
```

A language shoin does not know keeps the flat slab, and so does
`:set code_syntax off`.

---

Ordinary prose flows in a centered measure with wide margins, and that's the
whole interface.
