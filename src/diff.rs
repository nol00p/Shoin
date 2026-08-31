//! Line diff for the conflict view. SPEC.md §10.
//!
//! Produces an ALIGNMENT rather than a patch: one entry per screen row of the
//! side-by-side view, each naming the line it shows on the left, on the right,
//! or on only one of them. That is the shape the view wants, and it is why
//! nothing here talks about `+`/`-` — a unified patch would have to be
//! re-paired into rows by the renderer, which is the same work done later and
//! with less information.
//!
//! Hand-rolled, and not a dependency, for the reason `render/markdown/code.rs`
//! is: SPEC §11 keeps the dependency list to things that would be worse to
//! write than to take, and a line-level LCS is a page of code with an exact
//! answer. See `MAX_CELLS` for the one place it gives up.

use std::ops::Range;

/// Largest LCS table the aligner will allocate, in cells.
///
/// The table is O(left × right) on the lines left AFTER the common prefix and
/// suffix are trimmed, which is what makes this affordable at all: an external
/// edit to a 5 000-line note usually leaves a middle of a handful of lines.
/// A genuinely wholesale rewrite of a large file is the case this refuses, and
/// it says so — `Alignment::coarse` — rather than allocating gigabytes to align
/// two texts that share nothing.
const MAX_CELLS: usize = 1_000_000;

/// One row of the side-by-side view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// The same text on both sides.
    Same { left: usize, right: usize },
    /// Both sides have a line here and the two differ.
    Changed { left: usize, right: usize },
    /// Only the right side has a line; the left shows filler.
    Added { right: usize },
    /// Only the left side has a line; the right shows filler.
    Removed { left: usize },
}

impl Row {
    pub fn left(&self) -> Option<usize> {
        match *self {
            Row::Same { left, .. } | Row::Changed { left, .. } | Row::Removed { left } => Some(left),
            Row::Added { .. } => None,
        }
    }

    pub fn right(&self) -> Option<usize> {
        match *self {
            Row::Same { right, .. } | Row::Changed { right, .. } | Row::Added { right } => {
                Some(right)
            }
            Row::Removed { .. } => None,
        }
    }

    pub fn is_same(&self) -> bool {
        matches!(self, Row::Same { .. })
    }
}

/// The rows of the view, plus where the differences are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Alignment {
    pub rows: Vec<Row>,
    /// One range of `rows` per run of consecutive differing rows — a "hunk".
    /// This is what `]c` / `[c` step through and what a per-difference choice
    /// is made against, so it is computed here rather than rediscovered by
    /// every consumer.
    pub hunks: Vec<Range<usize>>,
    /// The middle was too large to align exactly (see `MAX_CELLS`), so it was
    /// emitted as one block. The rows are still correct and still cover every
    /// line; they are just coarser than an LCS would have made them.
    pub coarse: bool,
}

impl Alignment {
    pub fn is_identical(&self) -> bool {
        self.hunks.is_empty()
    }
}

/// Align two sequences of lines for side-by-side display.
pub fn align(left: &[String], right: &[String]) -> Alignment {
    // Trim what matches at both ends first. This is the whole reason the table
    // below is affordable: the common case is a small edit inside a long file.
    let prefix = left
        .iter()
        .zip(right.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let max_suffix = (left.len() - prefix).min(right.len() - prefix);
    let suffix = (0..max_suffix)
        .take_while(|k| left[left.len() - 1 - k] == right[right.len() - 1 - k])
        .count();

    let mut rows: Vec<Row> = Vec::with_capacity(left.len().max(right.len()));
    for i in 0..prefix {
        rows.push(Row::Same { left: i, right: i });
    }

    let l_mid = &left[prefix..left.len() - suffix];
    let r_mid = &right[prefix..right.len() - suffix];
    let coarse = l_mid.len().saturating_mul(r_mid.len()) > MAX_CELLS;
    let ops = match coarse {
        // Everything in the middle differs, as far as we are willing to look.
        true => {
            let mut v: Vec<Op> = (0..l_mid.len()).map(Op::Del).collect();
            v.extend((0..r_mid.len()).map(Op::Add));
            v
        }
        false => lcs_ops(l_mid, r_mid),
    };
    emit(&ops, prefix, prefix, &mut rows);

    for k in (0..suffix).rev() {
        rows.push(Row::Same {
            left: left.len() - 1 - k,
            right: right.len() - 1 - k,
        });
    }

    let hunks = group(&rows);
    Alignment { rows, hunks, coarse }
}

/// A step through the two sequences, before differing runs are paired up.
enum Op {
    Keep(usize, usize),
    Del(usize),
    Add(usize),
}

/// Longest-common-subsequence walk over the trimmed middles.
fn lcs_ops(a: &[String], b: &[String]) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        let mut v: Vec<Op> = (0..n).map(Op::Del).collect();
        v.extend((0..m).map(Op::Add));
        return v;
    }

    // dp[i * (m + 1) + j] = LCS length of a[i..] and b[j..]. Filled backwards so
    // the walk below can go forwards and take the longer tail at each step.
    let stride = m + 1;
    let mut dp = vec![0u32; (n + 1) * stride];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * stride + j] = if a[i] == b[j] {
                dp[(i + 1) * stride + j + 1] + 1
            } else {
                dp[(i + 1) * stride + j].max(dp[i * stride + j + 1])
            };
        }
    }

    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Keep(i, j));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * stride + j] >= dp[i * stride + j + 1] {
            ops.push(Op::Del(i));
            i += 1;
        } else {
            ops.push(Op::Add(j));
            j += 1;
        }
    }
    ops.extend((i..n).map(Op::Del));
    ops.extend((j..m).map(Op::Add));
    ops
}

/// Turn the op walk into rows, PAIRING each run of deletions with the run of
/// additions beside it.
///
/// The pairing is what makes a side-by-side view readable: an edited line is
/// one row showing both versions, not a removed row followed by an added row
/// several lines apart. Whatever is left over when one run is longer becomes a
/// one-sided row with filler opposite.
fn emit(ops: &[Op], l_base: usize, r_base: usize, rows: &mut Vec<Row>) {
    let mut dels: Vec<usize> = Vec::new();
    let mut adds: Vec<usize> = Vec::new();
    let flush = |dels: &mut Vec<usize>, adds: &mut Vec<usize>, rows: &mut Vec<Row>| {
        let paired = dels.len().min(adds.len());
        for k in 0..paired {
            rows.push(Row::Changed {
                left: l_base + dels[k],
                right: r_base + adds[k],
            });
        }
        for &d in &dels[paired..] {
            rows.push(Row::Removed { left: l_base + d });
        }
        for &a in &adds[paired..] {
            rows.push(Row::Added { right: r_base + a });
        }
        dels.clear();
        adds.clear();
    };

    for op in ops {
        match *op {
            Op::Keep(i, j) => {
                flush(&mut dels, &mut adds, rows);
                rows.push(Row::Same {
                    left: l_base + i,
                    right: r_base + j,
                });
            }
            Op::Del(i) => dels.push(i),
            Op::Add(j) => adds.push(j),
        }
    }
    flush(&mut dels, &mut adds, rows);
}

/// Runs of consecutive differing rows.
fn group(rows: &[Row]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, r) in rows.iter().enumerate() {
        match (r.is_same(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                out.push(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(s..rows.len());
    }
    out
}

/// Split a document into the lines the aligner compares.
///
/// Trailing-newline handling matters: `"a\n"` is ONE line, not two. Splitting
/// naively on `\n` gives a phantom empty last line, which would show up as a
/// spurious difference against a file that merely lacks the final newline.
pub fn lines_of(text: &str) -> Vec<String> {
    let body = text.strip_suffix('\n').unwrap_or(text);
    if body.is_empty() && text.is_empty() {
        return Vec::new();
    }
    body.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect()
}

/// The `:diff` conflict view: the buffer beside the file, aligned.
///
/// A SELF-DRAWN overlay in the shape of `Help` and `Finder`, not a pane showing
/// a document. That choice is what keeps the feature small: the two columns
/// share one scroll offset by construction (so there is no pane to keep in
/// step), filler rows are a thing the renderer draws rather than a fourth
/// `RowSource` the cache has to count, and the disk text never has to become a
/// `Buffer` at all — which also sidesteps `open_file` deduplicating by path and
/// handing back the very buffer being compared.
///
/// It holds SNAPSHOTS. The comparison a reader is deciding about must not
/// change under them because a background poll noticed something, so both sides
/// are captured when the view opens and `App::check_disk` leaves a document
/// alone while its diff is open.
pub struct DiffView {
    /// Which document this compares. Held by index because that is what `App`
    /// indexes by; validated on use, since a buffer can close under an overlay.
    pub doc: usize,
    pub name: String,
    /// The buffer's lines when the view opened.
    pub mine: Vec<String>,
    /// The file's lines when the view opened.
    pub theirs: Vec<String>,
    pub align: Alignment,
    pub scroll: usize,
    /// Which hunk `]c` / `[c` last landed on, so the view can mark it.
    pub hunk: usize,
    /// The `]` or `[` of a half-typed `]c` / `[c`. One character of memory is
    /// all this grammar needs, so it does not borrow `input::Pending`.
    pub pending: Option<char>,
}

impl DiffView {
    pub fn new(doc: usize, name: String, mine: Vec<String>, theirs: Vec<String>) -> DiffView {
        let align = align(&mine, &theirs);
        DiffView {
            doc,
            name,
            mine,
            theirs,
            align,
            scroll: 0,
            hunk: 0,
            pending: None,
        }
    }

    pub fn rows(&self) -> usize {
        self.align.rows.len()
    }

    /// Scroll so that `row` is on screen, keeping it off the very edge when
    /// there is room — the same courtesy `scroll_off` gives the editor.
    pub fn reveal(&mut self, row: usize, height: usize) {
        let pad = 2.min(height / 4);
        if row < self.scroll + pad {
            self.scroll = row.saturating_sub(pad);
        } else if row + pad >= self.scroll + height {
            self.scroll = (row + pad + 1).saturating_sub(height);
        }
        let max = self.rows().saturating_sub(height);
        self.scroll = self.scroll.min(max);
    }

    /// Move to the next (or previous) difference, returning the row it starts
    /// at. `None` when there are no differences at all.
    pub fn step_hunk(&mut self, forward: bool) -> Option<usize> {
        let n = self.align.hunks.len();
        if n == 0 {
            return None;
        }
        // Wraps, because a reader stepping past the last difference means
        // "show me them again", not "do nothing".
        self.hunk = match forward {
            true => (self.hunk + 1) % n,
            false => (self.hunk + n - 1) % n,
        };
        Some(self.align.hunks[self.hunk].start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    /// Every row must account for the lines it claims, in order and without
    /// gaps — the property the view depends on and the one a clever pairing is
    /// most likely to break.
    fn check_covers(a: &Alignment, left: &[String], right: &[String]) {
        let ls: Vec<usize> = a.rows.iter().filter_map(Row::left).collect();
        let rs: Vec<usize> = a.rows.iter().filter_map(Row::right).collect();
        assert_eq!(ls, (0..left.len()).collect::<Vec<_>>(), "left coverage");
        assert_eq!(rs, (0..right.len()).collect::<Vec<_>>(), "right coverage");
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        let a = v(&["one", "two", "three"]);
        let al = align(&a, &a);
        assert!(al.is_identical());
        assert_eq!(al.rows.len(), 3);
        assert!(al.rows.iter().all(Row::is_same));
        check_covers(&al, &a, &a);
    }

    /// An edited line is ONE row showing both versions, not a removed row and
    /// an added row. That pairing is the whole point of the alignment.
    #[test]
    fn an_edited_line_is_a_single_changed_row() {
        let l = v(&["keep", "mine", "tail"]);
        let r = v(&["keep", "theirs", "tail"]);
        let al = align(&l, &r);
        assert_eq!(
            al.rows,
            vec![
                Row::Same { left: 0, right: 0 },
                Row::Changed { left: 1, right: 1 },
                Row::Same { left: 2, right: 2 },
            ]
        );
        assert_eq!(al.hunks, vec![1..2]);
        check_covers(&al, &l, &r);
    }

    /// An insertion gets filler on the left, a deletion filler on the right —
    /// and neither is paired into a bogus "changed".
    #[test]
    fn one_sided_runs_get_filler_opposite() {
        let l = v(&["a", "b"]);
        let r = v(&["a", "new", "b"]);
        let al = align(&l, &r);
        assert_eq!(al.rows[1], Row::Added { right: 1 });
        assert!(al.rows[1].left().is_none(), "filler on the left");
        check_covers(&al, &l, &r);

        let al = align(&r, &l);
        assert_eq!(al.rows[1], Row::Removed { left: 1 });
        assert!(al.rows[1].right().is_none(), "filler on the right");
        check_covers(&al, &r, &l);
    }

    /// A longer replacement pairs what it can and leaves the rest one-sided.
    #[test]
    fn uneven_runs_pair_then_spill() {
        let l = v(&["top", "x", "bottom"]);
        let r = v(&["top", "1", "2", "3", "bottom"]);
        let al = align(&l, &r);
        assert_eq!(al.rows[1], Row::Changed { left: 1, right: 1 });
        assert_eq!(al.rows[2], Row::Added { right: 2 });
        assert_eq!(al.rows[3], Row::Added { right: 3 });
        assert_eq!(al.hunks, vec![1..4], "one hunk, not three");
        check_covers(&al, &l, &r);
    }

    #[test]
    fn separate_edits_are_separate_hunks() {
        let l = v(&["a", "b", "c", "d", "e"]);
        let r = v(&["a", "B", "c", "D", "e"]);
        let al = align(&l, &r);
        assert_eq!(al.hunks, vec![1..2, 3..4], "two hunks, not one spanning c");
        assert!(al.rows[2].is_same(), "the line between them is untouched");
    }

    /// Empty on either side is a real case: a file truncated to nothing, or a
    /// buffer that has not been written yet.
    #[test]
    fn an_empty_side_is_all_one_sided() {
        let l = v(&["a", "b"]);
        let empty: Vec<String> = Vec::new();
        let al = align(&l, &empty);
        assert_eq!(al.rows, vec![Row::Removed { left: 0 }, Row::Removed { left: 1 }]);
        check_covers(&al, &l, &empty);

        let al = align(&empty, &l);
        assert_eq!(al.rows, vec![Row::Added { right: 0 }, Row::Added { right: 1 }]);
        check_covers(&al, &empty, &l);

        let al = align(&empty, &empty);
        assert!(al.is_identical());
        assert!(al.rows.is_empty());
    }

    /// The prefix/suffix trim must not swallow a line it only APPEARS to match.
    /// `["a","a","b"]` vs `["a","b"]` shares prefix "a" and suffix "b", and the
    /// remaining middle must still be accounted for exactly once.
    #[test]
    fn overlapping_prefix_and_suffix_do_not_double_count() {
        let l = v(&["a", "a", "b"]);
        let r = v(&["a", "b"]);
        let al = align(&l, &r);
        check_covers(&al, &l, &r);
        assert_eq!(al.rows.len(), 3);

        // The pathological one: every line the same, one side longer.
        let l = v(&["x", "x", "x", "x"]);
        let r = v(&["x", "x"]);
        let al = align(&l, &r);
        check_covers(&al, &l, &r);
    }

    /// Trailing newlines are not a difference. `"a\n"` is one line.
    #[test]
    fn a_trailing_newline_is_not_a_line() {
        assert_eq!(lines_of("a\nb\n"), v(&["a", "b"]));
        assert_eq!(lines_of("a\nb"), v(&["a", "b"]));
        assert_eq!(lines_of(""), Vec::<String>::new());
        assert_eq!(lines_of("\n"), v(&[""]));
        // CRLF on disk against an LF rope must not read as every line changed.
        assert_eq!(lines_of("a\r\nb\r\n"), v(&["a", "b"]));
        assert!(align(&lines_of("a\nb\n"), &lines_of("a\nb")).is_identical());
    }

    /// A middle too large to align exactly still covers every line, and says
    /// that it gave up rather than pretending to a precision it does not have.
    #[test]
    fn an_oversized_middle_falls_back_but_still_covers() {
        let n = 1200; // 1200 x 1200 = 1.44M cells, past MAX_CELLS
        let l: Vec<String> = (0..n).map(|i| format!("left {i}")).collect();
        let r: Vec<String> = (0..n).map(|i| format!("right {i}")).collect();
        let al = align(&l, &r);
        assert!(al.coarse, "it should report the fallback");
        check_covers(&al, &l, &r);
        assert_eq!(al.hunks.len(), 1, "the whole middle is one block");

        // …and the same shape stays exact when a shared prefix/suffix trims it
        // down, which is the case that actually happens.
        let mut r2 = l.clone();
        r2[600] = "changed".into();
        let al = align(&l, &r2);
        assert!(!al.coarse, "trimming brought it back inside the cap");
        assert_eq!(al.hunks, vec![600..601]);
    }
}
