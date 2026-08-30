//! The per-line render cache. SPEC.md §9 ("draw only what changed").
//!
//! Rendering a line is expensive: an inline scan, a styled-span cover, a
//! conceal map, and a soft wrap. None of it depends on the frame — only on the
//! line's own text, its block kind, the theme/config, and the measure. So it is
//! computed once per line and kept until one of those changes.
//!
//! THE VALIDITY RULE, in order of cost:
//!   1. Same buffer revision, style key and measure → nothing to check at all.
//!   2. Otherwise every entry is VERIFIED against its source line by comparing
//!      chars straight off the rope (no allocation). Verification is what makes
//!      the cache correct; the splice below only makes it fast.
//!   3. Before verifying, entries are SPLICED to follow the edit: an edit that
//!      added or removed lines at line `d` shifts every later entry by the same
//!      delta, so moving them keeps them matching their (renumbered) source
//!      instead of re-parsing the rest of the document. If the splice guesses
//!      wrong, step 2 catches it — the entries simply miss and get rebuilt.
//!
//! What is NOT cached here is anything frame-local: focus dim, the search
//! highlight, the Visual selection, the command-box dim. Those are overlays
//! applied to a clone of the cached spans, for VISIBLE lines only — which is
//! what keeps the expensive work O(viewport) while the row index stays O(lines).

use crate::app::{App, BufferState};
use crate::config::schema::{GlyphConfig, MarkdownConfig};
use crate::render::conceal::{ConcealCtx, ConcealMap};
use crate::render::indent::{self, guides_for};
use crate::render::layout::{content_column, wrap_hanging, RowSource, VisualRow};
use crate::render::markdown::{self, block::BlockKind};
use crate::render::theme::Theme;
use crate::transclude::preview::Expansion;
use crate::render::StyledSpan;
use crate::text::buffer::{Buffer, Syntax};

/// One line's cached parse: everything that survives between frames.
pub struct LineEntry {
    /// The source text this entry was built from — the verification key.
    pub source: String,
    /// The block kind it was built under; a change here re-parses the line.
    pub block: BlockKind,
    /// Base styled spans in SOURCE coordinates, before any frame overlay.
    pub styled: Vec<StyledSpan>,
    /// The line's conceal map (empty ops when nothing conceals).
    pub cmap: ConcealMap,
    /// Wrap ranges of `source` at the cached measure.
    wrap_raw: Vec<(usize, usize)>,
    /// Wrap ranges of `concealed` at the cached measure.
    wrap_concealed: Vec<(usize, usize)>,
    /// Hanging indent for continuation rows, in each form. Zero unless this is
    /// a list item and `layout.hanging_indent` is on — and the two differ,
    /// because a concealed `- [ ] ` is narrower than the revealed one.
    hang_raw: u16,
    hang_concealed: u16,
    /// What this line draws instead of itself when it is a whole-line `![[…]]`
    /// and preview is on. Captured when the line is parsed, so the target is
    /// read once per edit rather than once per frame.
    pub embed: Option<Expansion>,
}

impl LineEntry {
    /// The wrap ranges for whichever form this line takes.
    fn wrap(&self, concealed: bool) -> &[(usize, usize)] {
        if concealed {
            &self.wrap_concealed
        } else {
            &self.wrap_raw
        }
    }

    /// The hanging indent for whichever form this line takes.
    fn hang(&self, concealed: bool) -> u16 {
        if concealed {
            self.hang_concealed
        } else {
            self.hang_raw
        }
    }

    /// True when the entry still describes `line` of `buffer`, compared without
    /// allocating a `String` per line.
    fn matches(&self, buffer: &Buffer, line: usize, block: &BlockKind) -> bool {
        if &self.block != block {
            return false;
        }
        let slice = buffer.rope.line(line);
        let mut n = slice.len_chars();
        if n > 0 && slice.char(n - 1) == '\n' {
            n -= 1;
            if n > 0 && slice.char(n - 1) == '\r' {
                n -= 1;
            }
        }
        // Compare the SLICE against the string, not char against char. Same
        // answer, but ropey walks whole chunks where the char iterators walked
        // one scalar at a time — and this runs for every line of the document
        // on every edit, which is what makes the constant worth caring about.
        slice.slice(..n) == self.source.as_str()
    }
}

/// Everything outside a line's own text that changes how it parses. Compared
/// per sync; a mismatch drops the whole cache.
#[derive(Clone, PartialEq)]
struct StyleKey {
    theme: Theme,
    markdown: MarkdownConfig,
    glyphs: GlyphConfig,
    tab_width: usize,
    syntax: Syntax,
    /// Changing `:embed` changes what every `![[…]]` line draws, so it
    /// invalidates the parse the same way a theme change does.
    embed_mode: crate::transclude::Mode,
    /// A list item's hanging indent is measured INTO the entry, so turning the
    /// setting off has to re-measure it. Missing from this key until
    /// 2026-08-20, and `matches()` could never have caught it — that compares
    /// the source text and the block kind, and neither of them moves when a
    /// layout setting does.
    hanging_indent: bool,
    /// Reaches an embed's expansion: the search root, the depth cap, the
    /// heading offset and the border are all read while the line is parsed.
    transclude: crate::config::schema::TranscludeConfig,
}

impl StyleKey {
    /// Compare against the app WITHOUT cloning — this runs every sync, while
    /// `of` only runs on the rare frame where something actually changed.
    fn matches(&self, app: &App, doc: &BufferState) -> bool {
        self.theme == app.theme
            && self.markdown == app.config.markdown
            && self.glyphs == app.config.glyphs
            && self.tab_width == app.config.editor.tab_width
            && self.syntax == doc.buffer.syntax
            && self.embed_mode == app.embed_mode
            && self.hanging_indent == app.config.layout.hanging_indent
            && self.transclude == app.config.transclude
    }

    fn of(app: &App, doc: &BufferState) -> Self {
        StyleKey {
            theme: app.theme.clone(),
            markdown: app.config.markdown.clone(),
            glyphs: app.config.glyphs.clone(),
            tab_width: app.config.editor.tab_width,
            syntax: doc.buffer.syntax,
            embed_mode: app.embed_mode,
            hanging_indent: app.config.layout.hanging_indent,
            transclude: app.config.transclude.clone(),
        }
    }
}

#[derive(Default)]
pub struct RenderCache {
    entries: Vec<Option<LineEntry>>,
    key: Option<StyleKey>,
    measure: u16,
    revision: u64,
    /// Whether each line took its concealed form in the current row index.
    concealed: Vec<bool>,
    /// Whether concealment is on at all this frame (`layout.conceal`, Markdown).
    conceal_on: bool,
    /// First visual row of each line, plus a final total-rows sentinel. Always
    /// `entries.len() + 1` long after a sync.
    row_starts: Vec<usize>,
    /// The buffer revision at which every entry was last verified against its
    /// line. `None` means the entries and the document may have diverged, which
    /// is the only state that has to pay for the verification walk.
    verified: Option<u64>,
    /// Lines parsed since startup. Instrumentation: the whole point of this
    /// type is that this number grows with EDITS, not with frames or lines.
    builds: u64,
    /// Verification walks since startup — rule 1 above says this grows with
    /// EDITS, not with frames, and the instrumentation is here so a test can
    /// hold it to that.
    verifications: u64,
}

impl RenderCache {
    /// Bring the cache in line with the buffer and rebuild the row index.
    /// Everything else on this type reads what this leaves behind.
    /// Sync against the CURRENT document. Panes showing another one call
    /// `sync_doc` directly.
    pub fn sync(&mut self, app: &App, measure: u16) {
        let doc = &app.docs[app.current()];
        self.sync_doc(app, doc, measure);
    }

    /// Bring the cache in step with `doc` at `measure`.
    ///
    /// `doc` is passed rather than read off `app` because a pane may be showing
    /// a document that is not the current one — and then nothing in it is
    /// "active", so every line conceals.
    pub fn sync_doc(&mut self, app: &App, doc: &BufferState, measure: u16) {
        let n = doc.buffer.line_count();

        if !self.key.as_ref().is_some_and(|k| k.matches(app, doc)) {
            self.entries.clear();
            self.verified = None;
            self.key = Some(StyleKey::of(app, doc));
        }
        if self.measure != measure {
            // The measure reaches concealment itself — a fence delimiter hides
            // behind a rule as wide as the text — so a resize invalidates the
            // parse, not just the wrap. Resizes are rare; this keeps one rule.
            self.entries.clear();
            self.verified = None;
            self.measure = measure;
        }

        if self.revision != doc.buffer.revision {
            self.splice(doc, n);
            self.revision = doc.buffer.revision;
            self.verified = None;
        }

        // RULE 1, and it is worth stating why it is safe rather than only that
        // it is fast. An entry is a function of the line's text, its block kind,
        // the style key and the measure. The last two have just been checked;
        // the block kinds are rebuilt only when the buffer revision moves
        // (`App::refresh_blocks` returns early otherwise), and so is the text.
        // So at an unchanged revision there is nothing left that could have
        // moved, and the walk below can only confirm what it already knows.
        //
        // This matters more than it looks: without it, a cursor key on a 512 KB
        // document compared every character of every line off the rope before
        // drawing a single row — ~100 ms of work to move one line.
        let stale = self.verified != Some(doc.buffer.revision) || self.entries.len() != n;

        self.entries.resize_with(n, || None);
        if stale {
            // `BlockKind` is cloned only where it is kept. Cloning it per line
            // to compare it allocated a String for every fence line in the
            // document, on every frame.
            static PARAGRAPH: BlockKind = BlockKind::Paragraph;
            for line in 0..n {
                let block = doc.blocks.kinds.get(line).unwrap_or(&PARAGRAPH);
                let valid = self.entries[line]
                    .as_ref()
                    .is_some_and(|e| e.matches(&doc.buffer, line, block));
                if !valid {
                    self.entries[line] =
                        Some(build(app, doc, line, block.clone(), measure));
                    self.builds += 1;
                }
            }
            self.verified = Some(doc.buffer.revision);
            self.verifications += 1;
        }

        self.index(app, doc, n);
    }

    /// Move entries to follow an edit's line shift, so the lines below an
    /// inserted or deleted line stay cached. `App::take_render_dirty` reports
    /// the lowest line touched since the last sync; without it (a wholesale
    /// change) nothing is spliced and verification re-parses what moved.
    fn splice(&mut self, doc: &BufferState, new_len: usize) {
        let Some(from) = doc.take_render_dirty() else {
            return;
        };
        let old_len = self.entries.len();
        if from >= old_len || old_len == 0 {
            return;
        }
        match new_len.cmp(&old_len) {
            std::cmp::Ordering::Greater => {
                let added = new_len - old_len;
                self.entries
                    .splice(from..from, std::iter::repeat_with(|| None).take(added));
                // The edited line itself, and every line it grew into, are new.
                for slot in self.entries.iter_mut().skip(from).take(added + 1) {
                    *slot = None;
                }
            }
            std::cmp::Ordering::Less => {
                let removed = (old_len - new_len).min(old_len - from);
                self.entries.drain(from..from + removed);
                if let Some(slot) = self.entries.get_mut(from) {
                    *slot = None;
                }
            }
            std::cmp::Ordering::Equal => {
                if let Some(slot) = self.entries.get_mut(from) {
                    *slot = None;
                }
            }
        }
    }

    /// Rebuild the line → first-row index for this frame's active set. Integer
    /// work only, so a cursor move that re-reveals a line costs O(lines) adds
    /// rather than a re-parse.
    fn index(&mut self, app: &App, doc: &BufferState, n: usize) {
        let conceal_on = app.config.layout.conceal && doc.buffer.syntax == Syntax::Markdown;
        let spacing = app.config.layout.line_spacing.min(crate::render::layout::MAX_LINE_SPACING) as usize;
        // Only the document under the cursor has an active (raw) line. A pane
        // showing another one conceals throughout — there is no cursor in it to
        // reveal anything.
        let current = std::ptr::eq(doc, &app.docs[app.current()]);
        self.conceal_on = conceal_on;
        self.concealed.clear();
        self.concealed.reserve(n);
        self.row_starts.clear();
        self.row_starts.reserve(n + 1);

        let mut rows = 0usize;
        for line in 0..n {
            let concealed = conceal_on && !(current && app.active.contains(line));
            self.concealed.push(concealed);
            self.row_starts.push(rows);
            // An embed expands only when it is NOT the active line — SPEC §2's
            // rule, applied to embeds exactly as it applies to `**bold**`.
            // That is also what keeps the cursor from ever being inside one.
            rows += content_rows(self.entries[line].as_ref(), concealed);
            rows += spacer_rows(spacing, doc, line, n);
        }
        self.row_starts.push(rows);
    }

    /// Total visual rows in the document, as currently revealed.
    pub fn total_rows(&self) -> usize {
        self.row_starts.last().copied().unwrap_or(0)
    }

    pub fn entry(&self, line: usize) -> Option<&LineEntry> {
        self.entries.get(line).and_then(|e| e.as_ref())
    }

    /// Whether concealment applies at all this frame. With it off there is no
    /// reveal, so nothing needs a gutter reserved for one.
    pub fn conceal_on(&self) -> bool {
        self.conceal_on
    }

    /// Whether `line` renders in its concealed form this frame.
    pub fn is_concealed(&self, line: usize) -> bool {
        self.concealed.get(line).copied().unwrap_or(false)
    }

    /// The visual row at global index `row` — a binary search over the line
    /// index, so drawing a viewport never walks the document.
    pub fn row(&self, row: usize) -> Option<VisualRow> {
        if row >= self.total_rows() {
            return None;
        }
        // `partition_point` gives the first line whose start row exceeds `row`;
        // the line we want is the one before it.
        let line = self.row_starts.partition_point(|&s| s <= row) - 1;
        let within = row - self.row_starts[line];
        let entry = self.entry(line)?;
        let concealed = self.is_concealed(line);
        // Line spacing's rows trail the line's own, so anything past its
        // content is air. It answers with the line it follows, which is what
        // makes a click in the gap land somewhere sensible.
        if within >= content_rows(Some(entry), concealed) {
            return Some(VisualRow {
                source: RowSource::Spacer(line),
                start_col: 0,
                end_col: 0,
                hanging: 0,
                editable: false,
            });
        }
        if concealed {
            if let Some(x) = &entry.embed {
                if within < x.len() {
                    return Some(VisualRow {
                        source: RowSource::Embedded { line, index: within },
                        start_col: 0,
                        end_col: 0,
                        hanging: 0,
                        editable: false,
                    });
                }
            }
        }
        let (start_col, end_col) = *entry.wrap(concealed).get(within)?;
        Some(VisualRow {
            source: RowSource::Buffer(line),
            start_col,
            end_col,
            // The first row carries the marker; only what follows hangs.
            hanging: if within == 0 { 0 } else { entry.hang(concealed) },
            editable: true,
        })
    }

    /// Which visual row holds a source position, and the column offset within
    /// it. Columns index the DISPLAY text, which for the cursor's own line —
    /// always in the active set — equals the source.
    pub fn locate(&self, line: usize, col: usize) -> (usize, usize) {
        let Some(entry) = self.entry(line) else {
            return (0, 0);
        };
        let base = self.row_starts.get(line).copied().unwrap_or(0);
        let wrap = entry.wrap(self.is_concealed(line));
        for (i, (start, end)) in wrap.iter().enumerate() {
            if col >= *start && col <= *end {
                return (base + i, col - start);
            }
        }
        (base + wrap.len().saturating_sub(1), 0)
    }
}

/// How many rows a line's own content occupies, in the form it is taking.
fn content_rows(entry: Option<&LineEntry>, concealed: bool) -> usize {
    match entry {
        Some(e) if concealed => match &e.embed {
            Some(x) => x.len(),
            None => e.wrap(true).len(),
        },
        Some(e) => e.wrap(false).len(),
        None => 1,
    }
}

/// Blank rows to leave after `line` — `layout.line_spacing`, minus the places
/// where air reads as damage rather than as breathing room. SPEC.md §6.
fn spacer_rows(spacing: usize, doc: &BufferState, line: usize, n: usize) -> usize {
    // Nothing trails the last line: spacing is what separates two lines, and
    // below the last one there is no second line to separate from.
    if spacing == 0 || line + 1 >= n {
        return 0;
    }
    match doc.blocks.kinds.get(line) {
        // A blank line is ALREADY a gap. Spacing it as well would make every
        // paragraph break twice the size of the setting, which is the objection
        // that kept this feature unbuilt: Markdown source is not double-spaced
        // prose, it is prose with blank lines in it.
        Some(BlockKind::Blank) => 0,
        // A fence is a slab and a table is a grid. Both are shapes the reader
        // measures by eye, and a row of air inside one breaks the shape.
        Some(BlockKind::FenceOpen(_) | BlockKind::FenceBody { .. } | BlockKind::FrontMatter) => 0,
        Some(BlockKind::Table) => match doc.blocks.kinds.get(line + 1) {
            Some(BlockKind::Table) => 0,
            _ => spacing,
        },
        _ => spacing,
    }
}

/// Parse and wrap one line. The single scan is handed to both the styler and
/// the conceal map — the two used to scan the line independently.
fn build(app: &App, doc: &BufferState, line: usize, block: BlockKind, measure: u16) -> LineEntry {
    let cfg = &app.config;
    let source = doc.buffer.line_text(line);

    // `[markdown] tables = false` opts out of table rendering; the classifier
    // stays as it is and the line simply renders as the prose it also is.
    let block = match block {
        BlockKind::Table if !cfg.markdown.tables => BlockKind::Paragraph,
        other => other,
    };

    let inline = if block.allows_inline() {
        markdown::inline::scan(&source, &cfg.markdown)
    } else {
        Vec::new()
    };
    let mut styled = markdown::style_line_with(
        &source,
        block.clone(),
        doc.buffer.syntax,
        &app.theme,
        &cfg.markdown,
        &inline,
    );
    let guides = guides_for(&source, &block, cfg.editor.tab_width);
    indent::apply(&mut styled, &guides, &app.theme, &cfg.glyphs);

    let cmap = ConcealMap::build(
        &source,
        &block,
        &inline,
        &ConcealCtx {
            glyphs: &cfg.glyphs,
            theme: &app.theme,
            measure,
            tab_width: cfg.editor.tab_width,
        },
    );
    let concealed = cmap.display_text(&source);

    // SPEC §6: a wrapped list item's continuation lines up under its content.
    // Measured on each DISPLAY form separately — the marker it hangs off is a
    // different width revealed than concealed.
    let (hang_raw, hang_concealed) = if cfg.layout.hanging_indent
        && matches!(block, BlockKind::ListItem { .. })
    {
        let tw = cfg.editor.tab_width as u16;
        (content_column(&source, tw), content_column(&concealed, tw))
    } else {
        (0, 0)
    };

    // A whole-line `![[…]]` draws its target instead of itself. Read here, in
    // the per-line parse, so the file is touched once per edit rather than once
    // per frame.
    //
    // `allows_inline` gates it for the same reason it gates the inline scan: in
    // a fence or in front matter the brackets are TEXT — someone documenting
    // the embed syntax must see what they wrote, not the file it names. It is
    // the rule `compile::expand` applies to the same construct on export.
    let embed = if app.embed_mode.is_on() && block.allows_inline() {
        crate::transclude::compile::whole_line_embed(&source).map(|link| {
            let from = doc
                .buffer
                .path
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            crate::transclude::preview::expand(
                &link,
                &from,
                &app.config.transclude,
                measure,
                app.embed_mode,
            )
        })
    } else {
        None
    };

    LineEntry {
        wrap_raw: wrap_hanging(&source, measure, hang_raw),
        wrap_concealed: wrap_hanging(&concealed, measure, hang_concealed),
        source,
        block,
        styled,
        cmap,
        hang_raw,
        hang_concealed,
        embed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::render::markdown::block::BlockCache;
    use crate::text::cursor::Cursor;

    const MEASURE: u16 = 40;

    fn app_with(text: &str) -> App {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.buffer.insert_str(Cursor::new(0, 0), text);
        app.buffer.cursor = Cursor::new(0, 0);
        app.blocks = BlockCache::build(&app.buffer);
        app.buffer.dirty_line = None;
        app
    }

    /// Sync the way the event loop does: blocks first, then the render cache.
    fn sync(app: &mut App) -> u64 {
        app.refresh_blocks();
        let mut cache = app.cache.borrow_mut();
        cache.sync(app, MEASURE);
        cache.builds
    }

    fn doc() -> String {
        (0..200)
            .map(|i| format!("line {i} with some **bold** text\n"))
            .collect()
    }

    /// A second frame with nothing changed parses nothing.
    #[test]
    fn an_unchanged_frame_parses_nothing() {
        let mut app = app_with(&doc());
        let first = sync(&mut app);
        assert_eq!(first, 201, "the first sync parses the document once");
        assert_eq!(sync(&mut app), first, "a repeat frame is free");
    }

    /// Moving the cursor re-reveals lines but re-parses none of them: the raw
    /// and concealed forms are two views of one cached parse.
    #[test]
    fn cursor_movement_does_not_reparse() {
        let mut app = app_with(&doc());
        let before = sync(&mut app);
        app.buffer.cursor = Cursor::new(40, 0);
        app.active = crate::render::conceal::ActiveSet { start: 40, end: 40 };
        assert_eq!(sync(&mut app), before);
    }

    /// Editing one line in place re-parses that line only.
    #[test]
    fn an_in_place_edit_reparses_one_line() {
        let mut app = app_with(&doc());
        let before = sync(&mut app);
        app.buffer.insert_str(Cursor::new(40, 0), "x");
        assert_eq!(sync(&mut app) - before, 1);
    }

    /// Splitting a line shifts every line below it. The splice moves their
    /// entries along instead of re-parsing the rest of the document.
    #[test]
    fn inserting_a_line_keeps_the_lines_below_cached() {
        let mut app = app_with(&doc());
        let before = sync(&mut app);
        app.buffer.insert_str(Cursor::new(40, 5), "\n");
        let parsed = sync(&mut app) - before;
        assert!(parsed <= 2, "expected the split line's two halves, got {parsed}");
    }

    /// Joining two lines likewise costs one parse, not the tail of the file.
    #[test]
    fn deleting_a_line_keeps_the_lines_below_cached() {
        let mut app = app_with(&doc());
        let before = sync(&mut app);
        let start = app.buffer.char_index(Cursor::new(40, 0));
        let end = app.buffer.char_index(Cursor::new(41, 0));
        app.buffer.delete_chars(start, end);
        let parsed = sync(&mut app) - before;
        assert!(parsed <= 2, "expected one merged line, got {parsed}");
    }

    /// The splice is only a guess; verification is what guarantees the cache is
    /// right. Feed it a deliberately wrong hint and the content still matches.
    #[test]
    fn verification_survives_a_wrong_splice_hint() {
        let mut app = app_with("alpha\nbravo\ncharlie\ndelta\n");
        sync(&mut app);
        // Two edits far apart whose line-count changes cancel out — the single
        // "lowest dirty line" hint cannot describe both.
        let start = app.buffer.char_index(Cursor::new(0, 5));
        app.buffer.delete_chars(start, start + 1); // join lines 0 and 1
        app.buffer.insert_str(Cursor::new(2, 0), "echo\n");
        sync(&mut app);

        let cache = app.cache.borrow();
        for line in 0..app.buffer.line_count() {
            assert_eq!(
                cache.entry(line).map(|e| e.source.as_str()),
                Some(app.buffer.line_text(line).as_str()),
                "line {line} out of step with the buffer"
            );
        }
    }

    /// Fenced code reaches the styler with its language and lexer state, so a
    /// keyword, a string and a comment each get their own color — through the
    /// block cache and the render cache, the way a frame gets them.
    #[test]
    fn a_fenced_block_is_highlighted_in_its_language() {
        let mut app = app_with("```rust\nlet s = \"hi\"; // note\n```\n");
        sync(&mut app);
        let cache = app.cache.borrow();
        let entry = cache.entry(1).expect("the body line is cached");
        let fg = |col: usize| {
            entry
                .styled
                .iter()
                .find(|s| s.range.contains(&col))
                .map(|s| s.style.fg)
        };
        let source = &entry.source;
        assert_eq!(fg(0), Some(app.theme.syntax_keyword), "`let` in {source:?}");
        assert_eq!(fg(source.find('"').unwrap()), Some(app.theme.syntax_string));
        assert_eq!(fg(source.find('/').unwrap()), Some(app.theme.syntax_comment));
        // The slab still runs under all of it.
        assert!(entry.styled.iter().all(|s| s.style.bg == app.theme.code_bg));
    }

    /// A language nobody taught it, and the setting turned off, both land on the
    /// flat slab this drew before highlighting existed.
    #[test]
    fn an_unknown_language_and_a_disabled_setting_stay_flat() {
        let mut app = app_with("```brainfuck\nlet x = 1;\n```\n");
        sync(&mut app);
        let flat = |app: &App| {
            let cache = app.cache.borrow();
            let entry = cache.entry(1).unwrap();
            entry.styled.iter().all(|s| s.style.fg == app.theme.code)
        };
        assert!(flat(&app), "an unknown info string is not highlighted");

        let mut app = app_with("```rust\nlet x = 1;\n```\n");
        app.config.markdown.code_syntax = false;
        sync(&mut app);
        assert!(flat(&app), "code_syntax = false is not highlighted");
    }

    /// Line spacing puts rows in the index that belong to no line's text — and
    /// leaves out the ones that would read as damage.
    #[test]
    fn line_spacing_adds_rows_that_belong_to_no_line() {
        let mut app = app_with("alpha\n\nbravo\n");
        let plain = {
            sync(&mut app);
            app.cache.borrow().total_rows()
        };

        app.config.layout.line_spacing = 1;
        sync(&mut app);
        let cache = app.cache.borrow();
        assert!(cache.total_rows() > plain, "spacing adds rows");

        let row = |r: usize| cache.row(r).expect("row in range");
        assert!(!row(0).is_spacer(), "the line itself comes first");
        assert!(row(1).is_spacer(), "then its spacing");
        assert_eq!(row(1).line(), 0, "which answers with the line it follows");
        assert!(!row(1).editable, "there is nothing in the rope behind it");
        // The blank line between the paragraphs is already a gap: it draws its
        // own row and no spacing, so the break stays ONE row wider than the
        // spacing inside a paragraph rather than twice it.
        assert!(!row(2).is_spacer());
        assert_eq!(row(2).line(), 1);
        assert!(!row(3).is_spacer());
        assert_eq!(row(3).line(), 2);

        // The cursor still locates onto content, never onto air.
        let (r, _) = cache.locate(2, 0);
        assert_eq!(r, 3);
    }

    /// A fenced block is a slab and a table is a grid. Neither takes spacing
    /// between its rows, or the shape breaks.
    #[test]
    fn line_spacing_leaves_fences_and_tables_alone() {
        let mut app = app_with("```\none\ntwo\n```\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        app.config.layout.line_spacing = 2;
        sync(&mut app);
        let cache = app.cache.borrow();

        // Lines 0..=2 are the fence's opener and body; 4..=6 are the table.
        for line in [0, 1, 2, 4, 5] {
            let start = cache.locate(line, 0).0;
            let next = cache.locate(line + 1, 0).0;
            assert_eq!(next - start, 1, "line {line} gained spacing it should not have");
        }
        // The row after the closing fence is spacing, not the table.
        let close = cache.locate(3, 0).0;
        assert!(cache.row(close + 1).unwrap().is_spacer());
    }

    /// Rows are indexed, not materialized: the row index answers where a line
    /// starts and what a row holds without building a row per line.
    #[test]
    fn row_index_tracks_wrapped_lines() {
        let mut app = app_with("short\nthis line is long enough that it wraps across the measure\n");
        sync(&mut app);
        let cache = app.cache.borrow();

        assert_eq!(cache.row(0).unwrap().line(), 0);
        assert_eq!(cache.row(1).unwrap().line(), 1);
        assert!(cache.total_rows() > 3, "the long line should wrap");

        // Every row maps back to a line, in order, with no gaps.
        let mut prev = 0;
        for r in 0..cache.total_rows() {
            let row = cache.row(r).expect("row in range");
            assert!(row.line() >= prev);
            prev = row.line();
        }
        assert!(cache.row(cache.total_rows()).is_none());

        // `locate` is the inverse: it lands on a row that contains the column.
        let (row, off) = cache.locate(1, 30);
        let vr = cache.row(row).unwrap();
        assert_eq!(vr.line(), 1);
        assert_eq!(vr.start_col + off, 30);
    }

    /// The same rule `compile::expand` follows: a `![[…]]` inside a fence is
    /// the syntax being documented, so it must draw as itself and not as a box
    /// full of somebody else's file.
    #[test]
    fn a_fenced_embed_does_not_expand_on_screen() {
        let d = std::env::temp_dir().join(format!(
            "shoin-cache-fence-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("frag.md"), "FRAGMENT\n").unwrap();

        let mut app = app_with("```markdown\n![[frag]]\n```\n\n![[frag]]\n");
        app.buffer.path = Some(d.join("comp.md"));
        app.embed_mode = crate::transclude::Mode::Short;
        // Park the cursor away from both, so neither line is active.
        app.buffer.cursor = Cursor::new(0, 0);
        sync(&mut app);

        let cache = app.cache.borrow();
        assert!(
            cache.entry(1).unwrap().embed.is_none(),
            "the fenced example is literal"
        );
        assert!(
            cache.entry(4).unwrap().embed.is_some(),
            "the real embed still expands"
        );
        drop(cache);
        std::fs::remove_dir_all(&d).ok();
    }


    /// RULE 1 of the validity rule, held to its word: a frame at an unchanged
    /// revision does not walk the document at all.
    ///
    /// This is a PERFORMANCE assertion with a correctness twin below, and it is
    /// worth a test because the rule was written in the module doc and then not
    /// implemented — a cursor key on a 512 KB document was comparing every
    /// character of every line off the rope before drawing a row.
    #[test]
    fn an_unchanged_frame_does_not_verify() {
        let mut app = app_with(&doc());
        sync(&mut app);
        let walks = app.cache.borrow().verifications;

        // Frames that change what is DRAWN but not what is parsed: a cursor
        // move, a re-reveal, a scroll.
        for line in [10usize, 40, 120] {
            app.buffer.cursor = Cursor::new(line, 0);
            app.active = crate::render::conceal::ActiveSet { start: line, end: line };
            sync(&mut app);
        }
        assert_eq!(
            app.cache.borrow().verifications,
            walks,
            "an unchanged revision must not re-walk the document"
        );

        // An edit is the thing that does earn a walk.
        app.buffer.insert_str(Cursor::new(40, 0), "x");
        sync(&mut app);
        assert_eq!(app.cache.borrow().verifications, walks + 1);
    }

    /// A batch of edits absorbed between two frames costs ONE walk, not one
    /// per edit — the property `App::step` buys by draining the event queue
    /// before it draws.
    ///
    /// This is the freeze, stated as arithmetic. A verification walk is
    /// O(lines), and a paste is one key event per character already sitting in
    /// the queue. Drawing between two of them made the cost of a paste
    /// O(characters x lines): 10 KB into a 5 000-line document took a minute of
    /// terminal that answered nothing. Nobody could have read those frames
    /// anyway — each was on screen only for as long as it took to compute the
    /// next one.
    ///
    /// The assertion is deliberately on the WALK and not on wall-clock time:
    /// what went wrong was never the speed of any one frame, it was how many
    /// frames a paste asked for.
    #[test]
    fn a_batch_of_edits_costs_one_walk() {
        let mut app = app_with(&doc());
        sync(&mut app);
        let walks = app.cache.borrow().verifications;

        // 500 characters typed into the document with no frame between them,
        // which is what a paste looks like from inside the event loop.
        for i in 0..500 {
            app.buffer.insert_str(Cursor::new(40, 0), "x");
            assert_eq!(
                app.cache.borrow().verifications,
                walks,
                "edit {i} must not walk the document on its own"
            );
        }

        sync(&mut app);
        assert_eq!(
            app.cache.borrow().verifications,
            walks + 1,
            "the whole batch is one walk — a frame per edit is the freeze"
        );

        // …and the batch still lands in full. Coalescing changes WHEN the
        // document is re-read, never what it says.
        assert!(
            app.buffer.line_text(40).starts_with(&"x".repeat(500)),
            "every absorbed edit has to be in the buffer"
        );
    }

    /// The correctness twin: the fast path must never outlive the thing that
    /// justifies it. A changed measure, theme or embed mode re-parses even
    /// though the revision has not moved.
    #[test]
    fn the_fast_path_yields_to_everything_that_invalidates_a_parse() {
        let mut app = app_with("# heading\nsome **bold** text\n");
        app.refresh_blocks();
        {
            let mut c = app.cache.borrow_mut();
            c.sync(&app, MEASURE);
        }
        let before = app.cache.borrow().builds;

        // A different measure is a different wrap and a different conceal.
        {
            let mut c = app.cache.borrow_mut();
            c.sync(&app, MEASURE - 10);
        }
        assert!(app.cache.borrow().builds > before, "a resize re-parses");

        // A different theme is a different style key.
        let after_resize = app.cache.borrow().builds;
        app.theme.text = crate::render::theme::Color::Indexed(9);
        {
            let mut c = app.cache.borrow_mut();
            c.sync(&app, MEASURE - 10);
        }
        assert!(
            app.cache.borrow().builds > after_resize,
            "a theme change re-parses"
        );
    }


    /// The transclude half of the same premise: these settings reach an
    /// embed's expansion, so changing one has to re-parse the `![[…]]` lines.
    #[test]
    fn a_transclude_change_invalidates_the_parse() {
        let mut app = app_with("text\n");
        app.refresh_blocks();
        {
            let mut c = app.cache.borrow_mut();
            c.sync(&app, MEASURE);
        }
        let before = app.cache.borrow().builds;

        app.config.transclude.heading_offset += 1;
        {
            let mut c = app.cache.borrow_mut();
            c.sync(&app, MEASURE);
        }
        assert!(
            app.cache.borrow().builds > before,
            "a transclude setting reaches build(), so it must invalidate"
        );
    }

    /// `StyleKey` has to name EVERY input `build` reads that is not the line's
    /// own text — that is the premise the whole fast path rests on. Two of them
    /// were missing, and the bug predates the fast path: `matches()` compares
    /// only the source and the block, so a `hanging_indent` change never
    /// invalidated anything even when every line was re-verified.
    #[test]
    fn a_layout_change_that_reaches_the_parse_invalidates_it() {
        let mut app = app_with("- a list item long enough to wrap at this measure indeed\n");
        app.config.layout.hanging_indent = true;
        app.refresh_blocks();
        {
            let mut c = app.cache.borrow_mut();
            c.sync(&app, MEASURE);
        }
        let hung = app.cache.borrow().entry(0).unwrap().hang(false);
        assert!(hung > 0, "a list item hangs when the setting is on");

        app.config.layout.hanging_indent = false;
        {
            let mut c = app.cache.borrow_mut();
            c.sync(&app, MEASURE);
        }
        assert_eq!(
            app.cache.borrow().entry(0).unwrap().hang(false),
            0,
            "turning hanging_indent off must re-parse the line"
        );
    }

}

