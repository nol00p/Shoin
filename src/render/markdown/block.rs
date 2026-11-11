//! Pass A — per-line block classification with a cached state vector.
//! SPEC.md §5.2.
//!
//! State crosses lines in two ways, both captured by `Carry`: the block a
//! line may be inside (a fence, front matter, a table, a math block), and the
//! list items it may be inside, which set where a nested fence or heading is
//! measured from. Carry state is small, so incremental invalidation is cheap:
//! after edits touching lines L..=M, rescan forward from L and STOP at the
//! first line past M where the recomputed carry matches the cached carry. A
//! typical paragraph edit terminates after one line.

use super::code::{self, Cont, Lang};
use crate::text::buffer::Buffer;
use std::rc::Rc;

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
    /// A line inside or delimiting a display math block ($$).
    MathBlock,
}

/// The state that crosses a line boundary.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Carry {
    /// The block the next line is inside, if any.
    pub open: Open,
    /// The list items the next line may be inside.
    pub items: Items,
}

impl Carry {
    /// Whether the line this state follows is inside a list item.
    pub fn in_list(&self) -> bool {
        !self.items.open.is_empty()
    }
}

impl From<Open> for Carry {
    fn from(open: Open) -> Self {
        Carry { open, items: Items::default() }
    }
}

/// A block that runs on past the line that opened it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Open {
    #[default]
    None,
    /// Inside a fenced block opened with this fence string, in the language its
    /// info string named, with the code lexer in this state.
    InFence { fence: String, lang: Option<Lang>, cont: Cont },
    InFrontMatter,
    /// A table's delimiter row has been seen; the rows that follow belong to it
    /// until a line without a `|` ends it.
    InTable,
    InMath,
}

/// The list items open above a line, outermost first, as (marker column, text
/// column) pairs, with marker columns strictly increasing.
///
/// A line is inside an item when it is indented deeper than the item's
/// marker, or is a lazy continuation of its text, or is blank (the next
/// non-blank line decides): the rule `Lists::bounds` applies in the editor.
/// Inside an item, a fence, `$$` block, heading or rule is measured from the
/// item's text column, as CommonMark measures it: under `10. ` a fence at
/// column 4 is a fence, not indented code.
///
/// Shared, not copied: every line of a list carries the same stack until an
/// item opens or closes, so a clone is a count bump.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Items {
    open: Rc<[(u16, u16)]>,
    /// What the line before was, which lazy continuation depends on. Always
    /// `Other` with no item open, so prose outside lists carries nothing.
    after: After,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum After {
    #[default]
    Other,
    /// Paragraph text or an item's own line: a paragraph line may follow it
    /// lazily, at any indent.
    Text,
    /// A display math line: another display block may follow it lazily.
    Math,
}

impl After {
    fn of(kind: &BlockKind) -> After {
        match kind {
            BlockKind::Paragraph | BlockKind::ListItem { .. } => After::Text,
            BlockKind::MathBlock => After::Math,
            _ => After::Other,
        }
    }
}

impl Items {
    /// How many of the open items a line at `indent` is deeper than.
    fn kept(&self, indent: usize) -> usize {
        self.open.iter().take_while(|&&(marker, _)| usize::from(marker) < indent).count()
    }

    /// The column blocks are measured from inside the first `kept` items.
    fn base(&self, kept: usize) -> usize {
        match kept {
            0 => 0,
            k => usize::from(self.open[k - 1].1),
        }
    }

    /// The items open after a line that is not inside a running block.
    ///
    /// This is `Lists::bounds`'s rule, line by line: keep it in step with
    /// `Lists::measure`.
    fn next(&self, line: &str, indent: usize, kept: usize, kind: &BlockKind) -> Items {
        let stays = match (kind, self.after) {
            // A blank line closes nothing yet: the next line decides.
            (BlockKind::Blank, _) => true,
            (BlockKind::Paragraph, After::Text) => true,
            (BlockKind::MathBlock, After::Text | After::Math) => true,
            _ => kept == self.open.len(),
        };
        let open = match kind {
            BlockKind::ListItem { .. } => {
                let col = |c: usize| c.min(u16::MAX as usize) as u16;
                let item = (col(indent), col(item_text_col(line, 1)));
                self.open[..kept].iter().copied().chain([item]).collect()
            }
            _ if stays => self.open.clone(),
            _ => self.open[..kept].into(),
        };
        self.with(open, kind)
    }

    /// These items, after a line of `kind` inside a running block.
    fn after_line(&self, kind: &BlockKind) -> Items {
        self.with(self.open.clone(), kind)
    }

    fn with(&self, open: Rc<[(u16, u16)]>, kind: &BlockKind) -> Items {
        let after = match open.is_empty() {
            true => After::Other,
            false => After::of(kind),
        };
        Items { open, after }
    }
}

/// The column an item line's text starts at, CommonMark's way: past the
/// marker and the 1 to 4 spaces after it. With none (an empty item), nothing
/// after them, or 5 or more, one column past the marker. A tab counts `tab`
/// columns: 1 here, as `leading_ws_cols` counts it, and `tab_width` in
/// `Lists`, as its own indents do.
pub(crate) fn item_text_col(line: &str, tab: usize) -> usize {
    let ws = |c: char| match c {
        ' ' => Some(1),
        '\t' => Some(tab),
        _ => None,
    };
    let mut chars = line.chars().peekable();
    let mut col = 0;
    while let Some(w) = chars.peek().copied().and_then(ws) {
        col += w;
        chars.next();
    }
    while chars.next_if(char::is_ascii_digit).is_some() {
        col += 1;
    }
    // The bullet, or the ordinal's `.` / `)`.
    chars.next();
    col += 1;
    let marker_end = col;
    while let Some(w) = chars.peek().copied().and_then(ws) {
        col += w;
        chars.next();
    }
    let gap = col - marker_end;
    if chars.peek().is_none() || gap == 0 || gap > 4 {
        marker_end + 1
    } else {
        col
    }
}

/// Cached classification, parallel to the buffer's lines.
pub struct BlockCache {
    pub kinds: Vec<BlockKind>,
    /// Carry state ENTERING each line. `carry[i]` is the state before line i.
    pub carry: Vec<Carry>,
    pub revision: u64,
    /// `[markdown] math` as it was when the cache was built. The setting
    /// changes what a `$$` line opens, so flipping it means a rebuild.
    pub math: bool,
}

impl BlockCache {
    pub fn build(buffer: &Buffer, math: bool) -> Self {
        Self::build_from(buffer, math, Carry::default())
    }

    /// `build` for a buffer that starts in the middle of a document, entered
    /// with the state `entering`.
    pub fn build_from(buffer: &Buffer, math: bool, entering: Carry) -> Self {
        let n = buffer.line_count();
        let mut kinds = Vec::with_capacity(n);
        let mut carry = Vec::with_capacity(n);
        let mut cur = entering;
        for i in 0..n {
            carry.push(cur.clone());
            let line = buffer.line_text(i);
            let (kind, next) = classify(&line, &cur, i == 0, math);
            kinds.push(kind);
            promote_table_header(&mut kinds, buffer, i, &cur);
            cur = next;
        }
        BlockCache {
            kinds,
            carry,
            revision: buffer.revision,
            math,
        }
    }

    /// The state entering `line` of `buffer`, classified from the top.
    pub fn carry_before(buffer: &Buffer, line: usize, math: bool) -> Carry {
        let mut cur = Carry::default();
        for i in 0..line.min(buffer.line_count()) {
            cur = classify(&buffer.line_text(i), &cur, i == 0, math).1;
        }
        cur
    }

    /// Rescan forward from the edit at `edited`, stopping early once the
    /// recomputed carry matches the cached one. Returns the last line touched.
    ///
    /// Correctness rests on one fact: lines before the edit were not touched,
    /// so the state ENTERING the line the scan resumes at is still valid. An
    /// insertion or deletion shifts every line below it, so the cached entries
    /// are shifted the same way first, at the edit; the early stop compares
    /// only past `edited_last`, where they line up with their own lines again.
    /// (Until 2026-11 a changed line count rescanned to EOF instead: ~7 ms per
    /// `Enter` or `dd` at 100,000 lines.)
    ///
    /// `edited_last` is the highest line the edits touched: the scan never stops
    /// before it, since an edit that kept the line count may have changed
    /// lines below a point where the carry happens to match again.
    pub fn invalidate_from(&mut self, buffer: &Buffer, edited: usize, edited_last: usize) -> usize {
        let n = buffer.line_count();
        let edited = edited.min(n.saturating_sub(1));
        // Resume ONE LINE EARLIER than the edit: a table header is classified
        // by the delimiter row beneath it, so deleting that row has to revisit
        // the header to demote it again.
        let start = edited.saturating_sub(1);

        // No valid entering state to resume from — rebuild wholesale.
        if start == 0 || start >= self.carry.len() {
            *self = Self::build(buffer, self.math);
            return n.saturating_sub(1);
        }

        // Lines past `edited_last` are as they were (`Buffer::take_dirty`), so
        // shifting the entries at `edited` — inside what the scan revisits —
        // puts each of them back beside its own line. An added count that
        // does not fit inside the edited lines means the bracket is not what
        // it claims, and then nothing is compared: the scan runs to the end.
        let old_n = self.kinds.len().min(self.carry.len());
        let can_early_stop = match n.cmp(&old_n) {
            std::cmp::Ordering::Equal => true,
            std::cmp::Ordering::Greater => {
                let added = n - old_n;
                let fits = edited + added <= edited_last + 1 && edited <= old_n;
                if fits {
                    self.kinds.splice(edited..edited, std::iter::repeat_n(BlockKind::Paragraph, added));
                    self.carry.splice(edited..edited, std::iter::repeat_n(Carry::default(), added));
                }
                fits
            }
            std::cmp::Ordering::Less => {
                // `edited < n`, so the drain stays inside the old entries.
                let removed = old_n - n;
                let fits = edited <= edited_last + 1;
                if fits {
                    self.kinds.drain(edited..edited + removed);
                    self.carry.drain(edited..edited + removed);
                }
                fits
            }
        };
        let mut cur = self.carry[start].clone();

        self.kinds.resize(n, BlockKind::Paragraph);
        self.carry.resize(n, Carry::default());

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
            let (kind, next) = classify(&line, &cur, i == 0, self.math);
            self.kinds[i] = kind;
            promote_table_header(&mut self.kinds, buffer, i, &cur);
            last = i;

            // Past the edit and the state leaving this line matches what the
            // next line already expected: everything below is unaffected.
            //
            // Not before the last edited line either: an edit that kept the
            // line count (a swap of two blocks, an indent) changed lines below
            // a point where the carry can happen to match again.
            //
            // And `i >= edited`, never the resumed line before it: that line was
            // NOT edited, so its carry is unchanged by construction and the
            // scan would stop before it ever reclassified the edit. Typing
            // `- ` in front of a paragraph left the line a paragraph until the
            // file was reloaded.
            //
            // Not above a table's delimiter row, though: `classify` just made
            // this line a paragraph, and only the delimiter row's own pass
            // promotes it back to the table's header. Stopping here left an
            // edited header row unstyled until something rescanned the table.
            let opens_table = i + 1 < n
                && self.kinds[i + 1] == BlockKind::Table
                && self.carry[i + 1].open != Open::InTable;
            if can_early_stop && i >= edited.max(edited_last) && !opens_table {
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

/// Whether a line of a display block ends it: it ends with a `$$` whose
/// first `$` is not escaped (`x = \$$` is a dollar sign and a lone `$`).
fn closes_math(trimmed: &str) -> bool {
    let chars: Vec<char> = trimmed.chars().collect();
    chars.len() >= 2
        && chars.ends_with(&['$', '$'])
        && !crate::render::markdown::inline::is_escaped(&chars, chars.len() - 2)
}

/// Byte offset of the first unescaped `$$` in `s`.
fn find_math_close(s: &str) -> Option<usize> {
    let chars: Vec<char> = s.chars().collect();
    let at = (0..chars.len().saturating_sub(1)).find(|&k| {
        chars[k] == '$' && chars[k + 1] == '$' && !crate::render::markdown::inline::is_escaped(&chars, k)
    })?;
    Some(chars[..at].iter().map(|c| c.len_utf8()).sum())
}

/// Classify one line given the carry state entering it.
/// Returns its kind and the carry state leaving it.
///
/// `math` is `[markdown] math`. Off, a `$$` line is the prose it was before
/// math existed: it never opens a block, so the lines under it classify
/// exactly as they would without the feature.
pub fn classify(line: &str, carry: &Carry, is_file_head: bool, math: bool) -> (BlockKind, Carry) {
    // Inside a fence, front matter or math block the line is text, not
    // structure: the items around it stay as they are.
    if !matches!(carry.open, Open::None | Open::InTable) {
        let (kind, open) = classify_block(line, &carry.open, 0, is_file_head, math);
        let items = carry.items.after_line(&kind);
        return (kind, Carry { open, items });
    }
    let indent = leading_ws_cols(line);
    let kept = carry.items.kept(indent);
    let (kind, open) = classify_block(line, &carry.open, carry.items.base(kept), is_file_head, math);
    let items = carry.items.next(line, indent, kept, &kind);
    (kind, Carry { open, items })
}

/// `classify` for one line, given the block it is inside and the column
/// `base` its indent counts from (the text column of the list item it is in,
/// else 0).
fn classify_block(line: &str, carry: &Open, base: usize, is_file_head: bool, math: bool) -> (BlockKind, Open) {
    // Multi-line constructs win over anything their body might resemble.
    match carry {
        Open::InFrontMatter => {
            let leaving = if closes_front_matter(line) {
                Open::None
            } else {
                Open::InFrontMatter
            };
            return (BlockKind::FrontMatter, leaving);
        }
        Open::InFence { fence, lang, cont } => {
            if closes_fence(line, fence) {
                return (BlockKind::FenceClose, Open::None);
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
                Open::InFence { fence: fence.clone(), lang: *lang, cont: next },
            );
        }
        // A table runs until a line with no cell separator in it.
        Open::InTable => {
            if has_cell_separator(line) {
                return (BlockKind::Table, Open::InTable);
            }
        }
        Open::InMath => {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                // An unclosed $$ is bounded by a blank line so a stray $$
                // does not turn the entire rest of the file into math.
                return (BlockKind::Blank, Open::None);
            }
            if closes_math(trimmed) {
                return (BlockKind::MathBlock, Open::None);
            }
            return (BlockKind::MathBlock, Open::InMath);
        }
        Open::None => {}
    }

    // Leading whitespace: 4+ columns past `base` is (CommonMark) indented
    // code, which this ruleset has no block kind for — such lines simply stay
    // Paragraph.
    let indent = leading_ws_cols(line);
    let shallow = indent < base + 4;

    if math && shallow {
        if let Some(after) = line.trim_start().strip_prefix("$$") {
            match find_math_close(after) {
                // `$$x$$` alone on its line is a one-line block.
                Some(p) if after[p + 2..].trim().is_empty() => {
                    return (BlockKind::MathBlock, Open::None);
                }
                // `$$x$$ more text` is a paragraph that holds display math,
                // as in Pandoc; the inline scanner picks out the `$$…$$`.
                Some(_) => {}
                // No closing `$$` yet: the block runs on below.
                None => return (BlockKind::MathBlock, Open::InMath),
            }
        }
    }

    if let Some(fence) = fence_opener(line).filter(|_| shallow) {
        let info = line.trim_start()[fence.len()..].trim().to_string();
        let lang = Lang::from_info(&info);
        return (
            BlockKind::FenceOpen(info),
            Open::InFence { fence, lang, cont: Cont::None },
        );
    }

    if is_file_head && line.trim_end() == "---" {
        return (BlockKind::FrontMatter, Open::InFrontMatter);
    }

    let body = line.trim();
    if body.is_empty() {
        return (BlockKind::Blank, Open::None);
    }

    // `|---|:--:|` — the delimiter row is what makes the lines around it a
    // table. The header ABOVE it is upgraded by `BlockCache`, which is the only
    // place that can see backwards. SPEC.md §5.2.
    if is_table_delimiter(body) {
        return (BlockKind::Table, Open::InTable);
    }

    // Headings and rules anchor the left margin (or the text column of the
    // item they are in): 4+ columns of indent past it (which a richer ruleset
    // would read as indented code) demotes them to prose.
    if shallow {
        // Thematic break before list: `* * *` and `- - -` are rules, not items.
        if is_thematic_break(body) {
            return (BlockKind::Rule, Open::None);
        }
        if let Some(level) = atx_heading_level(line.trim_start()) {
            return (BlockKind::Heading(level), Open::None);
        }
    }

    // Lists and quotes nest, so their indentation is meaningful rather than
    // disqualifying — depth is exactly what we want to keep.
    if line.trim_start().starts_with('>') {
        return (BlockKind::Quote(quote_depth(line.trim_start())), Open::None);
    }
    if let Some((marker, checked)) = list_marker(line.trim_start()) {
        return (
            BlockKind::ListItem {
                depth: indent.min(u8::MAX as usize) as u8,
                marker,
                checked,
            },
            Open::None,
        );
    }

    (BlockKind::Paragraph, Open::None)
}

/// Leading whitespace measured in columns (a tab counts as one column here — it
/// only gates the 4-column indented-code threshold, where exactness is moot).
pub(crate) fn leading_ws_cols(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// A run of >=3 backticks or tildes at the line start opens a fence (the
/// caller checks the indent). Returns the delimiter run, e.g. `"```"`. A
/// backtick fence's info string may not itself contain a backtick.
fn fence_opener(line: &str) -> Option<String> {
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
    if i == 0 || kinds[i] != BlockKind::Table || entering.open == Open::InTable {
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
                // A math block renders formulas; Markdown inline styling (*, _)
                // must not interfere with mathematical subscripts or asterisks.
                | BlockKind::MathBlock
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
        classify(line, &Carry::default(), false, true).0
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
        let cache = BlockCache::build(&b, true);
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

    /// Editing the header row in place keeps it the table's header: the
    /// rescan has to reach the delimiter row below, which promotes it.
    #[test]
    fn editing_the_header_row_keeps_it_a_header() {
        let mut b = buf("intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        let mut cache = BlockCache::build(&b, true);
        assert_eq!(cache.kinds[2], BlockKind::Table);

        b.insert_str(Cursor::new(2, 3), "x");
        cache.invalidate_from(&b, 2, 2);
        assert_eq!(cache.kinds[2], BlockKind::Table, "header row stays a header");
        assert_eq!(cache.kinds, BlockCache::build(&b, true).kinds);
    }

    /// Two lines swapped, same line count: the carry matches again right
    /// after the first, so only the edited range keeps the scan going to the
    /// second.
    #[test]
    fn a_swap_rescans_to_its_last_line() {
        let mut b = buf("intro\n\n- a\n    more\n- b\n");
        let mut cache = BlockCache::build(&b, true);
        let from = b.rope.line_to_char(3);
        let to = b.rope.line_to_char(5);
        b.delete_chars(from, to);
        b.insert_str(Cursor::new(3, 0), "- b\n    more\n");
        let mut stale = BlockCache::build(&buf("intro\n\n- a\n    more\n- b\n"), true);
        stale.invalidate_from(&b, 3, 3);
        cache.invalidate_from(&b, 3, 4);
        assert_eq!(cache.kinds, BlockCache::build(&b, true).kinds);
        assert_ne!(stale.kinds, cache.kinds, "stopping at the first line misses the second");
    }

    /// Removing the delimiter row must demote the header again. This is why
    /// `refresh_blocks` rescans from one line before the edit.
    #[test]
    fn deleting_the_delimiter_row_demotes_the_header() {
        let mut b = buf("| a | b |\n|---|---|\n| 1 | 2 |\n");
        let mut cache = BlockCache::build(&b, true);
        assert_eq!(cache.kinds[0], BlockKind::Table);

        let start = b.rope.line_to_char(1);
        let end = b.rope.line_to_char(2);
        b.delete_chars(start, end);
        cache.invalidate_from(&b, 0, 0);
        assert_eq!(cache.kinds, BlockCache::build(&b, true).kinds);
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
        let cache = BlockCache::build(&b, true);
        assert_eq!(cache.kinds[0], BlockKind::FenceOpen("rust".into()));
        assert!(matches!(cache.kinds[1], BlockKind::FenceBody { .. }));
        assert_eq!(cache.kinds[2], BlockKind::FenceClose);
        assert_eq!(cache.kinds[3], BlockKind::Paragraph);
        // A `#` inside the fence is body, not a heading.
        let b2 = buf("```\n# not a heading\n```\n");
        let c2 = BlockCache::build(&b2, true);
        assert!(matches!(c2.kinds[1], BlockKind::FenceBody { .. }));
    }

    #[test]
    fn front_matter_only_at_head() {
        let b = buf("---\ntitle: x\n---\n# Real heading\n");
        let cache = BlockCache::build(&b, true);
        assert_eq!(cache.kinds[0], BlockKind::FrontMatter);
        assert_eq!(cache.kinds[1], BlockKind::FrontMatter);
        assert_eq!(cache.kinds[2], BlockKind::FrontMatter);
        assert_eq!(cache.kinds[3], BlockKind::Heading(1));
    }

    #[test]
    fn incremental_invalidation_matches_full_rebuild() {
        let mut b = buf("# Title\n\nsome prose\n- a list\n> a quote\n");
        let mut cache = BlockCache::build(&b, true);

        // Edit line 2: turn prose into a heading.
        b.insert_str(Cursor::new(2, 0), "## ");
        cache.invalidate_from(&b, 2, 2);

        let fresh = BlockCache::build(&b, true);
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
        let mut cache = BlockCache::build(&b, true);
        assert_eq!(cache.kinds[2], BlockKind::Paragraph);

        b.insert_str(Cursor::new(2, 0), "- ");
        cache.invalidate_from(&b, 2, 2);

        assert!(
            matches!(cache.kinds[2], BlockKind::ListItem { .. }),
            "typing `- ` makes the line a list item: {:?}",
            cache.kinds[2]
        );
        assert_eq!(cache.kinds, BlockCache::build(&b, true).kinds);
    }

    #[test]
    fn incremental_invalidation_across_a_fence_edit() {
        // Opening a fence must reclassify the lines below it.
        let mut b = buf("prose\ncode line\nmore\n");
        let mut cache = BlockCache::build(&b, true);
        assert_eq!(cache.kinds[1], BlockKind::Paragraph);

        b.insert_str(Cursor::new(0, 0), "```\n");
        // Rescan from the edited first line.
        cache.invalidate_from(&b, 0, 0);

        let fresh = BlockCache::build(&b, true);
        assert_eq!(cache.kinds, fresh.kinds);
        assert_eq!(cache.kinds[0], BlockKind::FenceOpen(String::new()));
        assert!(matches!(cache.kinds[1], BlockKind::FenceBody { .. }));
    }

    #[test]
    fn multiline_math_blocks_classify_across_lines() {
        let b = buf("$$\n\\nabla \\times \\vec{F}\n$$\nprose\n");
        let cache = BlockCache::build(&b, true);
        assert_eq!(cache.kinds[0], BlockKind::MathBlock);
        assert_eq!(cache.kinds[1], BlockKind::MathBlock);
        assert_eq!(cache.kinds[2], BlockKind::MathBlock);
        assert_eq!(cache.kinds[3], BlockKind::Paragraph);

        // Blank line bounds unclosed $$:
        let unclosed = buf("$$\n\\nabla \\times \\vec{F}\n\nnormal prose\n");
        let cache_unclosed = BlockCache::build(&unclosed, true);
        assert_eq!(cache_unclosed.kinds[0], BlockKind::MathBlock);
        assert_eq!(cache_unclosed.kinds[1], BlockKind::MathBlock);
        assert_eq!(cache_unclosed.kinds[2], BlockKind::Blank);
        assert_eq!(cache_unclosed.kinds[3], BlockKind::Paragraph);
    }

    #[test]
    fn text_after_a_closed_one_line_block_makes_it_a_paragraph() {
        let k = |l: &str| classify(l, &Carry::default(), false, true);
        assert_eq!(k("$$x$$"), (BlockKind::MathBlock, Carry::default()));
        assert_eq!(k("  $$x$$  "), (BlockKind::MathBlock, Carry::default()));
        assert_eq!(k("$$E=mc^2$$ is famous"), (BlockKind::Paragraph, Carry::default()));
        assert_eq!(k("$$a$$ and $$b$$"), (BlockKind::Paragraph, Carry::default()));
        assert_eq!(k("$$ x"), (BlockKind::MathBlock, Carry::from(Open::InMath)));
        // An escaped `$` before the last one: not a closing `$$`.
        assert_eq!(k(r"$$a\$$"), (BlockKind::MathBlock, Carry::from(Open::InMath)));
        assert_eq!(
            classify(r"x = \$$", &Carry::from(Open::InMath), false, true),
            (BlockKind::MathBlock, Carry::from(Open::InMath))
        );
        assert_eq!(classify(r"x = \\$$", &Carry::from(Open::InMath), false, true), (BlockKind::MathBlock, Carry::default()));
    }

    fn kinds(text: &str) -> Vec<BlockKind> {
        BlockCache::build(&buf(text), true).kinds
    }

    const NINE: &str = "1. a\n2. b\n3. c\n4. d\n5. e\n6. f\n7. g\n8. h\n9. i\n";

    /// Under `10. ` the text starts at column 4, and so do the blocks in it.
    #[test]
    fn blocks_in_an_item_count_from_its_text_column() {
        let k = kinds(&format!("{NINE}10. j\n    ```\n    1. step\n    ```\n    $$\n    x\n    $$\n    # head\n    ---\n"));
        assert_eq!(k[10], BlockKind::FenceOpen(String::new()));
        assert!(matches!(k[11], BlockKind::FenceBody { .. }), "{:?}", k[11]);
        assert_eq!(k[12], BlockKind::FenceClose);
        assert_eq!(&k[13..16], &[BlockKind::MathBlock, BlockKind::MathBlock, BlockKind::MathBlock]);
        assert_eq!(k[16], BlockKind::Heading(1));
        assert_eq!(k[17], BlockKind::Rule);
        // 4 past the text column is indented code: prose here.
        assert_eq!(kinds(&format!("{NINE}10. j\n        ```\n"))[10], BlockKind::Paragraph);
        // Between the marker and the text column it still counts, as before.
        assert_eq!(kinds(&format!("{NINE}10. j\n   ```\n"))[10], BlockKind::FenceOpen(String::new()));
        // Two levels nested by 2: the text is at column 6.
        let k = kinds("- a\n  - b\n    - c\n      ```\n      x\n      ```\n");
        assert_eq!(k[3], BlockKind::FenceOpen(String::new()));
        assert!(matches!(k[4], BlockKind::FenceBody { .. }));
        // The item's own spacing sets its text column: `-   a` is at 4.
        assert_eq!(kinds("-   a\n    # h\n")[1], BlockKind::Heading(1));
        // `-      a` (5+ spaces) and `-` alone: one column past the marker.
        assert_eq!(kinds("-      a\n      # h\n")[1], BlockKind::Paragraph);
        assert_eq!(kinds("-\n      # h\n")[1], BlockKind::Paragraph);
    }

    /// Inside means what `Lists::bounds` says: deeper than the marker, a lazy
    /// line, or past blank lines.
    #[test]
    fn an_item_lasts_as_long_as_it_does_in_the_editor() {
        let fence = |t: &str| kinds(t.trim_end()).last().cloned().unwrap();
        let open = BlockKind::FenceOpen(String::new());
        assert_eq!(fence(&format!("{NINE}10. j\n\n\n    ```\n")), open, "past blank lines");
        assert_eq!(fence(&format!("{NINE}10. j\nlazy\n    ```\n")), open, "after a lazy line");
        assert_eq!(fence(&format!("{NINE}10. j\n    para\n\n    ```\n")), open, "after a paragraph in it");
        assert_eq!(fence(&format!("{NINE}10. j\n    ```\n    x\n    ```\n    ```\n")), open, "after a fence in it");
        assert_eq!(fence("- a\n  - b\n  ```\n  x\n  ```\n      ```\n"), BlockKind::Paragraph, "b ended at the fence");
        let prose = BlockKind::Paragraph;
        assert_eq!(fence(&format!("{NINE}10. j\n\nafter\n    ```\n")), prose, "a blank line ends laziness");
        assert_eq!(fence(&format!("{NINE}10. j\n# h\n    ```\n")), prose, "a heading ends the list");
        assert_eq!(fence(&format!("{NINE}10. j\n```\nx\n```\n    ```\n")), prose, "so does a fence at 0");
        assert_eq!(fence("    ```\n"), prose, "outside any list");
        // A lazy line in a fence is code, not the list's.
        assert_eq!(fence(&format!("{NINE}10. j\n    ```\nx\n    ```\n    ```\n")), open);
        // A table row ends an item only when it is not deeper than the marker.
        assert_eq!(fence("- a\n  | x |\n  |---|\n  # h\n").clone(), BlockKind::Heading(1));
    }

    /// Typing inside a list must leave the cache as a rebuild would: the
    /// carry now holds the open items, so random edits check the rescan.
    #[test]
    fn incremental_invalidation_matches_a_rebuild_in_lists() {
        let pool = [
            "", "- a", "  - b", "    - c", "10. j", "1. x", "   ```", "    ```", "      ```", "    $$", "$$",
            "lazy", "    para", "# h", "    # h", "  ---", "| a |", "|---|", "\tx", "\t- t", "-   w", "> q",
        ];
        let mut seed = 0x2545_f491_u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..300 {
            let lines: Vec<&str> = (0..12).map(|_| pool[next(pool.len())]).collect();
            let mut b = buf(&(lines.join("\n") + "\n"));
            let mut cache = BlockCache::build(&b, true);
            for _ in 0..6 {
                let at = next(12);
                let start = b.rope.line_to_char(at);
                let end = b.rope.line_to_char(at + 1) - 1;
                b.delete_chars(start, end);
                b.insert_str(Cursor::new(at, 0), pool[next(pool.len())]);
                cache.invalidate_from(&b, at, at);
                let fresh = BlockCache::build(&b, true);
                assert_eq!(cache.kinds, fresh.kinds, "{}", b.rope);
                assert_eq!(cache.carry, fresh.carry, "{}", b.rope);
            }
        }
    }

    /// An edit that adds or removes lines stops rescanning once the state
    /// settles, as one that keeps the count does — and still agrees with a
    /// full build, whatever it added or removed, after one edit or several.
    #[test]
    fn a_line_count_change_rescans_only_near_the_edit() {
        let mut b = buf(&"para\n\n".repeat(500));
        let mut cache = BlockCache::build(&b, true);
        b.clear_dirty();
        b.insert_str(Cursor::new(400, 2), "\n");
        let (first, last) = b.take_dirty().unwrap();
        let stop = cache.invalidate_from(&b, first, last);
        assert!(stop < 410, "rescanned to line {stop} of {}", b.line_count());
        assert_eq!(cache.kinds, BlockCache::build(&b, true).kinds);

        let pool = ["```", "```rust", "$$", "| a | b |", "|---|---|", "- item", "  more", "# h", "", "text", "> q", "a\nb", "\n\n"];
        let mut seed = 11u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..300 {
            let lines: Vec<&str> = (0..16).map(|_| pool[next(pool.len())]).collect();
            let mut b = buf(&(lines.join("\n") + "\n"));
            let mut cache = BlockCache::build(&b, true);
            b.clear_dirty();
            for _ in 0..6 {
                // One to three edits between rescans, each inserting or
                // deleting whole or partial lines.
                for _ in 0..1 + next(3) {
                    let len = b.rope.len_chars();
                    if next(2) == 0 && len > 0 {
                        let from = next(len);
                        let to = (from + 1 + next(12)).min(len);
                        b.delete_chars(from, to);
                    } else {
                        let line = next(b.line_count());
                        let piece = format!("{}\n", pool[next(pool.len())]);
                        b.insert_str(Cursor::new(line, 0), &piece);
                    }
                }
                let Some((first, last)) = b.take_dirty() else { continue };
                cache.invalidate_from(&b, first, last);
                let fresh = BlockCache::build(&b, true);
                assert_eq!(cache.kinds, fresh.kinds, "{}", b.rope);
                assert_eq!(cache.carry, fresh.carry, "{}", b.rope);
            }
        }
    }

    #[test]
    fn math_off_classifies_dollar_lines_as_without_math() {
        let b = buf("$$\n# Heading\n- item\n$$x$$\n```\n$$\n```\n");
        let cache = BlockCache::build(&b, false);
        assert_eq!(cache.kinds[0], BlockKind::Paragraph);
        assert_eq!(cache.kinds[1], BlockKind::Heading(1));
        assert!(matches!(cache.kinds[2], BlockKind::ListItem { .. }));
        assert_eq!(cache.kinds[3], BlockKind::Paragraph);
        assert_eq!(cache.kinds[4], BlockKind::FenceOpen(String::new()));
        assert!(matches!(cache.kinds[5], BlockKind::FenceBody { .. }));
        assert_eq!(cache.kinds[6], BlockKind::FenceClose);
        assert!(!cache.carry.contains(&Carry::from(Open::InMath)));
    }
}
