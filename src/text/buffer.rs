//! The document. SPEC.md §4.
//!
//! Rope-backed, char-indexed. Line endings are detected on load, normalized to
//! `\n` in the rope, and reapplied on save — the file round-trips byte for byte
//! unless the user actually edited it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use ropey::Rope;

use super::cursor::Cursor;
use super::history::History;
use crate::fs::open;
use crate::fs::save::{self, SavePolicy, SaveOptions};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LineEnding {
    Lf,
    Crlf,
}

/// Which styling ruleset applies. Decided from the file extension on open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Syntax {
    /// Full Markdown: `.md`, `.markdown`
    Markdown,
    /// Reduced ruleset — no wiki links, no tags: `.txt`
    PlainProse,
    /// No styling at all.
    None,
}

pub struct Buffer {
    pub rope: Rope,
    pub cursor: Cursor,
    pub history: History,

    pub path: Option<PathBuf>,
    pub syntax: Syntax,
    pub line_ending: LineEnding,

    /// Whether the file on disk ended with a newline. Preserved on save so we
    /// never silently append one.
    pub final_newline: bool,

    /// mtime at open/save. Compared before writing to catch external edits.
    pub disk_mtime: Option<SystemTime>,

    /// Unsaved changes present. Derived from `saved_revision`, never set
    /// directly, so undoing back to the last saved state clears it.
    pub modified: bool,

    /// The history position the file on disk holds. Compared with
    /// `history.state()`, so undoing back to it clears `modified` again.
    pub saved_state: u64,

    /// Bumped on every mutation. All render caches key off this.
    pub revision: u64,

    /// The last word count taken, and the revision it was taken at.
    ///
    /// `[status] show` lists `words` by DEFAULT, and the status line is drawn
    /// every frame — so without this, moving the cursor one line re-counted
    /// every word in the document. A `Cell` rather than a field set by the
    /// edit primitives, because counting is worth doing lazily: a session that
    /// never shows the segment never pays for it at all.
    words: std::cell::Cell<Option<(u64, usize)>>,

    /// Lowest line touched since a consumer last cleared it — the seam that
    /// lets `BlockCache::invalidate_from` rescan from the edit instead of
    /// rebuilding the document. `None` means "nothing since the last take";
    /// after an undo/redo or a multi-part edit it holds the MINIMUM line, so a
    /// single rescan from there covers every change.
    pub dirty_line: Option<usize>,

    /// The file changed on disk while this buffer had unsaved work.
    ///
    /// Only ever true for a MODIFIED buffer: a clean one is reloaded the moment
    /// the change is noticed, so there is no lasting state to hold. It is
    /// therefore not "is the buffer stale" but "the two diverged and only you
    /// can say which wins" — `:w!` keeps yours, `:revert!` takes theirs.
    pub conflict: bool,

    /// Char ranges that refuse edits. Always EMPTY, and deliberately kept.
    ///
    /// It was built for transclusion, which then did not need it: an embed
    /// occupies exactly one rope line and the active line always renders raw,
    /// so the cursor can never be inside expanded content and there is nothing
    /// to protect (SPEC.md §14.5, `transclude/preview.rs`). It stays because it
    /// costs one branch in `edit.rs` and is the escape hatch if embeds ever
    /// become editable — auditing every mutation site later would not be.
    pub readonly_ranges: Vec<std::ops::Range<usize>>,
}

impl Buffer {
    pub fn empty() -> Self {
        Buffer {
            rope: Rope::new(),
            cursor: Cursor::default(),
            history: History::default(),
            path: None,
            syntax: Syntax::Markdown,
            line_ending: LineEnding::Lf,
            final_newline: true,
            disk_mtime: None,
            modified: false,
            saved_state: 0,
            revision: 0,
            dirty_line: None,
            conflict: false,
            readonly_ranges: Vec::new(),
            words: std::cell::Cell::new(None),
        }
    }

    /// UTF-8 only. Invalid bytes are refused rather than lossily converted —
    /// we are about to let the user overwrite this file. SPEC.md §10.
    ///
    /// A path that does not exist is not an error: it opens an empty buffer
    /// that will create the file on first `:w`.
    pub fn open(path: PathBuf, plain_text_exts: &[String]) -> Result<Self> {
        if !path.exists() {
            let mut b = Buffer::empty();
            b.syntax = open::syntax_for(&path, plain_text_exts);
            b.path = Some(path);
            return Ok(b);
        }
        let loaded = open::load(&path, plain_text_exts)?;
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        Ok(Buffer {
            rope: loaded.rope,
            cursor: Cursor::default(),
            history: History::default(),
            path: Some(path),
            syntax: loaded.syntax,
            line_ending: loaded.line_ending,
            final_newline: loaded.final_newline,
            disk_mtime: mtime,
            modified: false,
            saved_state: 0,
            revision: 0,
            dirty_line: None,
            conflict: false,
            readonly_ranges: Vec::new(),
            words: std::cell::Cell::new(None),
        })
    }

    /// Atomic: temp file in the same directory, fsync, rename. SPEC.md §10.
    ///
    /// `force` is `:w!` — write even though the file changed underneath us.
    pub fn save(&mut self, policy: SavePolicy, force: bool) -> Result<()> {
        let path = match &self.path {
            Some(p) => p.clone(),
            None => anyhow::bail!("no filename — use :w <path>"),
        };
        self.write_to(&path, policy, force)
    }

    pub fn save_as(&mut self, path: PathBuf, policy: SavePolicy, force: bool) -> Result<()> {
        self.write_to(&path, policy, force)?;
        self.path = Some(path);
        Ok(())
    }

    fn write_to(&mut self, path: &Path, policy: SavePolicy, force: bool) -> Result<()> {
        // Refuse to clobber a file something else has written since we read it.
        // Only meaningful when this IS the file we read: `:w other.md` records
        // an mtime for a different path, and comparing the two would refuse a
        // perfectly ordinary write. SPEC.md §10.
        let same_file = self.path.as_deref() == Some(path);
        if !force && same_file && save::changed_externally(path, self.disk_mtime) {
            anyhow::bail!("{} changed on disk — :w! to overwrite", name_of(path));
        }
        let opts = SaveOptions {
            line_ending: self.line_ending,
            // `preserve` answers with what the file arrived with; the other two
            // overrule it. This is the only place that distinction is made.
            final_newline: policy.final_newline.resolve(self.final_newline),
            trim_trailing_whitespace: policy.trim_trailing_whitespace,
        };
        let mtime = save::write_atomic(path, &self.rope, &opts)?;
        self.disk_mtime = Some(mtime);
        // Close any in-flight step first, so what was written corresponds to a
        // sealed position and the next keystroke starts a new one.
        self.history.split();
        self.saved_state = self.history.state();
        self.modified = false;
        // The write settled whichever divergence there was: this text IS the
        // file now. `:w!` is how a reader says "keep mine", so the marker has
        // to come down here rather than needing its own command.
        self.conflict = false;
        Ok(())
    }

    /// Re-read the file from disk, replacing the buffer's text.
    ///
    /// `Ok(false)` means the file's mtime moved but its CONTENT did not — a
    /// `touch`, or a writer that rewrote identical bytes. The new mtime is
    /// absorbed and nothing else happens, because throwing away the cursor to
    /// replace text with itself is a worse outcome than the stale mtime was.
    ///
    /// Two things make this safe to do behind the user's back (`App::check_disk`
    /// only ever calls it on a CLEAN buffer):
    ///
    /// - It reads through `fs::open::load`, the same function `Buffer::open`
    ///   uses. Line-ending detection, the `final_newline` flag and the syntax
    ///   choice therefore cannot drift from what opening the file would have
    ///   given — a second read path would drift, and this repo has that scar
    ///   already (preview and export once had two and the preview lied).
    /// - The replacement is ONE undo step, so `u` gives back the pre-reload
    ///   text and lands the cursor where the step began. That is what makes an
    ///   automatic reload reversible rather than merely fast.
    ///
    /// Afterwards the buffer matches disk, so it takes the same three lines a
    /// save does: seal the step, record it as the saved state, clear `modified`.
    /// Undoing the reload therefore marks the buffer modified again, which is
    /// correct — you are once more holding something the file does not have.
    pub fn reload(&mut self, plain_text_exts: &[String]) -> Result<bool> {
        let path = match &self.path {
            Some(p) => p.clone(),
            None => anyhow::bail!("no file to reload"),
        };
        let loaded = open::load(&path, plain_text_exts)?;
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());

        if loaded.rope == self.rope {
            self.disk_mtime = mtime;
            self.conflict = false;
            return Ok(false);
        }

        self.replace_all(&loaded.rope.to_string());

        self.line_ending = loaded.line_ending;
        self.final_newline = loaded.final_newline;
        self.syntax = loaded.syntax;
        self.disk_mtime = mtime;
        self.saved_state = self.history.state();
        self.modified = false;
        self.conflict = false;
        Ok(true)
    }

    /// Replace the whole document, as ONE undo step.
    ///
    /// Shared by `reload` and the `:diff` merge because both replace everything
    /// and both must be a single `u` — two copies of this would be two chances
    /// for one of them to stop grouping, and the group is the whole reason an
    /// automatic reload is safe to do behind the reader's back.
    ///
    /// The cursor is left where it was; callers clamp (`App::sync_after_input`
    /// does it for every input path anyway), and the group records the
    /// pre-replacement cursor so undo lands where the reader was.
    pub fn replace_all(&mut self, text: &str) {
        self.history.begin_group(Some(self.cursor));
        let len = self.rope.len_chars();
        if len > 0 {
            self.delete_chars(0, len);
        }
        if !text.is_empty() {
            self.insert_str(Cursor::new(0, 0), text);
        }
        self.history.end_group();
    }

    /// Recompute `modified` from the revision the disk holds. Undoing back to
    /// the saved state marks the buffer clean again, exactly as vim does.
    pub fn sync_modified(&mut self) {
        self.modified = self.history.state() != self.saved_state;
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Line content WITHOUT its trailing newline.
    pub fn line_text(&self, line: usize) -> String {
        if line >= self.rope.len_lines() {
            return String::new();
        }
        let slice = self.rope.line(line);
        let mut s = slice.to_string();
        if s.ends_with('\n') {
            s.pop();
            if s.ends_with('\r') {
                s.pop();
            }
        }
        s
    }

    /// Char count of a line, excluding its trailing newline.
    pub fn line_len(&self, line: usize) -> usize {
        if line >= self.rope.len_lines() {
            return 0;
        }
        let slice = self.rope.line(line);
        let mut n = slice.len_chars();
        if n > 0 && slice.char(n - 1) == '\n' {
            n -= 1;
            if n > 0 && slice.char(n - 1) == '\r' {
                n -= 1;
            }
        }
        n
    }

    /// Absolute char index of a cursor position.
    pub fn char_index(&self, at: Cursor) -> usize {
        let line = at.line.min(self.rope.len_lines().saturating_sub(1));
        let base = self.rope.line_to_char(line);
        (base + at.col.min(self.line_len(line))).min(self.rope.len_chars())
    }

    /// Word count over the whole buffer, for the status line.
    pub fn word_count(&self) -> usize {
        if let Some((rev, n)) = self.words.get() {
            if rev == self.revision {
                return n;
            }
        }
        let mut n = 0;
        let mut in_word = false;
        for c in self.rope.chars() {
            if c.is_whitespace() {
                in_word = false;
            } else if !in_word {
                in_word = true;
                n += 1;
            }
        }
        self.words.set(Some((self.revision, n)));
        n
    }

    pub fn display_name(&self) -> String {
        match &self.path {
            Some(p) => name_of(p),
            None => "[no name]".to_string(),
        }
    }
}

/// A path as the status line and error messages name it: the file name alone,
/// falling back to the whole path when there isn't one.
fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Words are counted on runs of non-whitespace, so punctuation and CJK
    /// come out the way a writer expects to be told.
    #[test]
    fn words_are_runs_of_non_whitespace() {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str("one two\n\nthree  four\tfive\n");
        assert_eq!(b.word_count(), 5);

        b.rope = Rope::from_str("");
        b.revision += 1;
        assert_eq!(b.word_count(), 0);

        b.rope = Rope::from_str("   \n \t \n");
        b.revision += 1;
        assert_eq!(b.word_count(), 0, "whitespace alone is no words");
    }

    /// The count is memoized against the revision. Without it the status line
    /// re-counted the whole document on every frame, including the frames where
    /// only the cursor moved.
    #[test]
    fn the_word_count_is_taken_once_per_revision() {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str("alpha beta gamma\n");
        assert_eq!(b.word_count(), 3);

        // Swap the text WITHOUT bumping the revision: the memo is expected to
        // answer, which is what proves it answered rather than re-counted.
        b.rope = Rope::from_str("one two three four five\n");
        assert_eq!(b.word_count(), 3, "same revision, memoized answer");

        b.revision += 1;
        assert_eq!(b.word_count(), 5, "a new revision re-counts");
    }

    /// An edit through the primitives bumps the revision, so the count follows
    /// the text without anyone having to invalidate it by hand.
    #[test]
    fn editing_moves_the_count_along() {
        let mut b = Buffer::empty();
        b.insert_str(Cursor::new(0, 0), "one two\n");
        assert_eq!(b.word_count(), 2);
        b.insert_str(Cursor::new(0, 7), " three");
        assert_eq!(b.word_count(), 3);
        let idx = b.rope.len_chars();
        b.delete_chars(idx.saturating_sub(6), idx);
        assert_eq!(b.word_count(), 2);
    }

    /// `line_len` and `line_text` agree, and both drop the terminator — the
    /// pair every motion and every render measurement is built on.
    #[test]
    fn a_lines_length_excludes_its_terminator() {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str("abc\nde\n");
        assert_eq!(b.line_text(0), "abc");
        assert_eq!(b.line_len(0), 3);
        assert_eq!(b.line_text(1), "de");
        assert_eq!(b.line_len(1), 2);
        // Past the end is empty rather than a panic: motions clamp against it.
        assert_eq!(b.line_text(99), "");
        assert_eq!(b.line_len(99), 0);
    }

    /// `char_index` clamps on both axes. Every edit primitive goes through it,
    /// so an out-of-range cursor must land somewhere valid rather than panic
    /// inside ropey.
    #[test]
    fn char_index_clamps_rather_than_panics() {
        let mut b = Buffer::empty();
        b.rope = Rope::from_str("abc\nde\n");
        assert_eq!(b.char_index(Cursor::new(0, 0)), 0);
        assert_eq!(b.char_index(Cursor::new(1, 1)), 5);
        assert_eq!(b.char_index(Cursor::new(0, 99)), 3, "column clamps to the line");
        assert!(b.char_index(Cursor::new(99, 99)) <= b.rope.len_chars());
    }
}
