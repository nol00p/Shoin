# Folding

Folds hide a section or a list item behind its first line, so you can
see the shape of a long note. Put the cursor on a heading below and
press `zc`. The section closes into one line that ends with the number
of lines it hides. `zo` opens it again, and `za` does whichever of the
two fits.

`zM` folds every section and list item at once, and `zR` opens them
all. This whole note sits under its `#` heading, so after `zM` it is
one line. Press `zo` on it to see the `##` sections as an outline.
Folds only change the view: the file and its export stay as they are.

## A section to fold

Everything under this heading, down to the next `##` heading, folds
into the heading's line.

### A subsection

A subsection folds on its own. Press `zc` here to close just this
part. Press `zc` again and the `##` section around it closes too.

## A list to fold

Put the cursor on an item that has sub-items and press `zc`:

- Groceries
    - bread
    - apples
    - coffee
- Errands
    - post office
    - library
        - return two books
        - pick up the hold
- Call the dentist

`j` and `k` step over a closed fold as if it were one line. A search
that lands inside a fold opens it.

## The last section

`zR` opens everything you folded above.
