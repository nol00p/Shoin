//! Markdown to one self-contained HTML page. SPEC.md §14.4.
//!
//! Unlike PDF, this is NOT shelled out. `pandoc` exists because typesetting a
//! page is a hard problem shoin has no business solving; turning the block
//! kinds and inline spans it already computes into tags is not one, and making
//! HTML the only format that needs a tool installed would break the promise
//! that everything except PDF works on a bare machine.
//!
//! Two properties the output holds to:
//!
//!   - **One file, no assets.** The CSS is inlined and there is no script, no
//!     font and no image fetch. What you export is what you can email.
//!   - **It looks like the editor it came from.** The page is drawn from the
//!     live `[theme]` and `layout.measure`, so a document exported from a
//!     customised shoin arrives in that shoin's colors and column width — down
//!     to the syntax highlighting inside fences, which reuses `markdown::code`
//!     rather than a second, differently-opinionated highlighter.

use crate::config::schema::MarkdownConfig;
use crate::render::markdown::block::{classify, has_cell_separator, leading_ws_cols, BlockKind, Carry, Marker, Open};
use crate::render::markdown::code::{self, Class, Cont, Lang};
use crate::render::markdown::inline::{self, Inline};
use crate::render::theme::{Color, Theme};

/// How the page should look: the editor's own theme and measure.
#[derive(Clone)]
pub struct Page {
    pub theme: Theme,
    /// Text column width, in characters — `layout.measure`.
    pub measure: u16,
    /// The document's own directory. A relative `![](photo.png)` is written
    /// against the DOCUMENT, not against wherever the export was run from, so
    /// inlining it needs to know where the document lives.
    pub base: std::path::PathBuf,
    /// Stretch every wrapped line but a paragraph's last to fill `measure` —
    /// the browser's own `text-align:justify`, which already leaves the last
    /// line of a block ragged, so it needs no extra math unlike the plain-text
    /// path (`export::plain`). Never applied to a fenced block or a table,
    /// whose columns are the author's own layout.
    pub justify: bool,
    /// The syntax the page is written with: `MarkdownConfig::for_export`,
    /// which is every syntax on unless `export_follows_settings` asks for the
    /// editor's own settings.
    pub md: MarkdownConfig,
}

impl Default for Page {
    fn default() -> Self {
        Page {
            theme: Theme::default(),
            measure: 72,
            base: std::path::PathBuf::new(),
            justify: false,
            md: MarkdownConfig::default(),
        }
    }
}

/// Convert a Markdown document to a complete HTML page.
///
/// `fallback_title` names the page when the document has no `# Heading` of its
/// own to be named after — normally the file's stem.
pub fn render(text: &str, fallback_title: &str, page: &Page) -> String {
    let md = &page.md;
    let body = Body::new(md, &page.base).run(text);
    let title = first_heading(text, md).unwrap_or_else(|| fallback_title.to_string());

    let mut out = String::with_capacity(body.len() + 4096);
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!("<title>{}</title>\n", escape(&title)));
    out.push_str("<style>\n");
    out.push_str(&stylesheet(page));
    let main_class = if page.justify { " class=\"justify\"" } else { "" };
    out.push_str(&format!("</style>\n</head>\n<body>\n<main{main_class}>\n"));
    out.push_str(&body);
    out.push_str("</main>\n</body>\n</html>\n");
    out
}

/// The document's own title: its first heading, at whatever level.
fn first_heading(text: &str, md: &MarkdownConfig) -> Option<String> {
    let mut carry = Carry::default();
    for (i, line) in text.lines().enumerate() {
        let (kind, next) = classify(line, &carry, i == 0, md.math);
        let in_list = next.in_list();
        carry = next;
        // A heading inside a list item heads that item, not the page.
        if let (BlockKind::Heading(_), false) = (&kind, in_list) {
            let body = line.trim_start().trim_start_matches('#').trim();
            return Some(plain_text(body, md));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// The body
// ---------------------------------------------------------------------------

/// One open `<ul>`/`<ol>`, and how deeply its items were indented.
struct Level {
    ordered: bool,
    indent: u8,
}

/// The line-by-line walk. Markdown's blocks nest and Shoin's classifier is
/// per-line, so the grouping — which lines join into one paragraph, which items
/// belong to which list — is this type's whole job.
struct Body<'a> {
    md: &'a MarkdownConfig,
    /// The document's directory, for resolving relative image sources.
    base: &'a std::path::Path,
    out: String,
    /// Lines of the paragraph being accumulated, or of the current list item's
    /// lazy continuation.
    para: Vec<ParaLine>,
    /// Heading ids given so far, so a repeated heading gets `-1`, `-2`.
    ids: std::collections::HashSet<String>,
    lists: Vec<Level>,
    /// Whether an `<li>` element is open and still awaiting its `</li>`.
    item_open: bool,
    /// Whether that item already holds text or a block, so further text in
    /// it is a paragraph of its own rather than the item's first line.
    item_filled: bool,
    /// Whether that item is still taking continuation lines. A blank line ends
    /// the LAZY continuation without closing the item — that is what makes a
    /// loose list one list rather than two.
    lazy: bool,
    quote: u8,
    /// Raw lines of the table being accumulated.
    table: Vec<String>,
    /// Raw lines of the display math block being accumulated.
    math_block: Vec<String>,
    /// The open fence: its language, the lexer state, and its indent, which
    /// is stripped from its lines.
    fence: Option<(Option<Lang>, Cont, usize)>,
}

/// One line of a paragraph: its text with trailing whitespace gone, and the
/// text to use when it ends in a hard line break (two spaces, or a
/// backslash, which then goes).
struct ParaLine {
    text: String,
    brk: Option<String>,
}

impl ParaLine {
    fn new(line: &str) -> Self {
        let text = line.trim_end();
        let spaces = line.len() - line.trim_end_matches(' ').len();
        let slashes = text.len() - text.trim_end_matches('\\').len();
        let brk = if spaces >= 2 {
            Some(text.to_string())
        } else if spaces == 0 && slashes % 2 == 1 {
            Some(text[..text.len() - 1].to_string())
        } else {
            None
        };
        ParaLine { text: text.to_string(), brk }
    }
}

impl<'a> Body<'a> {
    fn new(md: &'a MarkdownConfig, base: &'a std::path::Path) -> Self {
        Body {
            md,
            base,
            out: String::new(),
            para: Vec::new(),
            ids: std::collections::HashSet::new(),
            lists: Vec::new(),
            item_open: false,
            item_filled: false,
            lazy: false,
            quote: 0,
            table: Vec::new(),
            math_block: Vec::new(),
            fence: None,
        }
    }

    fn run(mut self, text: &str) -> String {
        let mut carry = Carry::default();
        for (i, line) in text.lines().enumerate() {
            let (kind, next) = classify(line, &carry, i == 0, self.md.math);
            // `tables = false` turns a table back into the prose it also is,
            // as the editor does.
            let kind = match kind {
                BlockKind::Table if !self.md.tables => BlockKind::Paragraph,
                other => other,
            };
            // The classifier's word decides where a math block ends, so this
            // walk cannot group lines differently from the editor.
            let math_continues = next.open == Open::InMath;
            carry = next;
            self.line(line, &kind, math_continues);
        }
        self.close_all();
        self.out
    }

    fn line(&mut self, line: &str, kind: &BlockKind, math_continues: bool) {
        match kind {
            // Front matter is metadata for tools, not text for readers — the
            // same call plain text makes.
            BlockKind::FrontMatter => {}

            BlockKind::FenceOpen(info) => {
                let indent = leading_ws_cols(line);
                self.open_block(indent);
                let lang = if self.md.code_syntax { Lang::from_info(info) } else { None };
                // The `language-x` class is emitted for ANY info string, known
                // or not: it is what a reader's own stylesheet, or a highlighter
                // they run over the page later, looks for.
                let word = info.split_whitespace().next().unwrap_or("");
                let class = match word.is_empty() {
                    true => String::new(),
                    false => format!(" class=\"language-{}\"", escape_attr(word)),
                };
                self.out.push_str(&format!("<pre><code{class}>"));
                self.fence = Some((lang, Cont::None, indent));
            }
            BlockKind::FenceBody { .. } => {
                // The state is taken from the fence we opened, not from the
                // kind: an exported document is walked from the top, so this
                // walk's own lexer state is the authoritative one.
                let (lang, cont, indent) = self.fence.unwrap_or((None, Cont::None, 0));
                // As in CommonMark, a fence indented N columns takes up to N
                // columns of indent off each of its lines: code nested in an
                // item lines up with the item, not the page.
                let strip = leading_ws_cols(line).min(indent);
                let line: String = line.chars().skip(strip).collect();
                let (html, next) = highlight(&line, lang, cont);
                self.out.push_str(&html);
                self.out.push('\n');
                self.fence = Some((lang, next, indent));
            }
            BlockKind::FenceClose => {
                self.out.push_str("</code></pre>\n");
                self.fence = None;
            }

            BlockKind::Blank => {
                // A blank line ends a paragraph, a quote and a table — but NOT
                // a list: a blank line between two items is a loose list, and
                // closing here would split it into two.
                self.flush_para();
                self.flush_table();
                self.flush_math_block();
                self.close_quote();
                self.lazy = false;
            }

            BlockKind::Heading(level) => {
                self.open_block(leading_ws_cols(line));
                let body = line.trim_start().trim_start_matches('#').trim();
                let id = self.unique_id(slug(&plain_text(body, self.md)));
                let n = (*level).clamp(1, 6);
                self.out.push_str(&format!(
                    "<h{n} id=\"{}\">{}</h{n}>\n",
                    escape_attr(&id),
                    inline_html(body, self.md, self.base)
                ));
            }

            BlockKind::Rule => {
                self.open_block(leading_ws_cols(line));
                self.out.push_str("<hr>\n");
            }

            BlockKind::Quote(depth) => {
                self.flush_para();
                self.flush_table();
                self.flush_math_block();
                self.close_lists();
                self.open_quote(*depth);
                let body = line.trim_start().trim_start_matches(['>', ' ']);
                // A bare `>` only separates the quote's paragraphs.
                if !body.trim().is_empty() {
                    self.para.push(ParaLine::new(body));
                }
            }

            BlockKind::ListItem { depth, marker, checked } => {
                self.flush_para();
                self.flush_table();
                self.flush_math_block();
                self.close_quote();
                self.adjust_lists(*depth, matches!(marker, Marker::Ordered));
                let box_html = match checked {
                    Some(true) => "<input type=\"checkbox\" checked disabled> ",
                    Some(false) => "<input type=\"checkbox\" disabled> ",
                    None => "",
                };
                let class = if checked.is_some() { " class=\"task\"" } else { "" };
                self.out.push_str(&format!("<li{class}>{box_html}"));
                self.item_open = true;
                self.item_filled = false;
                self.lazy = true;
                self.para.push(ParaLine::new(super::item_body(line)));
            }

            BlockKind::Table => {
                // A table's HEADER row sits above the delimiter row that
                // identifies it, and `classify` only ever looks forward — so
                // the header arrived here as a paragraph line. `BlockCache`
                // promotes it in the editor by looking back one line; this walk
                // does the same, from the paragraph it is still holding.
                let header = match self.table.is_empty() {
                    true => self.para.last().map(|l| l.text.clone()).filter(|l| has_cell_separator(l)),
                    false => None,
                };
                if header.is_some() {
                    self.para.pop();
                }
                self.flush_para();
                self.flush_math_block();
                self.close_quote();
                self.close_lists();
                self.table.extend(header);
                self.table.push(line.to_string());
            }

            // A paragraph line inside an open item is that item's continuation
            // (CommonMark calls it lazy continuation); anywhere else it starts
            // or extends a paragraph.
            BlockKind::Paragraph => {
                self.flush_table();
                self.flush_math_block();
                // Not lazy, it stays in the items it is indented deeper than.
                if !self.lazy && self.quote == 0 {
                    self.close_lists_from(leading_ws_cols(line));
                }
                self.para.push(ParaLine::new(line.trim_start()));
            }

            // Only reached with `math` on: off, the classifier never opens one.
            BlockKind::MathBlock => {
                if self.math_block.is_empty() {
                    self.flush_para();
                    self.flush_table();
                    self.close_quote();
                    // Directly after an item's text the block joins that
                    // item. Anywhere else it stays only in the items it is
                    // indented deeper than, and the rest close: the same call
                    // `Lists::bounds` makes in the editor.
                    if !self.lazy {
                        self.close_lists_from(leading_ws_cols(line));
                    }
                }
                self.math_block.push(line.to_string());
                if !math_continues {
                    self.flush_math_block();
                    // A paragraph line after the block starts afresh rather
                    // than continuing the item, as in the editor.
                    self.lazy = false;
                }
            }
        }
    }

    /// `id`, or `id-1`, `id-2`… if a heading already has it (GitHub's rule).
    fn unique_id(&mut self, id: String) -> String {
        let mut out = id.clone();
        let mut n = 0;
        while self.ids.contains(&out) {
            n += 1;
            out = format!("{id}-{n}");
        }
        self.ids.insert(out.clone());
        out
    }

    /// Emit whatever text has accumulated, as the thing it belongs to.
    fn flush_para(&mut self) {
        if self.para.is_empty() {
            return;
        }
        // A hard line break splits the text into runs, each rendered on its
        // own: a span does not cross a `<br>`.
        let lines = std::mem::take(&mut self.para);
        let last = lines.len() - 1;
        let mut runs: Vec<String> = Vec::new();
        let mut run: Vec<&str> = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            match &l.brk {
                Some(text) if i < last => {
                    run.push(text);
                    runs.push(inline_html(run.join(" ").trim(), self.md, self.base));
                    run.clear();
                }
                _ => run.push(&l.text),
            }
        }
        runs.push(inline_html(run.join(" ").trim(), self.md, self.base));
        let html = runs.join("<br>\n");
        // Inside an item the text IS the item; everywhere else — including
        // inside a blockquote — it is a paragraph of its own.
        if self.item_open && !self.item_filled {
            self.out.push_str(&html);
            self.item_filled = true;
        } else {
            self.out.push_str(&format!("<p>{html}</p>\n"));
        }
    }

    fn close_item(&mut self) {
        if self.item_open {
            self.flush_para();
            self.out.push_str("</li>\n");
            self.item_open = false;
        }
        self.lazy = false;
    }

    /// Bring the open lists into line with an item at `indent`.
    ///
    /// The subtlety is where a nested list lives: INSIDE its parent's `<li>`,
    /// which therefore stays open across the whole child list and is closed by
    /// the `</ul>` that ends it. Getting that wrong produces a page that
    /// balances tag for tag and still nests wrongly, which no count of opens
    /// against closes will catch.
    fn adjust_lists(&mut self, indent: u8, ordered: bool) {
        while self.lists.last().is_some_and(|l| l.indent > indent) {
            self.pop_list();
        }
        // A different marker at the same depth is a different list.
        if self
            .lists
            .last()
            .is_some_and(|l| l.indent == indent && l.ordered != ordered)
        {
            self.pop_list();
        }
        match self.lists.last() {
            Some(l) if l.indent >= indent => self.close_item(),
            _ => {
                self.out.push_str(if ordered { "<ol>\n" } else { "<ul>\n" });
                self.lists.push(Level { ordered, indent });
                // The new list's first `<li>` has not opened yet; the parent's,
                // if there is one, is held open around it.
                self.item_open = false;
            }
        }
    }

    /// Close the innermost list, and with it the item it was nested in.
    fn pop_list(&mut self) {
        self.close_item();
        let Some(l) = self.lists.pop() else { return };
        self.out.push_str(if l.ordered { "</ol>\n" } else { "</ul>\n" });
        // Whatever list is left below has an item open around what just closed.
        self.item_open = !self.lists.is_empty();
        self.item_filled = true;
        self.lazy = self.item_open;
    }

    /// Close the lists whose items a line at `indent` is not deeper than:
    /// it stays in the rest, as in the editor (`Lists::bounds`).
    fn close_lists_from(&mut self, indent: usize) {
        while self.lists.last().is_some_and(|l| usize::from(l.indent) >= indent) {
            self.pop_list();
        }
        if self.lists.is_empty() {
            self.close_item();
        }
    }

    /// Start a block that is not text (a fence, heading or rule): whatever
    /// is pending ends, and it goes in the items it is indented deeper than.
    /// It never joins an item lazily, and text after it does not either.
    fn open_block(&mut self, indent: usize) {
        self.flush_para();
        self.flush_table();
        self.flush_math_block();
        self.close_quote();
        self.close_lists_from(indent);
        if self.item_open {
            self.item_filled = true;
        }
        self.lazy = false;
    }

    fn close_lists(&mut self) {
        while !self.lists.is_empty() {
            self.pop_list();
        }
        self.close_item();
    }

    fn open_quote(&mut self, depth: u8) {
        let depth = depth.max(1);
        while self.quote > depth {
            self.flush_para();
            self.out.push_str("</blockquote>\n");
            self.quote -= 1;
        }
        while self.quote < depth {
            self.out.push_str("<blockquote>\n");
            self.quote += 1;
        }
    }

    fn close_quote(&mut self) {
        if self.quote == 0 {
            return;
        }
        self.flush_para();
        while self.quote > 0 {
            self.out.push_str("</blockquote>\n");
            self.quote -= 1;
        }
    }

    /// A table is emitted whole, because its shape is decided by the delimiter
    /// row in the middle of it: the row above is a header, and the alignments
    /// belong to every row below.
    fn flush_table(&mut self) {
        if self.table.is_empty() {
            return;
        }
        let rows: Vec<Vec<String>> = self.table.iter().map(|l| cells(l)).collect();
        let delim = self.table.iter().position(|l| is_delimiter(l));
        let align: Vec<&'static str> = match delim {
            Some(i) => rows[i].iter().map(|c| alignment(c)).collect(),
            None => Vec::new(),
        };
        let at = |i: usize| align.get(i).copied().unwrap_or("");
        let base = self.base;
        let cell = |tag: &str, i: usize, text: &str, md: &MarkdownConfig| {
            let a = at(i);
            let style = if a.is_empty() { String::new() } else { format!(" style=\"text-align:{a}\"") };
            format!("<{tag}{style}>{}</{tag}>", inline_html(text, md, base))
        };

        self.out.push_str("<table>\n");
        for (r, row) in rows.iter().enumerate() {
            if delim == Some(r) {
                continue;
            }
            let header = delim.is_some_and(|d| r < d);
            if header && r == 0 {
                self.out.push_str("<thead>\n");
            }
            self.out.push_str("<tr>");
            for (i, text) in row.iter().enumerate() {
                self.out.push_str(&cell(if header { "th" } else { "td" }, i, text, self.md));
            }
            self.out.push_str("</tr>\n");
            if header && delim == Some(r + 1) {
                self.out.push_str("</thead>\n<tbody>\n");
            }
        }
        if delim.is_some() {
            self.out.push_str("</tbody>\n");
        }
        self.out.push_str("</table>\n");
        self.table.clear();
    }

    fn flush_math_block(&mut self) {
        if self.math_block.is_empty() {
            return;
        }
        let raw = self.math_block.join("\n");
        self.math_block.clear();
        let trimmed = raw.trim();
        let without_open = trimmed.strip_prefix("$$").unwrap_or(trimmed);
        let inner = without_open.strip_suffix("$$").unwrap_or(without_open).trim();
        self.out.push_str(&format!(
            "<div class=\"math display\">\\[{}\\]</div>\n",
            escape(inner)
        ));
        if self.item_open {
            self.item_filled = true;
        }
    }

    fn close_all(&mut self) {
        self.flush_para();
        self.flush_table();
        self.flush_math_block();
        self.close_quote();
        self.close_lists();
        // A document may simply END inside a fence — an unterminated block is
        // valid Markdown and the editor renders it as code to the last line, so
        // the page has to close the block the document did not.
        if self.fence.take().is_some() {
            self.out.push_str("</code></pre>\n");
        }
    }
}

// ---------------------------------------------------------------------------
// Inline
// ---------------------------------------------------------------------------

/// One line's inline markup as HTML.
fn inline_html(line: &str, md: &MarkdownConfig, base: &std::path::Path) -> String {
    inline_in(line, md, base, false)
}

/// `inline_html`, with `in_link` set inside a link's text.
///
/// `inline::scan` returns flat spans, so markup INSIDE a span — a link in
/// bold, bold in a link — is reached by scanning that span's body again. Only
/// prose bodies are: code, math, tags and an image's alt text are what they
/// say. In a link's text a nested link is its text, since HTML forbids an
/// anchor inside an anchor.
fn inline_in(line: &str, md: &MarkdownConfig, base: &std::path::Path, in_link: bool) -> String {
    let chars: Vec<char> = line.chars().collect();
    let spans = inline::scan(line, md);
    if spans.is_empty() {
        return escape(line);
    }
    let mut out = String::new();
    let mut i = 0usize;
    for sp in &spans {
        if sp.outer.start < i {
            continue;
        }
        out.push_str(&escape_chars(&chars[i..sp.outer.start]));
        let inner: String = chars[sp.inner.clone()].iter().collect();
        let whole: String = chars[sp.outer.clone()].iter().collect();
        let prose = || inline_in(&inner, md, base, in_link);
        match sp.kind {
            Inline::Bold => out.push_str(&format!("<strong>{}</strong>", prose())),
            Inline::Italic => out.push_str(&format!("<em>{}</em>", prose())),
            Inline::BoldItalic => out.push_str(&format!("<strong><em>{}</em></strong>", prose())),
            Inline::Strikethrough => out.push_str(&format!("<del>{}</del>", prose())),
            Inline::Highlight => out.push_str(&format!("<mark>{}</mark>", prose())),
            Inline::Code => out.push_str(&format!("<code>{}</code>", escape(&inner))),
            Inline::Link if in_link => out.push_str(&prose()),
            Inline::Link => {
                let (url, title) = inline::destination(sp, line).unwrap_or_else(|| (inner.clone(), None));
                let text = inline_in(&inner, md, base, true);
                out.push_str(&format!("<a href=\"{}\"{}>{text}</a>", safe_href(&url), title_attr(title)));
            }
            Inline::Autolink if in_link => out.push_str(&escape(&inner)),
            Inline::Autolink => {
                out.push_str(&format!("<a href=\"{}\">{}</a>", safe_href(&inner), escape(&inner)));
            }
            Inline::Image => {
                let (url, title) = inline::destination(sp, line).unwrap_or_default();
                out.push_str(&format!(
                    "<img src=\"{}\" alt=\"{}\"{}>",
                    safe_src(&inline_src(&url, base)),
                    escape(&inner),
                    title_attr(title)
                ));
            }
            Inline::WikiLink if in_link => out.push_str(&escape(wiki_parts(&inner).2)),
            Inline::WikiLink => out.push_str(&wiki_link(&inner)),
            Inline::Tag => out.push_str(&format!("<span class=\"tag\">{}</span>", escape(&whole))),
            Inline::Latex => {
                if whole.starts_with("$$") && whole.ends_with("$$") {
                    out.push_str(&format!("<span class=\"math display\">\\[{}\\]</span>", escape(&inner)));
                } else {
                    out.push_str(&format!("<span class=\"math inline\">\\({}\\)</span>", escape(&inner)));
                }
            }
            // The backslash was markup; what it protected is literal text.
            Inline::Escape => out.push_str(&escape(&inner)),
        }
        i = sp.outer.end;
    }
    out.push_str(&escape_chars(&chars[i.min(chars.len())..]));
    out
}

/// A wiki link's `(note, heading, label)`: `|` renames it, `#` names a
/// heading, as in the editor.
fn wiki_parts(inner: &str) -> (&str, Option<&str>, &str) {
    let (target, label) = match inner.split_once('|') {
        Some((t, l)) => (t.trim(), l.trim()),
        None => (inner.trim(), inner.trim()),
    };
    match target.split_once('#') {
        Some((f, s)) => (f.trim(), Some(s.trim()), label),
        None => (target, None, label),
    }
}

/// A `[[wiki link]]` as an anchor.
///
/// It points at `target.html`, on the assumption that a set of notes exported
/// to HTML lands beside each other — the only guess available, since a wiki
/// link names a NOTE and HTML has only addresses. `|` renames it and `#` is a
/// heading anchor, exactly as in the editor.
///
/// A target with a scheme in it (`[[javascript:…]]`, `[[https://…]]`) is not a
/// note name, so it is not a link either: the label is kept as plain text. A
/// `:` counts as a scheme's when it comes before the first `/`, which is how a
/// browser would read it; `[[a/b:c]]` is a path and stays a link. Nor is
/// `[[//host/x]]`, which a browser reads as an address on another host.
fn wiki_link(inner: &str) -> String {
    let (file, section, label) = wiki_parts(inner);
    if file.split('/').next().is_some_and(|head| head.contains(':')) || off_host(file) {
        return escape(label);
    }
    let mut href = String::new();
    if !file.is_empty() {
        href.push_str(&format!("{file}.html"));
    }
    if let Some(s) = section {
        href.push('#');
        href.push_str(&slug(s));
    }
    format!("<a class=\"wiki\" href=\"{}\">{}</a>", safe_href(&href), escape(label))
}

/// Whether a browser reads `file` as `//host/…`, an address on another host:
/// it skips leading controls and spaces, drops tabs and newlines, and takes
/// `\\` for `/`.
fn off_host(file: &str) -> bool {
    let mut seps = file
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'));
    matches!((seps.next(), seps.next()), (Some('/' | '\\'), Some('/' | '\\')))
}

/// Inline markup removed rather than translated — for a `<title>` and for the
/// heading ids, neither of which can hold tags. Prose bodies are stripped
/// again, as `inline_in` renders them again.
fn plain_text(line: &str, md: &MarkdownConfig) -> String {
    let chars: Vec<char> = line.chars().collect();
    let spans = inline::scan(line, md);
    if spans.is_empty() {
        return line.to_string();
    }
    let mut out = String::new();
    let mut i = 0usize;
    for sp in &spans {
        if sp.outer.start < i {
            continue;
        }
        out.extend(&chars[i..sp.outer.start]);
        let inner: String = chars[sp.inner.clone()].iter().collect();
        match sp.kind {
            Inline::Bold
            | Inline::Italic
            | Inline::BoldItalic
            | Inline::Strikethrough
            | Inline::Highlight
            | Inline::Link => out.push_str(&plain_text(&inner, md)),
            Inline::WikiLink => out.push_str(wiki_parts(&inner).2),
            _ => out.push_str(&inner),
        }
        i = sp.outer.end;
    }
    out.extend(&chars[i.min(chars.len())..]);
    out
}

/// ` title="…"` for a link or picture that has one.
fn title_attr(title: Option<String>) -> String {
    title.map(|t| format!(" title=\"{}\"", escape_attr(&t))).unwrap_or_default()
}

/// A heading's anchor: lowercase, words joined by hyphens. The GitHub form,
/// which is what a reader linking to `#a-section` will have typed.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if matches!(c, ' ' | '-' | '_') && !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

/// A row's cells, split on unescaped `|` with the outer pair dropped.
fn cells(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut escaped = false;
    for c in line.chars() {
        match c {
            '\\' if !escaped => {
                escaped = true;
                cur.push(c);
            }
            '|' if !escaped => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => {
                escaped = false;
                cur.push(c);
            }
        }
    }
    out.push(cur.trim().to_string());
    if out.first().is_some_and(|s| s.is_empty()) {
        out.remove(0);
    }
    if out.last().is_some_and(|s| s.is_empty()) {
        out.pop();
    }
    out
}

fn is_delimiter(line: &str) -> bool {
    let body = line.trim();
    !body.is_empty()
        && body.contains('-')
        && body.contains('|')
        && body.chars().all(|c| matches!(c, '-' | ':' | '|' | ' ' | '\t'))
}

fn alignment(cell: &str) -> &'static str {
    match (cell.starts_with(':'), cell.ends_with(':')) {
        (true, true) => "center",
        (false, true) => "right",
        (true, false) => "left",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Code
// ---------------------------------------------------------------------------

/// One fenced line as highlighted HTML, and the lexer state leaving it.
fn highlight(line: &str, lang: Option<Lang>, cont: Cont) -> (String, Cont) {
    let Some(lang) = lang else {
        return (escape(line), Cont::None);
    };
    let mut tokens = Vec::new();
    let next = code::scan(line, lang, cont, &mut tokens);
    let chars: Vec<char> = line.chars().collect();

    let mut out = String::new();
    let mut at = 0usize;
    for t in tokens {
        out.push_str(&escape_chars(&chars[at..t.range.start]));
        let text = escape_chars(&chars[t.range.clone()]);
        let class = match t.class {
            Class::Comment => "c",
            Class::Str => "s",
            Class::Literal => "n",
            Class::Keyword => "k",
            Class::Type => "t",
            Class::Function => "f",
            Class::Punct => "o",
        };
        out.push_str(&format!("<span class=\"{class}\">{text}</span>"));
        at = t.range.end;
    }
    out.push_str(&escape_chars(&chars[at.min(chars.len())..]));
    (out, next)
}

// ---------------------------------------------------------------------------
// Escaping and CSS
// ---------------------------------------------------------------------------

/// Text as HTML. Everything that reaches the page goes through here — the
/// document is someone's prose, and prose contains `<`, `&` and quotes.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_chars(chars: &[char]) -> String {
    escape(&chars.iter().collect::<String>())
}
/// A local image, turned into a `data:` URI so the page really is one file.
///
/// SPEC §14.4 calls HTML export "self-contained", and a page that reaches back
/// to `~/notes/photo.png` is not — it breaks the moment it is sent to anyone.
/// A URL, or a file that cannot be read, is left exactly as written: those are
/// the cases where the author meant a reference.
fn inline_src(url: &str, base: &std::path::Path) -> String {
    if url.is_empty() || url.contains("://") || url.starts_with("data:") {
        return url.to_string();
    }
    let path = std::path::Path::new(url);
    if !crate::image::looks_like_image(path) {
        return url.to_string();
    }
    // Two relative spellings reach here, and both have to work:
    //
    //   * what the AUTHOR wrote — `![](photo.png)` means "beside this file", so
    //     it resolves against the document;
    //   * what an EMBED compiled to — `![[photo]]` was resolved through the
    //     composition's own path, which leaves it relative to the working
    //     directory instead.
    //
    // Document first, because that is the one the author controls. A src that
    // answers to neither is left exactly as written.
    let bytes = if path.is_absolute() {
        std::fs::read(path)
    } else {
        std::fs::read(base.join(path)).or_else(|_| std::fs::read(path))
    };
    let Ok(bytes) = bytes else {
        return url.to_string();
    };
    let mime = match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        _ => return url.to_string(),
    };
    format!("data:{mime};base64,{}", crate::image::base64(&bytes))
}


/// An attribute value, which additionally must not close its own quote.
fn escape_attr(s: &str) -> String {
    escape(s).replace('\'', "&#39;")
}

/// An `href`, with any scheme that executes taken out.
///
/// This module promises a page with no script in it, and `escape_attr` alone
/// does not keep that promise: `[run](javascript:…)` breaks no quoting rule and
/// still runs code the moment a reader clicks it. Notes get pasted in from the
/// web and pulled in through `![[…]]` from files their author never read, so
/// "it is only your own writing" is not a guarantee this can rest on.
///
/// The refusal is VISIBLE — the link keeps its text and loses its destination,
/// rather than quietly pointing somewhere else.
fn safe_href(url: &str) -> String {
    // A scheme is what precedes the first `:`, and only when nothing before it
    // could make the `:` part of a path or a query. Everything relative —
    // `notes.md`, `#anchor`, `/img/a.png` — has no scheme and is fine.
    let scheme: String = url
        .chars()
        .take_while(|c| *c != ':' && *c != '/' && *c != '?' && *c != '#')
        .collect();
    let rest = url.len() > scheme.len() && url[scheme.len()..].starts_with(':');
    if !rest {
        return escape_attr(url);
    }
    match scheme.trim().to_ascii_lowercase().as_str() {
        "http" | "https" | "mailto" | "ftp" | "file" | "tel" => escape_attr(url),
        // `data:` is included: `data:text/html` in an href is a script vector
        // just as `javascript:` is. Images go through `inline_src`, which
        // builds its own `data:` URI and never reaches here.
        _ => "#".to_string(),
    }
}

/// An image's `src`: what `safe_href` allows, plus an inline `data:image/…`
/// (which is what `inline_src` builds, and which an `<img>` never runs).
fn safe_src(url: &str) -> String {
    let lower = url.trim_start().to_ascii_lowercase();
    if lower.starts_with("data:image/") {
        escape_attr(url)
    } else {
        safe_href(url)
    }
}

/// The page's stylesheet, written from the theme.
fn stylesheet(page: &Page) -> String {
    let t = &page.theme;
    let c = |color: Color, fallback: &str| color.to_css().unwrap_or_else(|| fallback.to_string());

    let mut css = String::new();
    css.push_str(":root{\n");
    let vars: [(&str, String); 20] = [
        ("bg", c(t.background, "#1a1b26")),
        ("fg", c(t.text, "#c0caf5")),
        ("dim", c(t.text_dim, "#565f89")),
        ("h1", c(t.headings[0], "#f7768e")),
        ("h2", c(t.headings[1], "#9ece6a")),
        ("h3", c(t.headings[2], "#e0af68")),
        ("h4", c(t.headings[3], "#7aa2f7")),
        ("h5", c(t.headings[4], "#bb9af7")),
        ("h6", c(t.headings[5], "#7dcfff")),
        ("code", c(t.code, "#9ece6a")),
        ("code-bg", c(t.code_bg, "#24283b")),
        ("bar", c(t.fence_bar, "#414868")),
        ("link", c(t.link, "#7aa2f7")),
        ("wiki", c(t.wiki_link, "#bb9af7")),
        ("tag", c(t.tag, "#7dcfff")),
        ("quote", c(t.quote, "#9aa5ce")),
        ("bullet", c(t.list_bullet, "#7aa2f7")),
        ("rule", c(t.rule, "#414868")),
        ("border", c(t.table_border, "#414868")),
        ("mark", c(t.highlight_bg, "#3d59a1")),
    ];
    for (k, v) in &vars {
        css.push_str(&format!("  --{k}: {v};\n"));
    }
    let syntax: [(&str, Color); 7] = [
        ("k", t.syntax_keyword),
        ("t", t.syntax_type),
        ("s", t.syntax_string),
        ("n", t.syntax_literal),
        ("c", t.syntax_comment),
        ("f", t.syntax_function),
        ("o", t.syntax_punct),
    ];
    for (k, v) in syntax {
        css.push_str(&format!("  --syn-{k}: {};\n", c(v, "#c0caf5")));
    }
    css.push_str("}\n");

    // The measure is the editor's, in characters, so an exported document keeps
    // the column the writer set it in.
    css.push_str(&format!(
        "\
*{{box-sizing:border-box}}
body{{margin:0;background:var(--bg);color:var(--fg);
  font:16px/1.7 ui-serif,Georgia,'Iowan Old Style',serif}}
main{{max-width:{measure}ch;margin:0 auto;padding:4rem 1.5rem 8rem}}
h1,h2,h3,h4,h5,h6{{line-height:1.25;margin:2.5em 0 .6em;font-weight:700}}
h1{{color:var(--h1);font-size:1.9em;margin-top:0}}
h2{{color:var(--h2);font-size:1.5em}}
h3{{color:var(--h3);font-size:1.25em}}
h4{{color:var(--h4);font-size:1.1em}}
h5{{color:var(--h5);font-size:1em}}
h6{{color:var(--h6);font-size:1em}}
p{{margin:0 0 1.2em}}
a{{color:var(--link);text-decoration:underline;text-underline-offset:2px}}
a.wiki{{color:var(--wiki)}}
.tag{{color:var(--tag)}}
mark{{background:var(--mark);color:var(--fg);padding:0 .15em;border-radius:2px}}
del{{color:var(--dim)}}
hr{{border:0;border-top:1px solid var(--rule);margin:3em 0}}
blockquote{{margin:1.5em 0;padding:0 0 0 1.2em;border-left:3px solid var(--bar);
  color:var(--quote);font-style:italic}}
blockquote blockquote{{margin:.6em 0}}
ul,ol{{margin:0 0 1.2em;padding-left:1.6em}}
li{{margin:.3em 0}}
li::marker{{color:var(--bullet)}}
li.task{{list-style:none;margin-left:-1.2em}}
li.task input{{margin-right:.5em;accent-color:var(--bullet)}}
code{{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:.9em;
  background:var(--code-bg);color:var(--code);padding:.1em .35em;border-radius:3px}}
pre{{background:var(--code-bg);border-left:3px solid var(--bar);border-radius:3px;
  padding:1em 1.2em;overflow-x:auto;margin:1.5em 0}}
pre code{{background:none;padding:0;color:var(--fg);font-size:.875em;line-height:1.55}}
table{{border-collapse:collapse;margin:1.5em 0;width:100%;font-size:.95em}}
th,td{{border:1px solid var(--border);padding:.4em .7em;text-align:left}}
th{{color:var(--fg);font-weight:700}}
img{{max-width:100%;height:auto}}
main.justify p,main.justify li,main.justify blockquote{{text-align:justify}}
main.justify pre,main.justify table{{text-align:left}}
.k{{color:var(--syn-k)}}
.t{{color:var(--syn-t)}}
.s{{color:var(--syn-s)}}
.n{{color:var(--syn-n)}}
.c{{color:var(--syn-c);font-style:italic}}
.f{{color:var(--syn-f)}}
.o{{color:var(--syn-o)}}
",
        measure = page.measure.max(20)
    ));
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(md: &str) -> String {
        Body::new(&MarkdownConfig::default(), std::path::Path::new("")).run(md)
    }

    /// A fence indented under an item stays in it, so the list carries on
    /// and keeps its numbering; its lines lose the fence's indent.
    #[test]
    fn a_fence_in_an_item_stays_in_it() {
        let got = body("1. step\n\n   ```\n   make\n     all\n   ```\n2. next\n");
        assert_eq!(got.matches("<ol>").count(), 1, "{got}");
        assert!(got.contains("<li>step<pre><code>make\n  all\n</code></pre>\n</li>\n<li>next"), "{got}");
        // Under `10.` at column 4: a fence, in the item.
        let nine = (1..=9).map(|n| format!("{n}. x\n")).collect::<String>();
        let got = body(&format!("{nine}10. j\n    ```\n    1. code\n    ```\n11. k\n"));
        assert_eq!(got.matches("<ol>").count(), 1, "{got}");
        assert!(got.contains("<li>j<pre><code>1. code\n</code></pre>\n</li>"), "{got}");
        // In a nested item only, the inner list closes around it.
        let got = body("- a\n  - b\n\n  ```\n  x\n  ```\n");
        assert!(got.contains("<li>b</li>\n</ul>\n<pre><code>x\n</code></pre>\n</li>\n</ul>"), "{got}");
        // Not indented: the list ends, even right after the item's text.
        let got = body("- a\n```\nx\n```\n");
        assert!(got.contains("<li>a</li>\n</ul>\n<pre><code>x\n"), "{got}");
    }

    /// Headings and rules indented under an item stay in it too, and text
    /// after a block in an item is a paragraph of that item.
    #[test]
    fn blocks_and_text_after_them_stay_in_the_item() {
        let nine = (1..=9).map(|n| format!("{n}. x\n")).collect::<String>();
        let got = body(&format!("{nine}10. j\n    # head\n    ---\n11. k\n"));
        assert_eq!(got.matches("<ol>").count(), 1, "{got}");
        assert!(got.contains("<li>j<h1 id=\"head\">head</h1>\n<hr>\n</li>\n<li>k"), "{got}");
        let got = body("1. step\n\n   ```\n   make\n   ```\n\n   then run it\n2. next\n");
        assert_eq!(got.matches("<ol>").count(), 1, "{got}");
        assert!(got.contains("</code></pre>\n<p>then run it</p>\n</li>\n<li>next"), "{got}");
        // A loose item's second paragraph is a paragraph, not glued on.
        let got = body("- a\n\n  more\n- b\n");
        assert!(got.contains("<li>a<p>more</p>\n</li>\n<li>b"), "{got}");
        // After a nested list closes, the parent's text goes on as a paragraph.
        let got = body("- a\n  - b\n\n  more\n");
        assert!(got.contains("</ul>\n<p>more</p>\n</li>"), "{got}");
        // Unindented after a blank line, text still ends the list.
        let got = body("- a\n\n  ```\n  x\n  ```\n\nafter\n");
        assert!(got.contains("</ul>\n<p>after</p>"), "{got}");
    }

    /// The page title is the first heading outside any list item.
    #[test]
    fn a_heading_in_an_item_is_not_the_title() {
        let md = MarkdownConfig::default();
        assert_eq!(first_heading("- a\n    # Notes\n# Real\n", &md).as_deref(), Some("Real"));
        assert_eq!(first_heading("- a\n  # Notes\n", &md), None);
    }

    #[test]
    fn headings_carry_a_level_and_an_anchor() {
        let got = body("# The Title\n\n### A Deeper One\n");
        assert!(got.contains("<h1 id=\"the-title\">The Title</h1>"), "{got}");
        assert!(got.contains("<h3 id=\"a-deeper-one\">A Deeper One</h3>"), "{got}");
    }

    /// Two headings with the same text get distinct ids, GitHub's way.
    #[test]
    fn repeated_headings_get_distinct_ids() {
        let got = body("# Same\n\n## Same\n\n### Same\n\n# Same 1\n");
        assert!(got.contains("<h1 id=\"same\">"), "{got}");
        assert!(got.contains("<h2 id=\"same-1\">"), "{got}");
        assert!(got.contains("<h3 id=\"same-2\">"), "{got}");
        // `Same 1` would be `same-1`, which is taken.
        assert!(got.contains("<h1 id=\"same-1-1\">"), "{got}");
    }

    /// A bare `>` separates paragraphs in a quote; it is not one itself.
    #[test]
    fn an_empty_quote_line_is_not_a_paragraph() {
        let got = body("> one\n>\n> two\n");
        assert!(!got.contains("<p></p>"), "{got}");
        assert!(got.contains("<blockquote>\n<p>one</p>\n<p>two</p>\n</blockquote>"), "{got}");
        let got = body(">\n> one\n>\n");
        assert!(!got.contains("<p></p>"), "{got}");
        well_formed(&got).unwrap();
    }

    /// Two trailing spaces or a backslash end a line with `<br>`.
    #[test]
    fn a_hard_line_break_is_kept() {
        assert!(body("one  \ntwo\n").contains("<p>one<br>\ntwo</p>"), "{}", body("one  \ntwo\n"));
        assert!(body("one\\\ntwo\n").contains("<p>one<br>\ntwo</p>"), "{}", body("one\\\ntwo\n"));
        // An escaped backslash is a backslash, and one space is a space.
        assert!(body("one\\\\\ntwo\n").contains("<p>one\\ two</p>"), "{}", body("one\\\\\ntwo\n"));
        assert!(body("one \ntwo\n").contains("<p>one two</p>"));
        // Not at the end of a paragraph.
        assert!(body("one  \n\ntwo\n").contains("<p>one</p>"));
        assert!(body("one\\\n").contains("<p>one\\</p>"), "{}", body("one\\\n"));
        // In an item's lazy continuation too.
        assert!(body("- a  \n  b\n").contains("<li>a<br>\nb</li>"), "{}", body("- a  \n  b\n"));
    }

    /// Consecutive lines are one paragraph; a blank line starts the next.
    #[test]
    fn paragraphs_group_and_split_on_blank_lines() {
        let got = body("one line\nand its continuation\n\na second paragraph\n");
        assert!(got.contains("<p>one line and its continuation</p>"), "{got}");
        assert!(got.contains("<p>a second paragraph</p>"), "{got}");
    }

    #[test]
    fn inline_markers_become_tags() {
        let got = body("a **bold**, *italic*, `code`, ~~gone~~ and ==marked==\n");
        assert!(got.contains("<strong>bold</strong>"), "{got}");
        assert!(got.contains("<em>italic</em>"), "{got}");
        assert!(got.contains("<code>code</code>"), "{got}");
        assert!(got.contains("<del>gone</del>"), "{got}");
        assert!(got.contains("<mark>marked</mark>"), "{got}");
    }

    /// By default export is clean whatever the editor shows; with
    /// `export_follows_settings` it follows the editor.
    #[test]
    fn export_follows_the_settings_only_when_asked() {
        let mut cfg = MarkdownConfig {
            math: false,
            tables: false,
            ..MarkdownConfig::default()
        };
        assert_eq!(cfg.for_export(), MarkdownConfig::default());
        let clean = render("$$x$$\n\n| a |\n|---|\n", "t", &Page { md: cfg.for_export(), ..Page::default() });
        assert!(clean.contains("math display") && clean.contains("<table"), "{clean}");

        cfg.export_follows_settings = true;
        assert_eq!(cfg.for_export(), cfg);
        let followed = render("$$x$$\n\n| a |\n|---|\n", "t", &Page { md: cfg.for_export(), ..Page::default() });
        assert!(!followed.contains("math display") && !followed.contains("<table"), "{followed}");
    }

    /// `[markdown] math` reaches `:export` through the page.
    #[test]
    fn export_follows_the_math_setting() {
        let on = render("$$\n# Heading\n", "t", &Page::default());
        assert!(!on.contains("<h1"), "{on}");
        let md_off = MarkdownConfig { math: false, ..MarkdownConfig::default() };
        let off = render("$$\n# Heading\n", "t", &Page { md: md_off.clone(), ..Page::default() });
        assert!(off.contains("<h1 id=\"heading\">Heading</h1>"), "{off}");
        let plain_off = crate::export::plain::render("$$\n# Heading\n", 72, false, &md_off);
        assert!(plain_off.contains("Heading\n======="), "{plain_off}");
    }

    /// The rest of `[markdown]` reaches export too: what is off in the
    /// editor is plain text on the page.
    #[test]
    fn export_follows_the_markdown_settings() {
        let off = MarkdownConfig {
            tables: false,
            code_syntax: false,
            tags: false,
            ..MarkdownConfig::default()
        };
        let page = Page { md: off, ..Page::default() };
        let got = render("| a | b |\n|---|---|\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n\n#tag\n", "t", &page);
        assert!(!got.contains("<table"), "{got}");
        assert!(got.contains("<pre><code class=\"language-rust\">let x = 1;"), "unhighlighted: {got}");
        assert!(!got.contains("class=\"tag\""), "{got}");
        let on = render("| a | b |\n|---|---|\n| 1 | 2 |\n\n#tag\n", "t", &Page::default());
        assert!(on.contains("<table") && on.contains("class=\"tag\""), "{on}");
    }

    #[test]
    fn latex_is_exported_as_pandoc_math() {
        let md = MarkdownConfig { math: true, ..Default::default() };
        let b = Body::new(&md, std::path::Path::new(""));
        let got = b.run("Gradient: $\\frac{\\partial^2 f}{\\partial x^2}$\n\n$$\n\\nabla \\times \\vec{F}\n$$\n");
        assert!(got.contains("<span class=\"math inline\">\\(\\frac{\\partial^2 f}{\\partial x^2}\\)</span>"), "{got}");
        assert!(got.contains("<div class=\"math display\">\\[\\nabla \\times \\vec{F}\\]</div>"), "{got}");

        // Mid-line $$...$$ exports as display math \[...\]:
        let b_mid = Body::new(&md, std::path::Path::new(""));
        let got_mid = b_mid.run("prose $$E=mc^2$$ more prose\n");
        assert!(got_mid.contains("<span class=\"math display\">\\[E=mc^2\\]</span>"), "{got_mid}");

        // Two one-line blocks in a row do not merge:
        let b2 = Body::new(&md, std::path::Path::new(""));
        let got_two = b2.run("$$a$$\n$$b$$\n");
        assert!(got_two.contains("<div class=\"math display\">\\[a\\]</div>\n<div class=\"math display\">\\[b\\]</div>"), "{got_two}");

        // Multi-line block followed directly by single-line block:
        let b3 = Body::new(&md, std::path::Path::new(""));
        let got_multi_single = b3.run("$$\nx\n$$\n$$b$$\n");
        assert!(got_multi_single.contains("<div class=\"math display\">\\[x\\]</div>\n<div class=\"math display\">\\[b\\]</div>"), "{got_multi_single}");

        // Multi-line block followed directly by another multi-line block:
        let b4 = Body::new(&md, std::path::Path::new(""));
        let got_multi_multi = b4.run("$$\nx\n$$\n$$\ny\n$$\n");
        assert!(got_multi_multi.contains("<div class=\"math display\">\\[x\\]</div>\n<div class=\"math display\">\\[y\\]</div>"), "{got_multi_multi}");

        // When math is off, blocks and inline math render as prose, and headings stay headings:
        let md_off = MarkdownConfig { math: false, ..Default::default() };
        let b_off = Body::new(&md_off, std::path::Path::new(""));
        let got_off = b_off.run("$$\n# Heading\n");
        assert!(got_off.contains("<h1 id=\"heading\">Heading</h1>"), "{got_off}");
        assert!(got_off.contains("<p>$$</p>"), "{got_off}");
        let b_off2 = Body::new(&md_off, std::path::Path::new(""));
        let got_off2 = b_off2.run("$$\n```\na *b*\n\nc\n```\n");
        assert!(got_off2.contains("a *b*\n\nc"), "a fence under $$ stays a fence: {got_off2}");

        // A block inside a list item stays in it, so numbering carries on:
        let b6 = Body::new(&md, std::path::Path::new(""));
        let got_list = b6.run("1. step\n   $$x$$\n2. next\n");
        assert_eq!(got_list.matches("<ol>").count(), 1, "{got_list}");
        assert!(
            got_list.contains("<li>step<div class=\"math display\">\\[x\\]</div>\n</li>"),
            "{got_list}"
        );
        // Unindented after a blank line, it ends the list:
        let b7 = Body::new(&md, std::path::Path::new(""));
        let got_after = b7.run("- a\n\n$$x$$\n");
        assert!(got_after.contains("</ul>\n<div class=\"math display\">"), "{got_after}");

        // A paragraph after a block that joined an item starts afresh, as in
        // the editor's list bounds:
        let b8 = Body::new(&md, std::path::Path::new(""));
        let got_more = b8.run("- a\n$$x$$\nmore\n");
        assert!(got_more.contains("</ul>\n<p>more</p>"), "{got_more}");
        // After a blank line, a block stays only in items it is deeper than:
        let b9 = Body::new(&md, std::path::Path::new(""));
        let got_nested = b9.run("- a\n  - b\n\n  $$x$$\n");
        assert!(
            got_nested.contains("<li>b</li>\n</ul>\n<div class=\"math display\">\\[x\\]</div>\n</li>"),
            "{got_nested}"
        );

        // `$$x$$ more text` is a paragraph holding display math, as in Pandoc:
        let b5 = Body::new(&md, std::path::Path::new(""));
        let got_text = b5.run("$$E=mc^2$$ is famous\n");
        assert!(
            got_text.contains("<p><span class=\"math display\">\\[E=mc^2\\]</span> is famous</p>"),
            "{got_text}"
        );
    }

    #[test]
    fn links_images_wiki_links_and_tags_all_land() {
        let got = body("[docs](https://example.com/a) and ![alt](pic.png)\n");
        assert!(got.contains("<a href=\"https://example.com/a\">docs</a>"), "{got}");
        assert!(got.contains("<img src=\"pic.png\" alt=\"alt\">"), "{got}");

        let got = body("see [[other note]] and [[note#A Part|the part]] #tag\n");
        assert!(got.contains("href=\"other note.html\""), "{got}");
        assert!(got.contains("href=\"note.html#a-part\""), "{got}");
        assert!(got.contains(">the part</a>"), "{got}");
        assert!(got.contains("<span class=\"tag\">#tag</span>"), "{got}");
    }

    /// Markup inside bold, italic or a link's text is markup, not literal
    /// characters.
    #[test]
    fn markup_nests_inside_emphasis_and_link_text() {
        let got = body("**bold with `code` and [a link](https://x.y)**\n");
        assert!(
            got.contains("<strong>bold with <code>code</code> and <a href=\"https://x.y\">a link</a></strong>"),
            "{got}"
        );
        let got = body("[**bold link**](https://x.y)\n");
        assert!(got.contains("<a href=\"https://x.y\"><strong>bold link</strong></a>"), "{got}");
        let got = body("*it ~~gone~~ ==hi==* and ~~a *b*~~\n");
        assert!(got.contains("<em>it <del>gone</del> <mark>hi</mark></em>"), "{got}");
        assert!(got.contains("<del>a <em>b</em></del>"), "{got}");
        // Code stays code: its stars are text.
        let got = body("`**not bold**`\n");
        assert!(got.contains("<code>**not bold**</code>"), "{got}");
        // Prose inside the markup is still escaped.
        let got = body("**a < b & [c > d](https://x.y)**\n");
        assert!(got.contains("<strong>a &lt; b &amp; <a href=\"https://x.y\">c &gt; d</a></strong>"), "{got}");
        well_formed(&got).unwrap();
    }

    /// HTML forbids an anchor inside an anchor: a link in a link's text is
    /// its text.
    #[test]
    fn a_link_inside_link_text_is_text() {
        // (`inline::scan` does not read brackets inside a link's text, so an
        // autolink is the one link that can sit there today.)
        let got = body("[see <https://a.b> **x**](https://x.y)\n");
        assert_eq!(got.matches("<a ").count(), 1, "{got}");
        assert!(got.contains("<a href=\"https://x.y\">see https://a.b <strong>x</strong></a>"), "{got}");
        well_formed(&got).unwrap();
    }

    /// Titles and heading ids drop nested markup too.
    #[test]
    fn titles_and_ids_drop_nested_markup() {
        let md = MarkdownConfig::default();
        assert_eq!(plain_text("**a [b](u) `c`**", &md), "a b c");
        let got = body("# **Big [deal](u)**\n");
        assert!(got.contains("<h1 id=\"big-deal\">"), "{got}");
    }

    /// The one thing an exporter must never get wrong: prose that contains
    /// markup characters is TEXT, not tags.
    #[test]
    fn prose_is_escaped_everywhere_it_appears() {
        let got = body("a < b && c > d, and \"quoted\"\n");
        assert!(got.contains("a &lt; b &amp;&amp; c &gt; d"), "{got}");
        assert!(!got.contains("<b "), "{got}");

        let got = body("```\n<script>alert(1)</script>\n```\n");
        assert!(got.contains("&lt;script&gt;"), "{got}");
        assert!(!got.contains("<script>"), "code is text too: {got}");

        let got = body("`<em>literal</em>`\n");
        assert!(got.contains("<code>&lt;em&gt;literal&lt;/em&gt;</code>"), "{got}");
    }

    #[test]
    fn lists_nest_and_tasks_keep_their_boxes() {
        let got = body("- one\n  - nested\n- two\n\n1. first\n2. second\n");
        assert_eq!(got.matches("<ul>").count(), 2, "an outer and an inner list: {got}");
        assert_eq!(got.matches("</ul>").count(), 2, "{got}");
        assert!(got.contains("<ol>") && got.contains("</ol>"), "{got}");
        assert!(got.contains("<li>one"), "{got}");

        let got = body("- [ ] todo\n- [x] done\n");
        assert!(got.contains("<input type=\"checkbox\" disabled> todo"), "{got}");
        assert!(got.contains("<input type=\"checkbox\" checked disabled> done"), "{got}");
    }

    /// Tags nest properly — every close matches the innermost open, and
    /// nothing is left open at the end.
    ///
    /// Counting `<ul>` against `</ul>` is NOT this test: a page can balance
    /// perfectly and still close a list before the item holding it, which is
    /// exactly the bug this caught.
    fn well_formed(html: &str) -> Result<(), String> {
        const VOID: [&str; 4] = ["hr", "img", "input", "br"];
        let mut stack: Vec<&str> = Vec::new();
        let mut rest = html;
        while let Some(open) = rest.find('<') {
            rest = &rest[open + 1..];
            let Some(end) = rest.find('>') else { break };
            let tag = &rest[..end];
            rest = &rest[end + 1..];
            let (closing, name) = match tag.strip_prefix('/') {
                Some(t) => (true, t),
                None => (false, tag.split([' ', '\t']).next().unwrap_or(tag)),
            };
            if VOID.contains(&name) {
                continue;
            }
            if closing {
                match stack.pop() {
                    Some(open) if open == name => {}
                    other => return Err(format!("</{name}> closes {other:?} in {html}")),
                }
            } else {
                stack.push(name);
            }
        }
        if stack.is_empty() {
            Ok(())
        } else {
            Err(format!("left open: {stack:?} in {html}"))
        }
    }

    #[test]
    fn the_markup_nests_properly_whatever_the_document_ends_in() {
        for md in [
            "- one\n  - two\n",
            "- one\n    - two\n        - three\n- back\n",
            "- bullet\n1. ordered\n- bullet again\n",
            "- item\n  continued lazily\n",
            "- [ ] task\n- [x] done\n",
            "> quoted\n",
            "> > deep\n> back\n",
            "| a | b |\n|---|---|\n| 1 | 2 |\n",
            "para **bold**\n",
            "```rust\nlet x = 1;\n",
            "# heading\n\n- a\n\n> q\n\n| x |\n|---|\n",
        ] {
            well_formed(&body(md)).unwrap_or_else(|e| panic!("{md:?}: {e}"));
        }
    }

    /// A nested list belongs INSIDE the item it hangs off, and that item stays
    /// open around it.
    #[test]
    fn a_nested_list_sits_inside_its_parent_item() {
        let got = body("- outer\n  - inner\n- sibling\n");
        assert!(got.contains("<li>outer<ul>"), "the child opens inside the parent: {got}");
        assert!(got.contains("</ul>\n</li>"), "and the parent closes after it: {got}");
        well_formed(&got).unwrap();
    }

    #[test]
    fn a_table_gets_a_header_and_its_alignments() {
        let got = body("| Name | Qty |\n|:-----|----:|\n| tea  | 2  |\n");
        assert!(got.contains("<thead>"), "{got}");
        assert!(got.contains("<th style=\"text-align:left\">Name</th>"), "{got}");
        assert!(got.contains("<th style=\"text-align:right\">Qty</th>"), "{got}");
        assert!(got.contains("<td style=\"text-align:right\">2</td>"), "{got}");
        assert!(!got.contains("---"), "the delimiter row is structure: {got}");
    }

    #[test]
    fn a_fence_is_highlighted_in_its_language() {
        let got = body("```rust\nlet x = 1; // hi\n```\n");
        assert!(got.contains("<pre><code class=\"language-rust\">"), "{got}");
        assert!(got.contains("<span class=\"k\">let</span>"), "{got}");
        assert!(got.contains("<span class=\"c\">// hi</span>"), "{got}");

        // An unknown language is plain text in a plain block, not an error.
        let got = body("```brainfuck\n+[->+]\n```\n");
        assert!(got.contains("<pre><code class=\"language-brainfuck\">+[-&gt;+]"), "{got}");
    }

    /// A block comment that opens on one line and closes on another is one
    /// comment, because the walk carries the lexer state the way the editor does.
    #[test]
    fn fence_highlighting_carries_across_lines() {
        let got = body("```c\n/* one\ntwo */ int x;\n```\n");
        assert_eq!(got.matches("class=\"c\"").count(), 2, "one comment span per line: {got}");
        assert!(got.contains("<span class=\"t\">int</span>"), "and code after it: {got}");
    }

    #[test]
    fn front_matter_is_metadata_and_goes() {
        let got = body("---\ntitle: x\n---\n\nbody\n");
        assert!(got.contains("<p>body</p>"), "{got}");
        assert!(!got.contains("title: x"), "{got}");
    }

    #[test]
    fn the_page_is_one_self_contained_file() {
        let page = render("# Note\n\ntext\n", "fallback", &Page::default());
        assert!(page.starts_with("<!doctype html>"), "{page}");
        assert!(page.contains("<title>Note</title>"), "the first heading names it");
        assert!(page.contains("<style>"), "the CSS is inlined");
        assert!(page.trim_end().ends_with("</html>"));
        // Nothing to fetch: no script, no external stylesheet, no web font.
        assert!(!page.contains("<script"), "no script");
        assert!(!page.contains("<link"), "no external stylesheet");
        assert!(!page.contains("http://") && !page.contains("https://"), "nothing off-machine");
    }

    /// The page is drawn from the live theme, not from a copy of the default
    /// that would drift away from it.
    #[test]
    fn the_page_wears_the_configured_theme() {
        let mut page = Page { measure: 100, ..Page::default() };
        page.theme.headings[0] = Color::Rgb(0x11, 0x22, 0x33);
        let html = render("# H\n", "x", &page);
        assert!(html.contains("--h1: #112233"), "the theme reaches the CSS");
        assert!(html.contains("max-width:100ch"), "and so does the measure");
    }

    /// `page.justify` is what `--justify`/`:export … justify` sets: it turns
    /// into a class on `<main>`, which the stylesheet justifies prose under
    /// and keeps a fenced block or table left regardless. The browser's own
    /// `text-align:justify` already leaves the last line of a paragraph
    /// ragged, so there is no stretch math to carry over from `export::plain`.
    #[test]
    fn justify_adds_the_class_and_css_but_leaves_code_and_tables_left() {
        let page = Page::default();
        let html = render("text\n", "x", &page);
        assert!(html.contains("<main>\n"), "no class without the flag");

        let page = Page { justify: true, ..Page::default() };
        let html = render("text\n", "x", &page);
        assert!(html.contains("<main class=\"justify\">\n"));
        assert!(html.contains("main.justify p,main.justify li,main.justify blockquote{text-align:justify}"));
        assert!(html.contains("main.justify pre,main.justify table{text-align:left}"));
    }

    /// An indexed color is what a theme downgraded for a 256-color terminal is
    /// made of; a browser has no palette to look it up in.
    #[test]
    fn indexed_colors_are_resolved_rather_than_emitted_as_slots() {
        let mut page = Page::default();
        page.theme.text = Color::Indexed(196);
        let html = render("body\n", "x", &page);
        assert!(html.contains("--fg: #ff0000"), "{}", &html[..600.min(html.len())]);
    }

    /// "Self-contained" has to mean it: a local picture travels IN the page,
    /// not as a path back into someone's home directory.
    #[test]
    fn a_local_image_is_inlined_as_data() {
        let png: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89,
        ];
        let dir = std::env::temp_dir().join(format!(
            "shoin-html-img-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("p.png");
        std::fs::write(&file, png).unwrap();

        let got = inline_src(file.to_str().unwrap(), std::path::Path::new(""));
        assert!(got.starts_with("data:image/png;base64,iVBORw0KGgo"), "got {got:.60}");

        // A URL is a reference the author meant; it stays one.
        assert_eq!(
            inline_src("https://example.com/a.png", &dir),
            "https://example.com/a.png"
        );

        // A RELATIVE src resolves against the document, not the process — this
        // is the case that was silently left as a broken link.
        assert!(
            inline_src("p.png", &dir).starts_with("data:image/png;base64,"),
            "relative to the document's own directory"
        );
        // …and one relative to the WORKING directory still resolves, which is
        // the spelling a compiled `![[…]]` embed arrives in.
        let from_cwd = file.strip_prefix(std::env::current_dir().unwrap()).ok();
        if let Some(rel) = from_cwd {
            assert!(
                inline_src(rel.to_str().unwrap(), std::path::Path::new("/nowhere"))
                    .starts_with("data:image/png;base64,")
            );
        }
        // So does a path that is not there to read.
        assert_eq!(inline_src("no/such/file.png", &dir), "no/such/file.png");
        // And a non-image src is not this function's business.
        assert_eq!(inline_src("diagram.svg", &dir), "diagram.svg");
        std::fs::remove_dir_all(&dir).ok();
    }


    /// The page promises no script. A link is the one place a scheme can smuggle
    /// one past the escaper, so the scheme is checked and not just quoted.
    #[test]
    fn an_executable_href_is_refused_and_the_text_kept() {
        let html = render(
            "[run](javascript:alert(1)) and [ok](https://example.com/a?b=1#c)\n\n\
             [rel](notes/other.md) and [anchor](#heading) and [data](data:text/html,<script>)\n",
            "t",
            &Page::default(),
        );
        assert!(!html.contains("javascript:"), "no executable scheme survives: {html}");
        assert!(!html.contains("data:text/html"), "nor a data document: {html}");
        assert!(html.contains(">run</a>"), "the link text is kept, only the target goes");
        assert!(
            html.contains("href=\"https://example.com/a?b=1#c\""),
            "an ordinary URL is untouched: {html}"
        );
        assert!(html.contains("href=\"notes/other.md\""), "relative paths have no scheme");
        assert!(html.contains("href=\"#heading\""), "nor do anchors");
    }

    /// Case and padding are not a way around the check.
    #[test]
    fn scheme_matching_is_not_fooled_by_spelling() {
        for url in ["JavaScript:alert(1)", "  javascript:alert(1)", "vbscript:x"] {
            let html = render(&format!("[x]({url})\n"), "t", &Page::default());
            assert!(
                html.contains("href=\"#\""),
                "{url:?} should have been refused: {html}"
            );
        }
    }

    #[test]
    fn a_tab_after_the_marker_keeps_the_first_word() {
        let got = body("-\tfoo bar\n\n1.\tone two\n\n- [x]\tdone\n");
        assert!(got.contains("<li>foo bar</li>"), "{got}");
        assert!(got.contains("<li>one two</li>"), "{got}");
        assert!(got.contains("checked disabled> done</li>"), "{got}");
    }

    /// A wiki link names a note, never an address: a target with a scheme is
    /// not a link at all, and its label stays as text.
    #[test]
    fn a_wiki_link_cannot_carry_a_scheme() {
        let evil = [
            "[[javascript:alert(1)//|click me]]",
            "[[JavaScript:alert(1)|click me]]",
            "[[ javascript:alert(1)|click me]]",
            "[[java\tscript:alert(1)|click me]]",
            "[[vbscript:msgbox(1)|click me]]",
            "[[data:text/html,<script>alert(1)</script>|click me]]",
            "[[javascript:alert(1)#part|click me]]",
            "[[https://example.com|click me]]",
        ];
        for src in evil {
            let html = render(&format!("{src}\n"), "t", &Page::default());
            let low = html.to_ascii_lowercase();
            for bad in ["javascript:", "vbscript:", "data:text", "java\tscript:", "https://example.com"] {
                assert!(!low.contains(bad), "{src:?} leaked {bad:?}: {html}");
            }
            assert!(!html.contains("class=\"wiki\""), "{src:?} is no link: {html}");
            assert!(html.contains("click me"), "{src:?} keeps its label: {html}");
        }
    }

    #[test]
    fn ordinary_wiki_links_are_unchanged() {
        let got = body("[[note]] [[note#Head]] [[folder/note|Label]] [[#Local]] [[a/b:c]]\n");
        assert!(got.contains("<a class=\"wiki\" href=\"note.html\">note</a>"), "{got}");
        assert!(got.contains("<a class=\"wiki\" href=\"note.html#head\">note#Head</a>"), "{got}");
        assert!(got.contains("<a class=\"wiki\" href=\"folder/note.html\">Label</a>"), "{got}");
        assert!(got.contains("<a class=\"wiki\" href=\"#local\">#Local</a>"), "{got}");
        // A `:` after a `/` is part of a path, not a scheme.
        assert!(got.contains("<a class=\"wiki\" href=\"a/b:c.html\">a/b:c</a>"), "{got}");
    }

    /// A title is a title, not part of the address; `<…>` is unwrapped.
    #[test]
    fn link_and_image_titles_become_attributes() {
        let got = body("![alt](pic.png \"the \\\"title\\\"\") [x](<a b.md> \"it's\") [y](u (paren))\n");
        assert!(got.contains("<img src=\"pic.png\" alt=\"alt\" title=\"the &quot;title&quot;\">"), "{got}");
        assert!(got.contains("<a href=\"a b.md\" title=\"it&#39;s\">x</a>"), "{got}");
        assert!(got.contains("<a href=\"u\" title=\"paren\">y</a>"), "{got}");
        let got = body("[z](https://x.y/a_(b))\n");
        assert!(got.contains("<a href=\"https://x.y/a_(b)\">z</a>"), "{got}");
        // No title, no attribute.
        let got = body("[w](https://x.y)\n");
        assert!(got.contains("<a href=\"https://x.y\">w</a>"), "{got}");
    }

    /// `//host/x` is an address on another host, not a note.
    #[test]
    fn a_wiki_link_cannot_be_protocol_relative() {
        for src in ["[[//evil.example/x|w]]", "[[\\\\evil.example/x|w]]", "[[/\\evil.example|w]]", "[[\u{1}//evil.example|w]]"] {
            let got = body(&format!("{src}\n"));
            assert!(!got.contains("evil"), "{src:?}: {got}");
            assert!(!got.contains("class=\"wiki\""), "{src:?}: {got}");
            assert!(got.contains(">w<") || got.contains("<p>w</p>"), "{src:?} keeps its label: {got}");
        }
        // A rooted path is still a note.
        let got = body("[[/notes/a]]\n");
        assert!(got.contains("href=\"/notes/a.html\""), "{got}");
    }

    /// Every `href` and `src` value in `html`.
    fn addresses(html: &str) -> Vec<String> {
        let mut out = Vec::new();
        for attr in ["href=\"", "src=\""] {
            let mut rest = html;
            while let Some(i) = rest.find(attr) {
                rest = &rest[i + attr.len()..];
                let end = rest.find('"').unwrap_or(rest.len());
                out.push(rest[..end].to_string());
                rest = &rest[end..];
            }
        }
        out
    }

    /// The scheme of an address as a browser reads it, if it has one:
    /// leading controls and spaces go, and tabs and newlines inside are
    /// ignored.
    fn scheme_of(addr: &str) -> Option<String> {
        let a: String = addr
            .trim_start_matches(|c: char| c <= ' ')
            .chars()
            .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
            .collect();
        let head: String = a.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')).collect();
        (!head.is_empty() && a[head.len()..].starts_with(':')).then(|| head.to_ascii_lowercase())
    }

    /// Whatever an inline kind is fed, the page it lands in links only to
    /// schemes on the allow list.
    #[test]
    fn no_inline_kind_puts_a_foreign_scheme_in_the_page() {
        let targets = [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            " javascript:alert(1)",
            "java\tscript:alert(1)",
            "\u{1}javascript:alert(1)",
            "vbscript:msgbox(1)",
            "data:text/html,<script>alert(1)</script>",
            "data:image/svg+xml,<svg onload=alert(1)>",
            "&#106;avascript:alert(1)",
            "javascript&colon;alert(1)",
            "livescript:x",
            "javascript://%0aalert(1)",
            "a/b:c",
            "//evil.example/x",
        ];
        for t in targets {
            let md = format!(
                "[a]({t}) <{t}> ![i]({t}) [[{t}]] [[{t}|w]] [[{t}#h|w]] **b** _i_ ~~s~~ ==m== `c` #tag $x$ \\*\n\n\
                 [a](<{t}>) [a]({t} \"x\") ![i](<{t}> 'x') **[b]({t}) ![b]({t}) [[{t}|b]]** *[[//{t}]]*\n\n\
                 - [l]({t}) [[{t}]]\n\n> [q]({t}) ![q]({t})\n\n| [c]({t}) | [[{t}]] |\n|---|---|\n| ![x]({t}) | y |\n"
            );
            let html = render(&md, "t", &Page::default());
            for addr in addresses(&html) {
                if let Some(s) = scheme_of(&addr) {
                    let ok = matches!(s.as_str(), "http" | "https" | "mailto" | "ftp" | "file" | "tel")
                        || (s == "data" && addr.to_ascii_lowercase().starts_with("data:image/"));
                    assert!(ok, "{t:?} put {addr:?} in the page: {html}");
                }
            }
        }
    }

}
