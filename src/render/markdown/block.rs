//! Pass A — per-line block classification with a cached state vector.
//! SPEC.md §5.2.
//!
//! Only two constructs carry state across lines: fenced code blocks and front
//! matter. Both are captured by `Carry`. Because carry state is one small enum,
//! incremental invalidation is cheap: after an edit at line L, rescan forward
//! and STOP as soon as the recomputed carry matches the cached carry. A typical
//! paragraph edit terminates after one line.

use super::code::{self, Cont, Lang};
use crate::text::buffer::Buffer;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    Dash,
    Star,
    Plus,
    Ordered,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Heading(u8),
    /// Opening fence, carrying its info string.
    FenceOpen(String),
    /// A line inside a fence, carrying what it takes to highlight it: the
    /// language its opener named (SPEC.md §5.3) and the lexer state ENTERING
    /// this line. Both are part of the kind because the render cache re-parses
    /// a line whose kind changed — which is exactly what must happen when an
    /// unterminated string above it opens or closes.
    FenceBody { lang: Option<Lang>, cont: Cont },
    FenceClose,
    /// `---` delimited YAML, only at the very head of the file.
    FrontMatter,
    Quote(u8),
    ListItem {
        depth: u8,
        marker: Marker,
        /// `Some(false)` = `- [ ]`, `Some(true)` = `- [x]`
        checked: Option<bool>,
    },
    /// Line containing unescaped `|` with a delimiter row above it.
    Table,
    /// `---`, `***`, `___`
    Rule,
    Blank,
    Paragraph,
}

/// The only state that crosses a line boundary.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Carry {
    #[default]
    None,
    /// Inside a fenced block opened with this fence string, in the language its
    /// info string named, with the code lexer in this state.
    InFence { fence: String, lang: Option<Lang>, cont: Cont },
    InFrontMatter,
    /// A table's delimiter row has been seen; the rows that follow belong to it
    /// until a line without a `|` ends it.
    InTable,
}

/// Cached classification, parallel to the buffer's lines.
pub struct BlockCache {
    pub kinds: Vec<BlockKind>,
    /// Carry state ENTERING each line. `carry[i]` is the state before line i.
    pub carry: Vec<Carry>,
    pub revision: u64,
}

impl BlockCache {
    pub fn build(buffer: &Buffer) -> Self {
        let n = buffer.line_count();
        let mut kinds = Vec::with_capacity(n);
        let mut carry = Vec::with_capacity(n);
        let mut cur = Carry::None;
        for i in 0..n {
            carry.push(cur.clone());
            let line = buffer.line_text(i);
            let (kind, next) = classify(&line, &cur, i == 0);
            kinds.push(kind);
            promote_table_header(&mut kinds, buffer, i, &cur);
            cur = next;
        }
        BlockCache {
            kinds,
            carry,
            revision: buffer.revision,
        }
    }

    /// Rescan forward from the edit at `edited`, stopping early once the
    /// recomputed carry matches the cached one. Returns the last line touched.
    ///
    /// Correctness rests on one fact: lines before the edit were not touched,
    /// so the state ENTERING the line the scan resumes at is still valid. The early stop only fires when the line
    /// count is unchanged, because an insertion/deletion shifts every cached
    /// carry entry and the index-wise comparison would be against the wrong
    /// source line; there we simply rescan to EOF, which is still correct.
    pub fn invalidate_from(&mut self, buffer: &Buffer, edited: usize) -> usize {
        let n = buffer.line_count();
        let edited = edited.min(n.saturating_sub(1));
        // Resume ONE LINE EARLIER than the edit: a table header is classified
        // by the delimiter row beneath it, so deleting that row has to revisit
        // the header to demote it again.
        let start = edited.saturating_sub(1);

        // No valid entering state to resume from — rebuild wholesale.
        if start == 0 || start >= self.carry.len() {
            *self = Self::build(buffer);
            return n.saturating_sub(1);
        }

        let can_early_stop = n == self.kinds.len();
        let mut cur = self.carry[start].clone();

        self.kinds.resize(n, BlockKind::Paragraph);
        self.carry.resize(n, Carry::None);

        let mut last = start;
        let mut i = start;
        while i < n {
            let old_next = if i + 1 < n {
                Some(self.carry[i + 1].clone())
            } else {
                None
            };
            self.carry[i] = cur.clone();
            let line = buffer.line_text(i);
            let (kind, next) = classify(&line, &cur, i == 0);
            self.kinds[i] = kind;
            promote_table_header(&mut self.kinds, buffer, i, &cur);
            last = i;

            // Past the edit and the state leaving this line matches what the
            // next line already expected: everything below is unaffected.
            //
            // `i >= edited`, never the resumed line before it: that line was
            // NOT edited, so its carry is unchanged by construction and the
            // scan would stop before it ever reclassified the edit. Typing
            // `- ` in front of a paragraph left the line a paragraph until the
            // file was reloaded.
            if can_early_stop && i >= edited {
                if let Some(old) = old_next {
                    if next == old {
                        break;
                    }
                }
            }
            cur = next;
            i += 1;
        }
        self.revision = buffer.revision;
        last
    }
}

/// Classify one line given the carry state entering it.
/// Returns its kind and the carry state leaving it.
pub fn classify(line: &str, carry: &Carry, is_file_head: bool) -> (BlockKind, Carry) {
    // Multi-line constructs win over anything their body might resemble.
    match carry {
        Carry::InFrontMatter => {
            let leaving = if closes_front_matter(line) {
                Carry::None
            } else {
                Carry::InFrontMatter
            };
            return (BlockKind::FrontMatter, leaving);
        }
        Carry::InFence { fence, lang, cont } => {
            if closes_fence(line, fence) {
                return (BlockKind::FenceClose, Carry::None);
            }
            // The body is lexed HERE, in the pass that already walks every line
            // and already caches its result, so the styler below never has to
            // scan from the top of the block to learn what state a line is in.
            // The tokens are thrown away; only the state leaving the line
            // crosses the boundary.
            let next = match lang {
                Some(lang) => code::scan(line, *lang, *cont, &mut Vec::new()),
                None => Cont::None,
            };
            return (
                BlockKind::FenceBody { lang: *lang, cont: *cont },
                Carry::InFence { fence: fence.clone(), lang: *lang, cont: next },
            );
        }
        // A table runs until a line with no cell separator in it.
        Carry::InTable => {
            if has_cell_separator(line) {
                return (BlockKind::Table, Carry::InTable);
            }
        }
        Carry::None => {}
    }

    // Leading whitespace: 4+ columns is (CommonMark) indented code, which this
    // ruleset has no block kind for — such lines simply stay Paragraph.
    let indent = leading_ws_cols(line);

    if let Some(fence) = fence_opener(line) {
        let info = line.trim_start()[fence.len()..].trim().to_string();
        let lang = Lang::from_info(&info);
        return (
            BlockKind::FenceOpen(info),
            Carry::InFence { fence, lang, cont: Cont::None },
        );
    }

    if is_file_head && line.trim_end() == "---" {
        return (BlockKind::FrontMatter, Carry::InFrontMatter);
    }

    let body = line.trim();
    if body.is_empty() {
        return (BlockKind::Blank, Carry::None);
    }

    // `|---|:--:|` — the delimiter row is what makes the lines around it a
    // table. The header ABOVE it is upgraded by `BlockCache`, which is the only
    // place that can see backwards. SPEC.md §5.2.
    if is_table_delimiter(body) {
        return (BlockKind::Table, Carry::InTable);
    }

    // Headings and rules anchor the left margin: 4+ columns of indent (which a
    // richer ruleset would read as indented code) demotes them to prose.
    if indent < 4 {
        // Thematic break before list: `* * *` and `- - -` are rules, not items.
        if is_thematic_break(body) {
            return (BlockKind::Rule, Carry::None);
        }
        if let Some(level) = atx_heading_level(line.trim_start()) {
            return (BlockKind::Heading(level), Carry::None);
        }
    }

    // Lists and quotes nest, so their indentation is meaningful rather than
    // disqualifying — depth is exactly what we want to keep.
    if line.trim_start().starts_with('>') {
        return (BlockKind::Quote(quote_depth(line.trim_start())), Carry::None);
    }
    if let Some((marker, checked)) = list_marker(line.trim_start()) {
        return (
            BlockKind::ListItem {
                depth: indent.min(u8::MAX as usize) as u8,
                marker,
                checked,
            },
            Carry::None,
        );
    }

    (BlockKind::Paragraph, Carry::None)
}

/// Leading whitespace measured in columns (a tab counts as one column here — it
/// only gates the 4-column indented-code threshold, where exactness is moot).
fn leading_ws_cols(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// A run of >=3 backticks or tildes at the line start (after <4 spaces) opens a
/// fence. Returns the delimiter run, e.g. `"```"`. A backtick fence's info
/// string may not itself contain a backtick.
fn fence_opener(line: &str) -> Option<String> {
    if leading_ws_cols(line) >= 4 {
        return None;
    }
    let s = line.trim_start();
    let ch = s.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let run = s.chars().take_while(|c| *c == ch).count();
    if run < 3 {
        return None;
    }
    if ch == '`' && s[run..].contains('`') {
        return None;
    }
    Some(std::iter::repeat_n(ch, run).collect())
}

/// A closing fence is a line of only the opener's fence character, at least as
/// long as the opener, with no trailing info string.
fn closes_fence(line: &str, open: &str) -> bool {
    let ch = match open.chars().next() {
        Some(c) => c,
        None => return false,
    };
    let s = line.trim();
    !s.is_empty()
        && s.chars().all(|c| c == ch)
        && s.chars().count() >= open.chars().count()
}

/// YAML front matter closes on a line of `---` or `...`.
fn closes_front_matter(line: &str) -> bool {
    matches!(line.trim_end(), "---" | "...")
}

/// `# ` .. `###### ` — 1..=6 hashes followed by a space or end of line.
fn atx_heading_level(s: &str) -> Option<u8> {
    let hashes = s.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    match s[hashes..].chars().next() {
        None => Some(hashes as u8),
        Some(' ') | Some('\t') => Some(hashes as u8),
        _ => None,
    }
}

/// A table's header row sits ABOVE the delimiter row that identifies it, and
/// `classify` only ever looks forward. So when line `i` turns out to open a
/// table, the line before it is upgraded here — the one place with the whole
/// sequence in hand.
fn promote_table_header(kinds: &mut [BlockKind], buffer: &Buffer, i: usize, entering: &Carry) {
    if i == 0 || kinds[i] != BlockKind::Table || *entering == Carry::InTable {
        return;
    }
    if has_cell_separator(&buffer.line_text(i - 1)) {
        kinds[i - 1] = BlockKind::Table;
    }
}

/// An unescaped `|` — what makes a line a table row, given a delimiter row
/// above it.
///
/// `pub` because HTML export has to promote a table header exactly the way
/// `promote_table_header` does: `classify` only looks forward, so anything
/// walking a document line by line inherits the same blind spot.
pub fn has_cell_separator(line: &str) -> bool {
    let mut escaped = false;
    for c in line.chars() {
        match c {
            '\\' => escaped = !escaped,
            '|' if !escaped => return true,
            _ => escaped = false,
        }
    }
    false
}

/// A GFM table delimiter row: `|---|:--:|`, i.e. only `- : | ` and at least one
/// of each of `-` and `|`.
fn is_table_delimiter(body: &str) -> bool {
    let mut dashes = false;
    let mut bars = false;
    for c in body.chars() {
        match c {
            '-' => dashes = true,
            '|' => bars = true,
            ':' | ' ' | '\t' => {}
            _ => return false,
        }
    }
    dashes && bars
}

/// `>=3` of the same char among `- * _`, with only spaces between.
fn is_thematic_break(body: &str) -> bool {
    let mut ch = None;
    let mut count = 0;
    for c in body.chars() {
        match c {
            ' ' | '\t' => {}
            '-' | '*' | '_' => {
                match ch {
                    Some(x) if x != c => return false,
                    _ => {}
                }
                ch = Some(c);
                count += 1;
            }
            _ => return false,
        }
    }
    count >= 3
}

/// Number of leading `>` markers, each allowed one trailing space.
fn quote_depth(s: &str) -> u8 {
    let mut depth = 0u8;
    let mut chars = s.chars().peekable();
    loop {
        match chars.peek() {
            Some('>') => {
                chars.next();
                depth = depth.saturating_add(1);
                if chars.peek() == Some(&' ') {
                    chars.next();
                }
            }
            Some(' ') => {
                chars.next();
            }
            _ => break,
        }
    }
    depth
}

/// A list marker at the start of the (indent-stripped) line: `- `, `* `, `+ `,
/// or an ordered `N.`/`N)`. Detects an optional task checkbox after the marker.
fn list_marker(s: &str) -> Option<(Marker, Option<bool>)> {
    let mut chars = s.chars();
    let first = chars.next()?;

    let (marker, rest_at) = match first {
        '-' => (Marker::Dash, 1),
        '*' => (Marker::Star, 1),
        '+' => (Marker::Plus, 1),
        d if d.is_ascii_digit() => {
            let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
            let delim = s.chars().nth(digits);
            if !matches!(delim, Some('.') | Some(')')) {
                return None;
            }
            (Marker::Ordered, digits + 1)
        }
        _ => return None,
    };

    // The marker must be followed by a space (or be the whole line: `- `).
    match s.chars().nth(rest_at) {
        None => return Some((marker, None)),
        Some(' ') | Some('\t') => {}
        _ => return None,
    }

    let after: String = s.chars().skip(rest_at + 1).collect();
    let checked = task_checkbox(&after);
    Some((marker, checked))
}

/// `[ ] ` -> `Some(false)`, `[x] `/`[X] ` -> `Some(true)`, else `None`.
fn task_checkbox(after_marker: &str) -> Option<bool> {
    let mut chars = after_marker.chars();
    if chars.next()? != '[' {
        return None;
    }
    let inner = chars.next()?;
    if chars.next()? != ']' {
        return None;
    }
    // Must be followed by a space or end the line.
    match chars.next() {
        None | Some(' ') | Some('\t') => {}
        _ => return None,
    }
    match inner {
        ' ' => Some(false),
        'x' | 'X' => Some(true),
        _ => None,
    }
}


impl BlockKind {
    /// Inline scanning is suppressed inside fenced code and front matter.
    pub fn allows_inline(&self) -> bool {
        !matches!(
            self,
            BlockKind::FenceOpen(_)
                | BlockKind::FenceBody { .. }
                | BlockKind::FenceClose
                | BlockKind::FrontMatter
                // A table's columns are aligned by its author, in the source.
                // Concealing markers inside a cell would pull the pipes out of
                // line, so table rows stay 1:1 like code does.
                | BlockKind::Table
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::buffer::Buffer;
    use crate::text::cursor::Cursor;

    /// Classify a standalone line with no carry, not at file head.
    fn kind(line: &str) -> BlockKind {
        classify(line, &Carry::None, false).0
    }

    fn buf(text: &str) -> Buffer {
        let mut b = Buffer::empty();
        b.insert_str(Cursor::new(0, 0), text);
        b
    }

    /// A table is identified by its delimiter row, and the header ABOVE it is
    /// promoted retroactively — the one backwards-looking rule in the scanner.
    #[test]
    fn tables_classify_header_delimiter_and_body() {
        let b = buf("intro\n| a | b |\n|---|---|\n| 1 | 2 |\n\nafter\n");
        let cache = BlockCache::build(&b);
        assert_eq!(cache.kinds[0], BlockKind::Paragraph, "prose above is untouched");
        assert_eq!(cache.kinds[1], BlockKind::Table, "header row promoted");
        assert_eq!(cache.kinds[2], BlockKind::Table, "delimiter row");
        assert_eq!(cache.kinds[3], BlockKind::Table, "body row");
        assert_eq!(cache.kinds[4], BlockKind::Blank, "a blank line ends the table");
        assert_eq!(cache.kinds[5], BlockKind::Paragraph);
    }

    /// `---` on its own is still a rule, not a table delimiter.
    #[test]
    fn a_rule_is_not_a_table_delimiter() {
        assert_eq!(kind("---"), BlockKind::Rule);
        assert_eq!(kind("| --- | --- |"), BlockKind::Table);
    }

    /// Removing the delimiter row must demote the header again. This is why
    /// `refresh_blocks` rescans from one line before the edit.
    #[test]
    fn deleting_the_delimiter_row_demotes_the_header() {
        let mut b = buf("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let mut cache = BlockCache::build(&b);
        assert_eq!(cache.kinds[0], BlockKind::Table);

        let start = b.rope.line_to_char(1);
        let end = b.rope.line_to_char(2);
        b.delete_chars(start, end);
        cache.invalidate_from(&b, 0);
        assert_eq!(cache.kinds, BlockCache::build(&b).kinds);
        assert_eq!(cache.kinds[0], BlockKind::Paragraph, "no delimiter, no table");
    }

    #[test]
    fn headings() {
        assert_eq!(kind("# Title"), BlockKind::Heading(1));
        assert_eq!(kind("###### deep"), BlockKind::Heading(6));
        assert_eq!(kind("#"), BlockKind::Heading(1));
        // Seven hashes is not a heading; no space after hashes is not either.
        assert_eq!(kind("####### too deep"), BlockKind::Paragraph);
        assert_eq!(kind("#no-space"), BlockKind::Paragraph);
    }

    #[test]
    fn lists_and_tasks() {
        assert!(matches!(
            kind("- item"),
            BlockKind::ListItem { marker: Marker::Dash, checked: None, .. }
        ));
        assert!(matches!(
            kind("* item"),
            BlockKind::ListItem { marker: Marker::Star, .. }
        ));
        assert!(matches!(
            kind("1. item"),
            BlockKind::ListItem { marker: Marker::Ordered, .. }
        ));
        assert!(matches!(
            kind("12) item"),
            BlockKind::ListItem { marker: Marker::Ordered, .. }
        ));
        assert!(matches!(
            kind("- [ ] todo"),
            BlockKind::ListItem { checked: Some(false), .. }
        ));
        assert!(matches!(
            kind("- [x] done"),
            BlockKind::ListItem { checked: Some(true), .. }
        ));
        // Nested item carries its indentation as depth.
        assert!(matches!(
            kind("    - deep"),
            BlockKind::ListItem { depth: 4, .. }
        ));
    }

    #[test]
    fn rules_beat_lists() {
        assert_eq!(kind("---"), BlockKind::Rule);
        assert_eq!(kind("***"), BlockKind::Rule);
        assert_eq!(kind("___"), BlockKind::Rule);
        assert_eq!(kind("- - -"), BlockKind::Rule);
        assert_eq!(kind("* * *"), BlockKind::Rule);
        // Mixed markers are not a rule.
        assert_eq!(kind("-*-"), BlockKind::Paragraph);
    }

    #[test]
    fn quotes() {
        assert_eq!(kind("> a"), BlockKind::Quote(1));
        assert_eq!(kind(">> a"), BlockKind::Quote(2));
        assert_eq!(kind("> > a"), BlockKind::Quote(2));
    }

    #[test]
    fn blank_and_paragraph() {
        assert_eq!(kind(""), BlockKind::Blank);
        assert_eq!(kind("   "), BlockKind::Blank);
        assert_eq!(kind("just prose"), BlockKind::Paragraph);
        // 4-space indent is not a heading/list here — falls through to prose.
        assert_eq!(kind("    # not a heading"), BlockKind::Paragraph);
    }

    #[test]
    fn fenced_code_spans_lines() {
        let b = buf("```rust\nlet x = 1;\n```\nafter\n");
        let cache = BlockCache::build(&b);
        assert_eq!(cache.kinds[0], BlockKind::FenceOpen("rust".into()));
        assert!(matches!(cache.kinds[1], BlockKind::FenceBody { .. }));
        assert_eq!(cache.kinds[2], BlockKind::FenceClose);
        assert_eq!(cache.kinds[3], BlockKind::Paragraph);
        // A `#` inside the fence is body, not a heading.
        let b2 = buf("```\n# not a heading\n```\n");
        let c2 = BlockCache::build(&b2);
        assert!(matches!(c2.kinds[1], BlockKind::FenceBody { .. }));
    }

    #[test]
    fn front_matter_only_at_head() {
        let b = buf("---\ntitle: x\n---\n# Real heading\n");
        let cache = BlockCache::build(&b);
        assert_eq!(cache.kinds[0], BlockKind::FrontMatter);
        assert_eq!(cache.kinds[1], BlockKind::FrontMatter);
        assert_eq!(cache.kinds[2], BlockKind::FrontMatter);
        assert_eq!(cache.kinds[3], BlockKind::Heading(1));
    }

    #[test]
    fn incremental_invalidation_matches_full_rebuild() {
        let mut b = buf("# Title\n\nsome prose\n- a list\n> a quote\n");
        let mut cache = BlockCache::build(&b);

        // Edit line 2: turn prose into a heading.
        b.insert_str(Cursor::new(2, 0), "## ");
        cache.invalidate_from(&b, 2);

        let fresh = BlockCache::build(&b);
        assert_eq!(cache.kinds, fresh.kinds);
        assert_eq!(cache.carry, fresh.carry);
        assert_eq!(cache.kinds[2], BlockKind::Heading(2));
    }

    /// The rescan resumes one line ABOVE the edit (for table headers), and that
    /// line is unedited — so its carry always matches what the line below
    /// expected. Stopping there left the EDITED line with its old kind, and
    /// nothing recovered it short of reloading the file: markup typed into an
    /// existing paragraph never took effect.
    #[test]
    fn an_edit_is_reclassified_even_when_the_line_above_is_unchanged() {
        let mut b = buf("intro\n\nplain para\n\ntail\n");
        let mut cache = BlockCache::build(&b);
        assert_eq!(cache.kinds[2], BlockKind::Paragraph);

        b.insert_str(Cursor::new(2, 0), "- ");
        cache.invalidate_from(&b, 2);

        assert!(
            matches!(cache.kinds[2], BlockKind::ListItem { .. }),
            "typing `- ` makes the line a list item: {:?}",
            cache.kinds[2]
        );
        assert_eq!(cache.kinds, BlockCache::build(&b).kinds);
    }

    #[test]
    fn incremental_invalidation_across_a_fence_edit() {
        // Opening a fence must reclassify the lines below it.
        let mut b = buf("prose\ncode line\nmore\n");
        let mut cache = BlockCache::build(&b);
        assert_eq!(cache.kinds[1], BlockKind::Paragraph);

        b.insert_str(Cursor::new(0, 0), "```\n");
        // Rescan from the edited first line.
        cache.invalidate_from(&b, 0);

        let fresh = BlockCache::build(&b);
        assert_eq!(cache.kinds, fresh.kinds);
        assert_eq!(cache.kinds[0], BlockKind::FenceOpen(String::new()));
        assert!(matches!(cache.kinds[1], BlockKind::FenceBody { .. }));
    }
}
