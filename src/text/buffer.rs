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

    /// How far the file goes on below the LAST line touched since the same
    /// take (the highest-numbered one), in lines. An edit that keeps the line count (swapping two
    /// blocks, indenting) can change lines well below `dirty_line`, and a
    /// rescan that stopped as soon as its state matched again would miss
    /// them. Counted from the END because an edit above a line does not move
    /// it relative to the end, so later edits need no bookkeeping, and the
    /// rope's own line metric (`\n` only, as everywhere else) is the only
    /// one used. Read through `take_dirty`.
    pub(crate) dirty_tail: Option<usize>,

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

    /// The whole buffer refuses edits: read-only mode (`shoin -R`, `:view`).
    /// The app refuses editing commands before they get here, with a message;
    /// this is the guarantee underneath, for any path that does not.
    pub read_only: bool,
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
            dirty_tail: None,
            conflict: false,
            readonly_ranges: Vec::new(),
            read_only: false,
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
            dirty_tail: None,
            conflict: false,
            readonly_ranges: Vec::new(),
            read_only: false,
            words: std::cell::Cell::new(None),
        })
    }

    /// Text with no file behind it, read from stdin. It has no name, so `:w`
    /// asks for one, and it starts unmodified: the text still exists wherever
    /// it was piped from, so leaving without saving loses nothing, and `q`
    /// after reading should just quit. Always Markdown, since that is what
    /// gets piped into a Markdown editor.
    pub fn unnamed(bytes: Vec<u8>) -> Result<Self> {
        let loaded = open::decode(bytes, "stdin", Syntax::Markdown)?;
        let mut b = Buffer::empty();
        b.rope = loaded.rope;
        b.line_ending = loaded.line_ending;
        b.final_newline = loaded.final_newline;
        Ok(b)
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
        let same_file = self.path.as_deref().is_some_and(|ours| same_path(ours, path));
        if !force && same_file && save::changed_externally(path, self.disk_mtime) {
            anyhow::bail!("{} changed on disk — :w! to overwrite", name_of(path));
        }
        // Nor one that is not ours at all: `:w notes.md` over an existing
        // notes.md would destroy it. vim's E13, and `:w!` the same way past.
        if !force && !same_file && path.exists() {
            anyhow::bail!("{} exists — :w! to overwrite", name_of(path));
        }
        let mtime = save::write_atomic(path, &self.rope, &self.save_options(policy))?;
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

    /// How this buffer's text goes to disk under `policy`.
    fn save_options(&self, policy: SavePolicy) -> SaveOptions {
        SaveOptions {
            line_ending: self.line_ending,
            // `preserve` answers with what the file arrived with; the other two
            // overrule it. This is the only place that distinction is made.
            final_newline: policy.final_newline.resolve(self.final_newline),
            trim_trailing_whitespace: policy.trim_trailing_whitespace,
        }
    }

    /// `:w other.md` on a buffer that has a file: write the text there as a
    /// COPY, and change nothing about the buffer — its path, `modified`, the
    /// saved state and the mtime all still describe its own file, as vim's
    /// `:w {file}` leaves them. `:saveas` is the command that switches.
    ///
    /// An existing file is refused without `force`, as `write_to` refuses it.
    pub fn write_copy(&self, path: &Path, policy: SavePolicy, force: bool) -> Result<()> {
        if !force && path.exists() {
            anyhow::bail!("{} exists — :w! to overwrite", name_of(path));
        }
        save::write_atomic(path, &self.rope, &self.save_options(policy))?;
        Ok(())
    }

    /// Whether `path` is this buffer's own file, by any spelling.
    pub fn is_own_file(&self, path: &Path) -> bool {
        self.path.as_deref().is_some_and(|ours| same_path(ours, path))
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
            // Same text, but perhaps not the same bytes: a tool that turned
            // LF into CRLF leaves the rope identical, and keeping the old
            // ending would turn it back on the next save.
            self.line_ending = loaded.line_ending;
            self.final_newline = loaded.final_newline;
            self.disk_mtime = mtime;
            self.conflict = false;
            return Ok(false);
        }

        // Taking the file's text is not an edit by the reader, so read-only
        // mode (where watching a file is the point) must not refuse it. The
        // flag is lifted for this one replacement only. Anything else that
        // refuses leaves the text, and the old mtime, so the next check
        // tries again rather than reporting a reload that did not happen.
        let read_only = std::mem::replace(&mut self.read_only, false);
        let replaced = self.replace_all(&loaded.rope.to_string());
        self.read_only = read_only;
        if !replaced {
            anyhow::bail!("{} could not be reloaded — part of it refuses edits", name_of(&path));
        }

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
    ///
    /// Returns false, having changed nothing, when the buffer refuses edits
    /// (read-only mode or a read-only range), so no caller can report a
    /// replacement that did not happen.
    pub fn replace_all(&mut self, text: &str) -> bool {
        let len = self.rope.len_chars();
        if !self.is_editable(&(0..len)) {
            return false;
        }
        self.history.begin_group(Some(self.cursor));
        if len > 0 {
            self.delete_chars(0, len);
        }
        if !text.is_empty() {
            self.insert_str(Cursor::new(0, 0), text);
        }
        self.history.end_group();
        true
    }

    /// Recompute `modified` from the revision the disk holds. Undoing back to
    /// the saved state marks the buffer clean again, exactly as vim does.
    pub fn sync_modified(&mut self) {
        self.modified = self.history.state() != self.saved_state;
    }

    /// Lines as ropey counts them, which is by `\n` alone: ropey is built
    /// without its `unicode_lines` feature, so `\r`, U+2028 and the other
    /// Unicode breaks are ordinary characters inside a line, as they are to
    /// `str::lines`, `line_text` and the renderer.
    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// The last line the cursor can rest on outside Insert mode — vim's last
    /// line.
    ///
    /// Ropey counts the empty string after a trailing newline as a line of its
    /// own, so `line_count() - 1` is one past the end for any file ending in
    /// `\n`. That line is not part of the document: saving writes the same
    /// bytes whether or not it is there. Insert mode may still sit on it (an
    /// Enter at the end of the text lands there), so `line_count` keeps
    /// ropey's meaning and this is what motions and clamps use instead.
    pub fn last_line(&self) -> usize {
        let n = self.rope.len_lines();
        let len = self.rope.len_chars();
        if n > 1 && self.rope.char(len - 1) == '\n' {
            n - 2
        } else {
            n - 1
        }
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

/// Whether two spellings name one file: `notes.md` and `./notes.md` do, and
/// so does a symlink and what it points at.
fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::cursor::Cursor;

    /// `take_dirty` brackets every line a run of edits touched, in the text
    /// as it now stands, by the rope's own line breaks.
    #[test]
    fn dirty_lines_bracket_every_edit() {
        let text = b"0\n1\n2\n3\n4\n5\n".to_vec();
        let mut b = Buffer::unnamed(text.clone()).unwrap();
        b.insert_str(Cursor::new(4, 0), "x\ny\n"); // lines 4..=6
        assert_eq!(b.take_dirty(), Some((4, 6)));
        assert_eq!(b.take_dirty(), None, "taken");

        // An edit above an earlier one moves it down; the range follows.
        b.insert_str(Cursor::new(4, 0), "x\ny\n");
        b.insert_str(Cursor::new(1, 0), "new\n");
        assert_eq!(b.take_dirty(), Some((1, 7)));

        // A delete above it moves it up.
        let mut b = Buffer::unnamed(text.clone()).unwrap();
        b.insert_str(Cursor::new(4, 0), "x\ny\n");
        let at = b.rope.line_to_char(2);
        b.delete_chars(at, at + 2);
        assert_eq!(b.take_dirty(), Some((2, 5)));

        // U+2028 is not a break: the edit stays on one line.
        let mut b = Buffer::unnamed(text.clone()).unwrap();
        b.insert_str(Cursor::new(1, 0), "p\u{2028}q");
        assert_eq!(b.take_dirty(), Some((1, 1)));

        // Two edits far apart that keep the line count between them: every
        // line from the first to the second is in the range.
        let ten = b"0\n1\n2\n3\n4\n5\n6\n7\n8\n9\n".to_vec();
        let mut b = Buffer::unnamed(ten).unwrap();
        b.insert_str(Cursor::new(3, 0), "new\n");
        let at = b.rope.line_to_char(8);
        b.delete_chars(at, at + 2);
        assert_eq!(b.take_dirty(), Some((3, 8)));
    }

    /// Undo and redo write the rope directly; they record the whole span of
    /// what they put back, not just where it starts.
    #[test]
    fn undo_and_redo_record_what_they_put_back() {
        let mut b = Buffer::unnamed(b"0\n1\n2\n3\n4\n5\n".to_vec()).unwrap();
        b.history.begin_group(None);
        let at = b.rope.line_to_char(2);
        b.delete_chars(at, at + 4); // lines "2" and "3"
        b.history.end_group();
        b.take_dirty();
        b.undo();
        assert_eq!(b.take_dirty(), Some((2, 4)), "undo put two lines back");

        let mut b = Buffer::unnamed(b"0\n1\n2\n".to_vec()).unwrap();
        b.history.begin_group(None);
        b.insert_str(Cursor::new(1, 0), "x\ny\n");
        b.history.end_group();
        b.undo();
        b.take_dirty();
        b.redo();
        assert_eq!(b.take_dirty(), Some((1, 3)), "redo put two lines back");
    }

    /// Deleting the `\n` of a `\r\n` leaves a lone `\r`, which is an
    /// ordinary character: the two lines join, and the first line touched
    /// is the one the `\r` is on.
    #[test]
    fn a_lone_carriage_return_keeps_the_first_line() {
        let mut b = Buffer::empty();
        b.rope = ropey::Rope::from_str("a\r\nb\nc\n");
        assert_eq!(b.rope.len_lines(), 4);
        b.delete_chars(2, 3);
        assert_eq!(b.rope.to_string(), "a\rb\nc\n");
        assert_eq!(b.rope.len_lines(), 3, "the lone \\r does not break the line");
        assert_eq!(b.line_text(0), "a\rb");
        assert_eq!(b.take_dirty(), Some((0, 0)));
    }

    /// Piped text has no name and nothing unsaved, and keeps its line ending
    /// for the `:w` that names it.
    #[test]
    fn piped_text_is_unnamed_and_clean() {
        let b = Buffer::unnamed(b"# Title\r\nBody\r\n".to_vec()).unwrap();
        assert!(b.path.is_none());
        assert!(!b.modified);
        assert_eq!(b.syntax, Syntax::Markdown);
        assert_eq!(b.line_ending, LineEnding::Crlf);
        assert_eq!(b.line_text(0), "# Title");

        let err = Buffer::unnamed(b"ok\xff".to_vec()).err().expect("refused");
        assert!(err.to_string().starts_with("stdin is not valid UTF-8"), "{err}");
    }

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

    /// A trailing newline ends the last line rather than opening another, so
    /// `last_line` is one short of ropey's count exactly when the text ends
    /// in `\n` — and never below line 0.
    #[test]
    fn the_last_line_ignores_the_empty_string_after_a_trailing_newline() {
        let mut b = Buffer::empty();
        let cases = [
            ("", 0),
            ("\n", 0),
            ("one", 0),
            ("one\ntwo", 1),
            ("one\ntwo\n", 1),
            ("one\n\n", 1),
        ];
        for (text, last) in cases {
            b.rope = Rope::from_str(text);
            assert_eq!(b.last_line(), last, "{text:?}");
        }
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

    /// A fresh, empty directory under the system temp dir.
    fn dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shoin-buf-{tag}-{t}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn policy() -> SavePolicy {
        SavePolicy {
            trim_trailing_whitespace: false,
            final_newline: save::FinalNewline::Preserve,
        }
    }

    /// Writes `bytes` with an mtime an hour back, so the next write is
    /// certain to move it.
    fn write_old(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(past)
            .unwrap();
    }

    /// Read-only mode is where a file is watched, so a reload there must
    /// take the new text. It used to keep the old text, record the new
    /// mtime and report success. F02.
    #[test]
    fn a_read_only_buffer_reloads_the_new_text() {
        let d = dir("ro-reload");
        let path = d.join("n.md");
        write_old(&path, b"one\n");
        let mut b = Buffer::open(path.clone(), &[]).unwrap();
        b.read_only = true;
        let before = b.disk_mtime;

        std::fs::write(&path, b"two\nthree\n").unwrap();
        assert!(b.reload(&[]).unwrap(), "reported as replaced");
        assert_eq!(b.rope.to_string(), "two\nthree\n");
        assert!(b.read_only, "still read-only");
        assert!(!b.modified);
        assert_ne!(b.disk_mtime, before);

        // The reader cannot undo while reading.
        assert!(!b.undo());
        assert_eq!(b.rope.to_string(), "two\nthree\n");

        // Leaving read-only: clean, and `u` takes the reload back in one step.
        b.read_only = false;
        assert!(!b.modified);
        assert!(b.undo());
        assert_eq!(b.rope.to_string(), "one\n");
        assert!(b.modified, "holding text the file does not have");
        assert!(b.redo());
        assert_eq!(b.rope.to_string(), "two\nthree\n");
        assert!(!b.modified, "back at what the disk holds");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A read-only reload takes the file's line ending and final newline
    /// too, so the next save writes the file's own bytes.
    #[test]
    fn a_read_only_reload_keeps_crlf_and_a_missing_final_newline() {
        let d = dir("ro-crlf");
        let path = d.join("n.md");
        write_old(&path, b"one\n");
        let mut b = Buffer::open(path.clone(), &[]).unwrap();
        b.read_only = true;

        std::fs::write(&path, b"a\r\nb\r\nc").unwrap();
        assert!(b.reload(&[]).unwrap());
        assert_eq!(b.rope.to_string(), "a\nb\nc");
        assert_eq!(b.line_ending, LineEnding::Crlf);
        assert!(!b.final_newline);

        b.read_only = false;
        b.save(policy(), false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"a\r\nb\r\nc");
        std::fs::remove_dir_all(&d).ok();
    }

    /// `replace_all` says whether it replaced, and a refusal changes
    /// nothing: no text, no history step, no half-done group.
    #[test]
    fn replace_all_reports_a_refusal() {
        let mut b = Buffer::unnamed(b"old\n".to_vec()).unwrap();
        b.read_only = true;
        assert!(!b.replace_all("new\n"));
        assert_eq!(b.rope.to_string(), "old\n");

        b.read_only = false;
        b.readonly_ranges.push(1..2);
        assert!(!b.replace_all("new\n"));
        assert_eq!(b.rope.to_string(), "old\n");
        assert!(!b.undo(), "nothing was recorded");

        b.readonly_ranges.clear();
        assert!(b.replace_all("new\n"));
        assert_eq!(b.rope.to_string(), "new\n");
        assert!(b.undo());
        assert_eq!(b.rope.to_string(), "old\n", "one step");
    }

    /// A reload that cannot replace the text is an error, and leaves the
    /// mtime alone so the next check tries again.
    #[test]
    fn a_refused_reload_is_an_error_and_keeps_the_old_mtime() {
        let d = dir("refused");
        let path = d.join("n.md");
        write_old(&path, b"one\n");
        let mut b = Buffer::open(path.clone(), &[]).unwrap();
        b.readonly_ranges.push(0..1);
        let before = b.disk_mtime;

        std::fs::write(&path, b"two\n").unwrap();
        assert!(b.reload(&[]).is_err());
        assert_eq!(b.rope.to_string(), "one\n");
        assert_eq!(b.disk_mtime, before);
        std::fs::remove_dir_all(&d).ok();
    }

    /// Characters other text tools treat as line breaks. Shoin breaks lines
    /// on `\n` only, so each of these sits inside its line. F05.
    const NOT_BREAKS: [&str; 6] = ["\u{2028}", "\u{2029}", "\u{85}", "\x0b", "\x0c", "\r"];

    /// One line holding the character is one line: the line count, its
    /// length and its text agree with what the screen shows, and typing at
    /// the end of it lands at the real end, not after the character.
    #[test]
    fn only_a_newline_breaks_a_line() {
        for sep in NOT_BREAKS {
            let text = format!("abc{sep}def\n");
            let mut b = Buffer::unnamed(text.clone().into_bytes()).unwrap();
            assert_eq!(b.line_count(), 2, "{sep:?}: the line and the empty one after it");
            assert_eq!(b.last_line(), 0, "{sep:?}");
            assert_eq!(b.line_text(0), format!("abc{sep}def"), "{sep:?}");
            assert_eq!(b.line_len(0), 7, "{sep:?}");

            // What `A` then `Q` does: insert at the end of the line.
            let end = Cursor::new(0, b.line_len(0));
            assert!(b.insert_str(end, "Q"));
            assert_eq!(b.rope.to_string(), format!("abc{sep}defQ\n"), "{sep:?}");
            assert_eq!(b.rope.char_to_line(7), 0, "{sep:?}: Q is on line 0");

            // What `dd` on the line removes: all of it.
            let to = b.rope.line_to_char(1);
            b.delete_chars(0, to);
            assert_eq!(b.rope.to_string(), "", "{sep:?}");
        }
    }

    /// The characters are kept as they are on load and written back as they
    /// were, in an LF file and in a CRLF one.
    #[test]
    fn a_file_with_them_saves_byte_for_byte() {
        let d = dir("seps");
        for (i, sep) in NOT_BREAKS.iter().enumerate() {
            for (j, eol) in ["\n", "\r\n"].iter().enumerate() {
                let bytes = format!("abc{sep}def{eol}two{eol}");
                let path = d.join(format!("f{i}-{j}.md"));
                std::fs::write(&path, &bytes).unwrap();
                let mut b = Buffer::open(path.clone(), &[]).unwrap();
                assert_eq!(b.line_count(), 3, "{sep:?} {eol:?}");
                assert_eq!(b.line_text(0), format!("abc{sep}def"), "{sep:?} {eol:?}");
                assert_eq!(b.line_text(1), "two", "{sep:?} {eol:?}");
                b.save(policy(), false).unwrap();
                assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes, "{sep:?} {eol:?}");
            }
        }
        std::fs::remove_dir_all(&d).ok();
    }
}

/// How the screen shows the characters that no longer break a line. Here
/// rather than in `render/` because it guards what changing the rope's line
/// metric (F05) let through to the renderer; it only reads the drawn frame.
#[cfg(test)]
mod drawn {
    use crate::app::App;
    use crate::config::Config;
    use crate::render::markdown::block::BlockCache;
    use crate::text::cursor::Cursor;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// A control character inside a line is drawn as nothing, never sent to
    /// the terminal, and the caret still sits on the character it is on.
    #[test]
    fn control_characters_inside_a_line_draw_as_nothing() {
        for sep in ["\u{85}", "\x0b", "\x0c", "\r", "\x07", "\x1b"] {
            let mut app = App::new(Config::default(), None, None).unwrap();
            app.buffer.insert_str(Cursor::new(0, 0), &format!("abc{sep}def\n"));
            app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math);
            app.buffer.cursor = Cursor::new(0, 6); // on the `f`
            let mut t = Terminal::new(TestBackend::new(40, 6)).unwrap();
            t.draw(|f| crate::render::frame::render(f, &app)).unwrap();
            let caret = t.get_cursor_position().unwrap();
            let buf = t.backend().buffer();
            let row: Vec<&str> = (0..buf.area.width).map(|x| buf[(x, caret.y)].symbol()).collect();
            assert!(
                !row.iter().any(|s| s.chars().any(char::is_control)),
                "{sep:?} reached the screen: {row:?}"
            );
            assert_eq!(row.concat().trim(), "abcdef", "{sep:?}");
            assert_eq!(row[caret.x as usize], "f", "{sep:?}: the caret is on the f");
        }
    }
}
