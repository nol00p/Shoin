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
use crate::render::markdown::block::{classify, has_cell_separator, BlockKind, Carry, Marker};
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
}

impl Default for Page {
    fn default() -> Self {
        Page { theme: Theme::default(), measure: 72, base: std::path::PathBuf::new() }
    }
}

/// Convert a Markdown document to a complete HTML page.
///
/// `fallback_title` names the page when the document has no `# Heading` of its
/// own to be named after — normally the file's stem.
pub fn render(text: &str, fallback_title: &str, page: &Page) -> String {
    let md = MarkdownConfig::default();
    let body = Body::new(&md, &page.base).run(text);
    let title = first_heading(text, &md).unwrap_or_else(|| fallback_title.to_string());

    let mut out = String::with_capacity(body.len() + 4096);
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!("<title>{}</title>\n", escape(&title)));
    out.push_str("<style>\n");
    out.push_str(&stylesheet(page));
    out.push_str("</style>\n</head>\n<body>\n<main>\n");
    out.push_str(&body);
    out.push_str("</main>\n</body>\n</html>\n");
    out
}

/// The document's own title: its first heading, at whatever level.
fn first_heading(text: &str, md: &MarkdownConfig) -> Option<String> {
    let mut carry = Carry::None;
    for (i, line) in text.lines().enumerate() {
        let (kind, next) = classify(line, &carry, i == 0);
        carry = next;
        if let BlockKind::Heading(_) = kind {
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
    para: Vec<String>,
    lists: Vec<Level>,
    /// Whether an `<li>` element is open and still awaiting its `</li>`.
    item_open: bool,
    /// Whether that item is still taking continuation lines. A blank line ends
    /// the LAZY continuation without closing the item — that is what makes a
    /// loose list one list rather than two.
    lazy: bool,
    quote: u8,
    /// Raw lines of the table being accumulated.
    table: Vec<String>,
    fence: Option<(Option<Lang>, Cont)>,
}

impl<'a> Body<'a> {
    fn new(md: &'a MarkdownConfig, base: &'a std::path::Path) -> Self {
        Body {
            md,
            base,
            out: String::new(),
            para: Vec::new(),
            lists: Vec::new(),
            item_open: false,
            lazy: false,
            quote: 0,
            table: Vec::new(),
            fence: None,
        }
    }

    fn run(mut self, text: &str) -> String {
        let mut carry = Carry::None;
        for (i, line) in text.lines().enumerate() {
            let (kind, next) = classify(line, &carry, i == 0);
            carry = next;
            self.line(line, &kind);
        }
        self.close_all();
        self.out
    }

    fn line(&mut self, line: &str, kind: &BlockKind) {
        match kind {
            // Front matter is metadata for tools, not text for readers — the
            // same call plain text makes.
            BlockKind::FrontMatter => {}

            BlockKind::FenceOpen(info) => {
                self.close_all();
                let lang = Lang::from_info(info);
                // The `language-x` class is emitted for ANY info string, known
                // or not: it is what a reader's own stylesheet, or a highlighter
                // they run over the page later, looks for.
                let word = info.split_whitespace().next().unwrap_or("");
                let class = match word.is_empty() {
                    true => String::new(),
                    false => format!(" class=\"language-{}\"", escape_attr(word)),
                };
                self.out.push_str(&format!("<pre><code{class}>"));
                self.fence = Some((lang, Cont::None));
            }
            BlockKind::FenceBody { .. } => {
                // The state is taken from the fence we opened, not from the
                // kind: an exported document is walked from the top, so this
                // walk's own lexer state is the authoritative one.
                let (lang, cont) = self.fence.unwrap_or((None, Cont::None));
                let (html, next) = highlight(line, lang, cont);
                self.out.push_str(&html);
                self.out.push('\n');
                self.fence = Some((lang, next));
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
                self.close_quote();
                self.lazy = false;
            }

            BlockKind::Heading(level) => {
                self.close_all();
                let body = line.trim_start().trim_start_matches('#').trim();
                let id = slug(&plain_text(body, self.md));
                let n = (*level).clamp(1, 6);
                self.out.push_str(&format!(
                    "<h{n} id=\"{}\">{}</h{n}>\n",
                    escape_attr(&id),
                    inline_html(body, self.md, self.base)
                ));
            }

            BlockKind::Rule => {
                self.close_all();
                self.out.push_str("<hr>\n");
            }

            BlockKind::Quote(depth) => {
                self.flush_para();
                self.flush_table();
                self.close_lists();
                self.open_quote(*depth);
                let body = line.trim_start().trim_start_matches(['>', ' ']);
                self.para.push(body.to_string());
            }

            BlockKind::ListItem { depth, marker, checked } => {
                self.flush_para();
                self.flush_table();
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
                self.lazy = true;
                self.para.push(item_body(line).to_string());
            }

            BlockKind::Table => {
                // A table's HEADER row sits above the delimiter row that
                // identifies it, and `classify` only ever looks forward — so
                // the header arrived here as a paragraph line. `BlockCache`
                // promotes it in the editor by looking back one line; this walk
                // does the same, from the paragraph it is still holding.
                let header = match self.table.is_empty() {
                    true => self.para.last().filter(|l| has_cell_separator(l)).cloned(),
                    false => None,
                };
                if header.is_some() {
                    self.para.pop();
                }
                self.flush_para();
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
                if !self.lazy && self.quote == 0 {
                    self.close_lists();
                }
                self.para.push(line.trim().to_string());
            }
        }
    }

    /// Emit whatever text has accumulated, as the thing it belongs to.
    fn flush_para(&mut self) {
        if self.para.is_empty() {
            return;
        }
        let joined = self.para.join(" ");
        self.para.clear();
        let html = inline_html(joined.trim(), self.md, self.base);
        // Inside an item the text IS the item; everywhere else — including
        // inside a blockquote — it is a paragraph of its own.
        if self.item_open {
            self.out.push_str(&html);
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
        self.lazy = self.item_open;
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

    fn close_all(&mut self) {
        self.flush_para();
        self.flush_table();
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

/// The text of a list item, past its marker and any checkbox.
fn item_body(line: &str) -> &str {
    let t = line.trim_start();
    let past = match t.find(' ') {
        Some(i) => &t[i + 1..],
        None => "",
    };
    let t = past.trim_start();
    if t.len() >= 3 && (t.starts_with("[ ]") || t.starts_with("[x]") || t.starts_with("[X]")) {
        t[3..].trim_start()
    } else {
        t
    }
}

// ---------------------------------------------------------------------------
// Inline
// ---------------------------------------------------------------------------

/// One line's inline markup as HTML. Spans never overlap (`inline::scan`
/// guarantees it), so this is a single pass with no nesting to track.
fn inline_html(line: &str, md: &MarkdownConfig, base: &std::path::Path) -> String {
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
        let body = escape(&inner);
        match sp.kind {
            Inline::Bold => out.push_str(&format!("<strong>{body}</strong>")),
            Inline::Italic => out.push_str(&format!("<em>{body}</em>")),
            Inline::BoldItalic => out.push_str(&format!("<strong><em>{body}</em></strong>")),
            Inline::Strikethrough => out.push_str(&format!("<del>{body}</del>")),
            Inline::Highlight => out.push_str(&format!("<mark>{body}</mark>")),
            Inline::Code => out.push_str(&format!("<code>{body}</code>")),
            Inline::Link => {
                let url = url_of(&whole).unwrap_or(&inner);
                out.push_str(&format!("<a href=\"{}\">{body}</a>", safe_href(url)));
            }
            Inline::Autolink => {
                out.push_str(&format!("<a href=\"{}\">{body}</a>", safe_href(&inner)));
            }
            Inline::Image => {
                let url = url_of(&whole).unwrap_or("");
                out.push_str(&format!(
                    "<img src=\"{}\" alt=\"{body}\">",
                    escape_attr(&inline_src(url, base))
                ));
            }
            Inline::WikiLink => out.push_str(&wiki_link(&inner)),
            Inline::Tag => out.push_str(&format!("<span class=\"tag\">{}</span>", escape(&whole))),
            // The backslash was markup; what it protected is literal text.
            Inline::Escape => out.push_str(&body),
        }
        i = sp.outer.end;
    }
    out.push_str(&escape_chars(&chars[i.min(chars.len())..]));
    out
}

/// A `[[wiki link]]` as an anchor.
///
/// It points at `target.html`, on the assumption that a set of notes exported
/// to HTML lands beside each other — the only guess available, since a wiki
/// link names a NOTE and HTML has only addresses. `|` renames it and `#` is a
/// heading anchor, exactly as in the editor.
fn wiki_link(inner: &str) -> String {
    let (target, label) = match inner.split_once('|') {
        Some((t, l)) => (t.trim(), l.trim()),
        None => (inner.trim(), inner.trim()),
    };
    let (file, section) = match target.split_once('#') {
        Some((f, s)) => (f.trim(), Some(s.trim())),
        None => (target, None),
    };
    let mut href = String::new();
    if !file.is_empty() {
        href.push_str(&format!("{file}.html"));
    }
    if let Some(s) = section {
        href.push('#');
        href.push_str(&slug(s));
    }
    format!("<a class=\"wiki\" href=\"{}\">{}</a>", escape_attr(&href), escape(label))
}

/// Inline markup removed rather than translated — for a `<title>` and for the
/// heading ids, neither of which can hold tags.
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
        out.extend(&chars[sp.inner.clone()]);
        i = sp.outer.end;
    }
    out.extend(&chars[i.min(chars.len())..]);
    out
}

/// The `url` from `[text](url)`.
fn url_of(whole: &str) -> Option<&str> {
    let open = whole.rfind("](")?;
    let close = whole.rfind(')')?;
    (close > open + 2).then(|| whole[open + 2..close].trim())
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

    #[test]
    fn headings_carry_a_level_and_an_anchor() {
        let got = body("# The Title\n\n### A Deeper One\n");
        assert!(got.contains("<h1 id=\"the-title\">The Title</h1>"), "{got}");
        assert!(got.contains("<h3 id=\"a-deeper-one\">A Deeper One</h3>"), "{got}");
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

}
