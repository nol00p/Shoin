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
    /// This is what `n` / `p` step through and what a per-difference choice
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

/// Which version of one difference survives the merge.
///
/// A three-rung ladder cycled by one key, in the shape `transclude::Mode` set:
/// the two simple answers first, and `Both` as the deliberate extra rather than
/// something you pass through on the way back to `Live`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    /// What is in the editor. The DEFAULT, so a merge with nothing toggled
    /// keeps the reader's unsaved work rather than quietly dropping it.
    #[default]
    Live,
    /// What is on disk.
    File,
    /// Both, the editor's first — matching the left-to-right column order, so
    /// the output order is the one the screen already showed.
    Both,
}

impl Side {
    pub fn cycle(self) -> Side {
        match self {
            Side::Live => Side::File,
            Side::File => Side::Both,
            Side::Both => Side::Live,
        }
    }

    /// The divider-column glyph. It points at the side that wins, which is one
    /// less thing to remember than a letter would be.
    pub fn glyph(self) -> &'static str {
        match self {
            Side::Live => "\u{25c0}",
            Side::File => "\u{25b6}",
            Side::Both => "\u{25c6}",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Side::Live => "live",
            Side::File => "file",
            Side::Both => "both",
        }
    }
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
    /// Which hunk `n` / `p` last landed on, so the view can mark it.
    pub hunk: usize,
    /// The chosen side for each hunk, parallel to `align.hunks`.
    ///
    /// Every hunk always HAS a side, which is what makes the view a complete
    /// document at all times: there is no undecided state for `w` to trip over,
    /// and no partial application to unwind if the reader aborts.
    pub sides: Vec<Side>,
    /// The file's mtime when the snapshot was taken. Checked again before
    /// committing, so a merge cannot write over a THIRD edit it never showed.
    pub snapshot_mtime: Option<std::time::SystemTime>,
}

impl DiffView {
    pub fn new(
        doc: usize,
        name: String,
        mine: Vec<String>,
        theirs: Vec<String>,
        snapshot_mtime: Option<std::time::SystemTime>,
    ) -> DiffView {
        let align = align(&mine, &theirs);
        let sides = vec![Side::default(); align.hunks.len()];
        DiffView {
            doc,
            name,
            mine,
            theirs,
            align,
            scroll: 0,
            hunk: 0,
            sides,
            snapshot_mtime,
        }
    }

    /// The side chosen for the hunk containing `row`, or `None` for a row that
    /// is the same on both sides.
    pub fn side_at(&self, row: usize) -> Option<Side> {
        let h = self.align.hunks.iter().position(|h| h.contains(&row))?;
        self.sides.get(h).copied()
    }

    /// Advance the current hunk's choice one rung.
    pub fn cycle_current(&mut self) -> Option<Side> {
        let side = self.sides.get_mut(self.hunk)?;
        *side = side.cycle();
        Some(*side)
    }

    pub fn set_all(&mut self, side: Side) {
        self.sides.iter_mut().for_each(|s| *s = side);
    }

    /// Assemble the merged document from the per-hunk choices.
    ///
    /// Walks the alignment ONE HUNK AT A TIME rather than row by row: a hunk is
    /// the unit of choice, so `Both` has to emit all of its left lines and then
    /// all of its right lines, which a per-row walk could not order correctly.
    /// That also makes the one-sided rows free — a difference that exists only
    /// on disk has no live counterpart, so `Both` and `File` agree there
    /// without a special case.
    pub fn merged(&self) -> String {
        let mut out: Vec<&str> = Vec::with_capacity(self.align.rows.len());
        let mut i = 0usize;
        let mut h = 0usize;
        while i < self.align.rows.len() {
            let row = self.align.rows[i];
            if row.is_same() {
                if let Some(l) = row.left() {
                    out.push(self.mine[l].as_str());
                }
                i += 1;
                continue;
            }
            // `group` guarantees the hunks are in order and cover exactly the
            // runs of differing rows, so the hunk starting here is this one.
            let range = self.align.hunks[h].clone();
            let side = self.sides.get(h).copied().unwrap_or_default();
            let rows = &self.align.rows[range.clone()];
            if matches!(side, Side::Live | Side::Both) {
                out.extend(rows.iter().filter_map(Row::left).map(|l| self.mine[l].as_str()));
            }
            if matches!(side, Side::File | Side::Both) {
                out.extend(rows.iter().filter_map(Row::right).map(|r| self.theirs[r].as_str()));
            }
            i = range.end;
            h += 1;
        }
        match out.is_empty() {
            true => String::new(),
            // A trailing newline, as every text path here produces; the save
            // policy (`final_newline`) is what decides whether the FILE keeps it.
            false => format!("{}\n", out.join("\n")),
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

    fn view(mine: &[&str], theirs: &[&str]) -> DiffView {
        DiffView::new(0, "t.md".into(), v(mine), v(theirs), None)
    }

    /// The ladder, and that it comes back round. `Both` is the deliberate extra
    /// at the end rather than something you pass through returning to `Live`.
    #[test]
    fn the_side_ladder_cycles_live_file_both() {
        assert_eq!(Side::default(), Side::Live, "the default keeps your work");
        assert_eq!(Side::Live.cycle(), Side::File);
        assert_eq!(Side::File.cycle(), Side::Both);
        assert_eq!(Side::Both.cycle(), Side::Live);
    }

    /// Every hunk starts with a side, so the view is a complete document from
    /// the moment it opens — there is no undecided state for `w` to trip on.
    #[test]
    fn every_hunk_starts_chosen_and_defaults_to_live() {
        let v = view(&["a", "mine", "c", "x", "e"], &["a", "theirs", "c", "y", "e"]);
        assert_eq!(v.sides.len(), v.align.hunks.len());
        assert_eq!(v.sides, vec![Side::Live, Side::Live]);
        assert_eq!(v.side_at(1), Some(Side::Live), "a differing row has a side");
        assert_eq!(v.side_at(0), None, "an identical row has none");
        assert_eq!(v.merged(), "a\nmine\nc\nx\ne\n", "so w with no toggles keeps live");
    }

    /// The three whole-document answers.
    #[test]
    fn merged_follows_the_chosen_side() {
        let mut v = view(&["keep", "mine", "tail"], &["keep", "theirs", "extra", "tail"]);
        assert_eq!(v.merged(), "keep\nmine\ntail\n");

        v.set_all(Side::File);
        assert_eq!(v.merged(), "keep\ntheirs\nextra\ntail\n");

        // Both: every left line of the hunk, THEN every right line — the
        // left-to-right order the screen showed.
        v.set_all(Side::Both);
        assert_eq!(v.merged(), "keep\nmine\ntheirs\nextra\ntail\n");
    }

    /// The point of the feature: different answers for different differences.
    #[test]
    fn each_hunk_merges_on_its_own_choice() {
        let mut v = view(&["a", "mine", "c", "x", "e"], &["a", "theirs", "c", "y", "e"]);
        assert_eq!(v.align.hunks.len(), 2);
        v.sides[0] = Side::File;
        v.sides[1] = Side::Both;
        assert_eq!(v.merged(), "a\ntheirs\nc\nx\ny\ne\n");

        // Cycling acts on the CURRENT hunk only.
        v.set_all(Side::Live);
        v.hunk = 1;
        assert_eq!(v.cycle_current(), Some(Side::File));
        assert_eq!(v.sides, vec![Side::Live, Side::File], "hunk 0 untouched");
    }

    /// A one-sided difference has no counterpart to append, so `Both` and the
    /// side that HAS the lines agree — without a special case anywhere.
    #[test]
    fn both_on_a_one_sided_hunk_is_not_a_duplicate() {
        let mut v = view(&["a", "b"], &["a", "new", "b"]);
        v.set_all(Side::Both);
        assert_eq!(v.merged(), "a\nnew\nb\n");
        v.set_all(Side::File);
        assert_eq!(v.merged(), "a\nnew\nb\n", "same answer, nothing doubled");
        v.set_all(Side::Live);
        assert_eq!(v.merged(), "a\nb\n", "and live drops the insertion");
    }

    /// Identical texts merge to themselves whatever is selected — there are no
    /// hunks, so there is nothing for a choice to change.
    #[test]
    fn merging_an_identical_pair_changes_nothing() {
        let mut v = view(&["one", "two"], &["one", "two"]);
        assert!(v.sides.is_empty());
        for s in [Side::Live, Side::File, Side::Both] {
            v.set_all(s);
            assert_eq!(v.merged(), "one\ntwo\n");
        }
        assert_eq!(v.cycle_current(), None, "nothing to cycle");
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
