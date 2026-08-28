//! Undo/redo. SPEC.md §4.
//!
//! A single undo STEP is a group of primitive `Change`s. Grouping is driven by
//! the caller (`app`): each Normal/Visual command opens a group and each Insert
//! session runs inside one, so an operator (`dd`, `dw`) or a whole `i…Esc` is
//! one undo step regardless of how many rope edits it made.
//!
//! Within an insert session the caller may `split` the group at a word
//! boundary, a newline, or after `editor.undo_coalesce_ms` of thinking time —
//! so one `u` takes back a word rather than the whole paragraph.
//!
//! Every sealed step carries a unique, never-reused id. `state()` names the
//! position in history, which is what lets "saved" be compared across undo and
//! redo: a revision counter only ever goes up, so it cannot say that undoing
//! landed back on the version that is on disk.

use std::rc::Rc;

use super::cursor::Cursor;

#[derive(Clone)]
pub struct Change {
    /// Char index in the rope where the replaced text begins.
    pub start: usize,
    /// Text that occupied the range before.
    pub before: String,
    /// Text that occupies it now.
    pub after: String,
    /// Cursor position to restore on undo.
    pub cursor_before: Cursor,
    /// Cursor position to restore on redo.
    pub cursor_after: Cursor,
}

/// One undo step: the primitive changes of a single command, in the order they
/// were applied.
#[derive(Default, Clone)]
pub struct Step {
    pub changes: Vec<Change>,
    /// The cursor as it stood when the command that opened this step began —
    /// BEFORE any of its motions ran. `Change::cursor_before` cannot stand in
    /// for it: that is sampled inside the rope primitive, and a Visual-mode
    /// selection has already walked the cursor to the far end of the range by
    /// then. `None` for a step that no `begin_group` opened (a bare primitive,
    /// or a sub-step split out of an insert session mid-flight), where
    /// `cursor_before` is the right answer anyway.
    pub cursor_start: Option<Cursor>,
}

pub struct History {
    /// Sealed steps, each with the id it was given.
    ///
    /// Behind an `Rc` because `undo`/`redo` move a step from one stack to the
    /// other AND hand it to the caller to invert. Owning it outright meant
    /// copying every byte the step held — so taking back a `dG` copied the
    /// whole document, at the moment the user is already waiting.
    undo: Vec<(u64, Rc<Step>)>,
    redo: Vec<(u64, Rc<Step>)>,
    /// The step currently accumulating changes.
    current: Step,
    /// Anchor handed to the next step to seal, set by `begin_group` and taken
    /// by `seal`. Only the FIRST step of a group gets it — sub-steps that
    /// `split` carves off mid-insert legitimately start where typing left off.
    pending_start: Option<Cursor>,
    /// True between `begin_group` and `end_group`; changes accrue to `current`
    /// instead of sealing after each one.
    grouping: bool,
    /// The id the next sealed step will take. Never reused, so no two points in
    /// history ever compare equal.
    next_id: u64,
}

impl Default for History {
    fn default() -> Self {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            current: Step::default(),
            pending_start: None,
            grouping: false,
            next_id: 1,
        }
    }
}

impl History {
    /// Record one primitive change. Invalidates the redo stack (a fresh edit
    /// forks history). Seals immediately unless inside an explicit group.
    pub fn record(&mut self, change: Change) {
        self.redo.clear();
        self.current.changes.push(change);
        if !self.grouping {
            self.seal();
        }
    }

    /// Move the accumulating step onto the undo stack, if non-empty, taking the
    /// group's cursor anchor with it.
    fn seal(&mut self) {
        if !self.current.changes.is_empty() {
            let mut step = std::mem::take(&mut self.current);
            step.cursor_start = self.pending_start.take();
            self.undo.push((self.next_id, Rc::new(step)));
            self.next_id += 1;
        }
    }

    /// End the current step but stay grouped — the coalescing seam. Typing a
    /// space mid-insert calls this so the word just typed becomes its own undo.
    pub fn split(&mut self) {
        self.seal();
    }

    /// Where we are in history: the id of the step last applied, or the id the
    /// in-flight step will take. Two different documents never share one.
    pub fn state(&self) -> u64 {
        if !self.current.changes.is_empty() {
            return self.next_id;
        }
        self.undo.last().map(|(id, _)| *id).unwrap_or(0)
    }

    /// Open an explicit group — everything until `end_group` is one undo step.
    ///
    /// `cursor` is `Some` only at a command BOUNDARY, and then it is where the
    /// command starts. Mid-command keys (a Visual selection being dragged out,
    /// a motion completing an operator) pass `None` so the anchor set at the
    /// boundary survives to the edit — those keys are part of the same command
    /// and have already moved the cursor away from where it began.
    pub fn begin_group(&mut self, cursor: Option<Cursor>) {
        self.seal();
        if let Some(c) = cursor {
            self.pending_start = Some(c);
        }
        self.grouping = true;
    }

    pub fn end_group(&mut self) {
        self.grouping = false;
        self.seal();
    }

    /// Pop the last step for the caller to invert, moving it to the redo stack.
    pub fn undo(&mut self) -> Option<Rc<Step>> {
        self.seal();
        let entry = self.undo.pop()?;
        let step = Rc::clone(&entry.1);
        self.redo.push(entry);
        Some(step)
    }

    /// Pop the last undone step for the caller to re-apply.
    pub fn redo(&mut self) -> Option<Rc<Step>> {
        let entry = self.redo.pop()?;
        let step = Rc::clone(&entry.1);
        self.undo.push(entry);
        Some(step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(start: usize, before: &str, after: &str) -> Change {
        Change {
            start,
            before: before.to_string(),
            after: after.to_string(),
            cursor_before: Cursor::new(0, start),
            cursor_after: Cursor::new(0, start + after.chars().count()),
        }
    }

    /// Outside a group each primitive is its own step; inside one they are all
    /// the same step, which is what makes `dd` one `u`.
    #[test]
    fn a_group_is_one_step_however_many_edits_it_made() {
        let mut h = History::default();
        h.record(change(0, "", "a"));
        h.record(change(1, "", "b"));
        assert!(h.undo().is_some());
        assert!(h.undo().is_some());
        assert!(h.undo().is_none(), "two ungrouped edits are two steps");

        let mut h = History::default();
        h.begin_group(Some(Cursor::new(0, 0)));
        h.record(change(0, "", "a"));
        h.record(change(1, "", "b"));
        h.end_group();
        let step = h.undo().expect("one step");
        assert_eq!(step.changes.len(), 2);
        assert!(h.undo().is_none(), "the group was one step");
    }

    /// The anchor is where the COMMAND began, not where the last primitive
    /// found the cursor — a Visual selection has already walked it to the far
    /// end of the range by the time the rope is touched.
    #[test]
    fn the_step_remembers_where_its_command_began() {
        let mut h = History::default();
        h.begin_group(Some(Cursor::new(7, 3)));
        // A mid-command key passes None and must not overwrite the anchor.
        h.begin_group(None);
        h.record(change(0, "abc", ""));
        h.end_group();
        let step = h.undo().unwrap();
        assert_eq!(step.cursor_start, Some(Cursor::new(7, 3)));
    }

    /// Sub-steps that `split` carves out of an insert session get no anchor —
    /// `cursor_before` is the right answer for them.
    #[test]
    fn a_split_sub_step_takes_no_anchor() {
        let mut h = History::default();
        h.begin_group(Some(Cursor::new(1, 1)));
        h.record(change(0, "", "one"));
        h.split();
        h.record(change(3, "", " two"));
        h.end_group();

        let second = h.undo().unwrap();
        assert_eq!(second.cursor_start, None, "the sub-step starts where typing left off");
        let first = h.undo().unwrap();
        assert_eq!(first.cursor_start, Some(Cursor::new(1, 1)));
    }

    /// `state()` names a POSITION, not a count — that is what lets `modified`
    /// clear when you undo back to what is on disk, which a monotonic revision
    /// counter cannot express.
    #[test]
    fn undoing_back_to_a_saved_position_compares_equal() {
        let mut h = History::default();
        h.record(change(0, "", "a"));
        let saved = h.state();

        h.record(change(1, "", "b"));
        assert_ne!(h.state(), saved, "an edit moves off the saved position");

        h.undo();
        assert_eq!(h.state(), saved, "undoing lands back on it");

        h.redo();
        assert_ne!(h.state(), saved, "and redoing leaves again");
    }

    /// A fresh edit forks history: what was undone can no longer be redone.
    #[test]
    fn a_new_edit_discards_the_redo_stack() {
        let mut h = History::default();
        h.record(change(0, "", "a"));
        h.record(change(1, "", "b"));
        h.undo();
        assert!(h.redo().is_some());

        h.undo();
        h.record(change(1, "", "c"));
        assert!(h.redo().is_none(), "the forked future is gone");
    }

    /// Ids are never reused, so no two points in history compare equal even
    /// after undoing and re-editing over the same text.
    #[test]
    fn history_positions_are_never_reused() {
        let mut h = History::default();
        let mut seen = Vec::new();
        for i in 0..4 {
            h.record(change(i, "", "x"));
            seen.push(h.state());
        }
        h.undo();
        h.record(change(9, "", "y"));
        seen.push(h.state());

        let mut sorted = seen.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), seen.len(), "an id came back: {seen:?}");
    }

    /// An empty group seals nothing — pressing a motion key does not create an
    /// undo step to sit through.
    #[test]
    fn a_group_that_changed_nothing_is_not_a_step() {
        let mut h = History::default();
        h.begin_group(Some(Cursor::new(0, 0)));
        h.end_group();
        assert!(h.undo().is_none());
        assert_eq!(h.state(), 0);
    }
}
