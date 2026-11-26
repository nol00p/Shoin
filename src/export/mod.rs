//! `:export` — write the finished document out.
//!
//! "Finished" means every `![[…]]` resolved and flattened, which is the same
//! text `:embed full` shows on screen. The four formats are the four things
//! people do with a finished piece: keep it as Markdown, read it as plain text,
//! put it on the web, or hand it to someone who wants a printed page.

mod destination;
pub mod html;
mod mermaid;
mod pdf_assets;
pub mod plain;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::schema::{MarkdownConfig, TranscludeConfig};
use crate::render::markdown::block::{classify, has_cell_separator, BlockKind, Carry, Open, COMMONMARK_TAB};
use crate::render::markdown::inline::Inline;
use destination::Destination;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Text,
    Html,
    Pdf,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "md" | "markdown" => Format::Markdown,
            "txt" | "text" => Format::Text,
            "html" | "htm" | "web" => Format::Html,
            "pdf" => Format::Pdf,
            _ => return None,
        })
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Markdown => "md",
            Format::Text => "txt",
            Format::Html => "html",
            Format::Pdf => "pdf",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Markdown => "markdown",
            Format::Text => "plain text",
            Format::Html => "HTML",
            Format::Pdf => "PDF",
        }
    }
}

/// Where an export defaults to: beside the source, same stem, new extension;
/// `<stem>-export.<ext>` when that would be the source itself (Markdown from
/// `notes.md` is `notes-export.md`).
///
/// Never the source itself — that is checked at the call site as well, because
/// a suggestion the reader can edit is only a suggestion.
pub fn default_path(source: &Path, format: Format) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".into());
    let plain = source.with_file_name(format!("{stem}.{}", format.extension()));
    if plain == source || crate::transclude::same_file(&plain, source) {
        return source.with_file_name(format!("{stem}-export.{}", format.extension()));
    }
    plain
}

/// Flatten `source` and write it to `dest` in `format`.
///
/// `page` supplies rendering settings. HTML also uses its theme.
///
/// A returned note describes content the export kept as source.
pub fn write(
    source: &Path,
    dest: &Path,
    format: Format,
    cfg: &TranscludeConfig,
    page: &html::Page,
) -> Result<Option<String>> {
    let result = Job::new(source, dest.to_path_buf(), format, cfg, page)
        .and_then(|job| job.render(page, Default::default()))
        .and_then(Completed::commit)
        .map(|(_, note)| note);
    #[cfg(unix)]
    if destination::cancelled() {
        // The signal thread exits once it has drained the workers and scratch files.
        loop {
            std::thread::park();
        }
    }
    result
}

pub(crate) fn shutdown() {
    destination::stop();
    crate::render::mermaid::shutdown();
    destination::drain();
    pdf_assets::cleanup();
}

pub(crate) struct Job {
    source: PathBuf,
    dest: Destination,
    flat: String,
    format: Format,
}

impl Job {
    /// Compile and snapshot file identities before returning control to the editor.
    pub(crate) fn new(
        source: &Path, dest: PathBuf, format: Format, cfg: &TranscludeConfig, page: &html::Page,
    ) -> Result<Self> {
        let (flat, sources) = crate::transclude::compile::compile_with_sources(source, cfg)?;
        let pictures = match format {
            Format::Html | Format::Pdf => linked_pictures(&flat, &page.base, &page.md),
            _ => Vec::new(),
        };
        let dest = Destination::new(dest, sources, pictures)?;
        Ok(Self { source: source.to_path_buf(), dest, flat, format })
    }

    pub(crate) fn render(
        self, page: &html::Page, renderer: crate::render::mermaid::MermaidRenderer,
    ) -> Result<Completed> {
        let _worker = destination::start()?;
        let Self { source, dest, flat, format } = self;
        let (output, note) = match format {
            Format::Markdown => (Output::Markdown { flat, source, md: page.md.clone() }, None),
            Format::Text => (Output::Text(plain::render(&flat, page.measure, page.justify, &page.md)), None),
            Format::Html => {
                let title = source.file_stem().unwrap_or(std::ffi::OsStr::new("document")).to_string_lossy();
                let (html, note) = html::render_with_mermaid(&flat, &title, page, renderer);
                (Output::Text(html), note)
            }
            Format::Pdf => {
                let flat = rebase_images(&flat, &source, None, &page.md);
                let (scratch, note) = write_pdf(&flat, &source, &page.md, renderer)?;
                (Output::Pdf(scratch), note)
            }
        };
        Ok(Completed { dest, output, note })
    }
}

enum Output {
    Markdown { flat: String, source: PathBuf, md: MarkdownConfig },
    Text(String),
    Pdf(pdf_assets::Assets),
}

pub(crate) struct Completed {
    dest: Destination,
    output: Output,
    note: Option<String>,
}

impl Completed {
    /// Interactive callers commit on the editor thread, between file operations.
    pub(crate) fn commit(self) -> Result<(String, Option<String>)> {
        use std::io::Write;

        self.dest.commit(|file| match &self.output {
            Output::Markdown { flat, source, md } => {
                let flat = rebase_images(flat, source, Some(self.dest.path()), md);
                file.write_all(flat.as_bytes())
            }
            Output::Text(text) => file.write_all(text.as_bytes()),
            Output::Pdf(scratch) => {
                let mut pdf = std::fs::File::open(scratch.path().join(PDF_OUTPUT))?;
                std::io::copy(&mut pdf, file).map(|_| ())
            }
        })?;
        let path = self.dest.path();
        let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
        Ok((name, self.note))
    }
}

/// Every local picture file `markdown` links to (`![alt](img/p.png)`),
/// found from `base` the way HTML export finds them to carry them inside
/// the page (`html::local_file`). Code, fenced blocks (quoted ones too),
/// front matter and math are not read, as in `rebase_images`; a remote
/// address names no file.
fn linked_pictures(markdown: &str, base: &Path, md: &MarkdownConfig) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut carry = Carry::default();
    let mut held = Held::default();
    for (i, line) in markdown.lines().enumerate() {
        let (kind, next) = classify(line, &carry, i == 0, md.math, COMMONMARK_TAB);
        carry = next;
        held = held.next(line, &kind, md.math);
        if !held.code && !matches!(
            kind,
            BlockKind::FenceOpen(_)
                | BlockKind::FenceBody { .. }
                | BlockKind::FenceClose
                | BlockKind::FrontMatter
                | BlockKind::MathBlock
        ) {
            pictures_in(line, base, md, &mut out);
        }
    }
    out
}

/// Whether a span's body is prose, which can hold more markup: emphasis and
/// a link's text. `inline::scan` returns flat spans, so every export walker
/// scans these bodies again to reach a link in bold or a picture in a link;
/// everything else (code, math, tags, a picture's alt text) is what it says.
/// The one list the walkers share, with no catch-all: a new kind has to be
/// placed here.
fn holds_prose(kind: Inline) -> bool {
    match kind {
        Inline::Bold | Inline::Italic | Inline::BoldItalic | Inline::Strikethrough | Inline::Highlight | Inline::Link => true,
        Inline::Code
        | Inline::WikiLink
        | Inline::Image
        | Inline::Tag
        | Inline::Autolink
        | Inline::Escape
        | Inline::Latex => false,
    }
}

/// One line of `linked_pictures`, reaching into emphasis and link text.
fn pictures_in(line: &str, base: &Path, md: &MarkdownConfig, out: &mut Vec<PathBuf>) {
    use crate::render::markdown::inline;
    let chars: Vec<char> = line.chars().collect();
    for sp in inline::scan(line, md) {
        let inner: String = chars[sp.inner.clone()].iter().collect();
        match sp.kind {
            Inline::Image => {
                if let Some(file) = inline::destination(&sp, line).and_then(|(url, _)| html::local_file(&url, base)) {
                    out.push(file);
                }
            }
            k if holds_prose(k) => pictures_in(&inner, base, md, out),
            _ => {}
        }
    }
}

/// What a paragraph left open by the lines so far is: whether it sits in a
/// quote and how deep, whether that quote's line was paragraph text, and
/// the quote's own block state (a quoted fence is code, not text).
///
/// One rule for what carries a paragraph on, used by HTML to group lines
/// into `<p>` and by plain text to decide whether a hard break's marker is
/// a break: the two exports must not read a document differently.
#[derive(Clone, Debug, Default)]
pub(crate) struct Held {
    quote: u8,
    prose: bool,
    inner: Carry,
    /// The line read was a quoted fence's: its opening, its code or its
    /// closing. Every walker that leaves code alone skips such a line.
    pub(crate) code: bool,
}

/// A quote line as its quote reads it: the text past its markers, what that
/// text is inside the quote, and the depth it is read at.
pub(crate) struct Quoted {
    pub(crate) body: String,
    pub(crate) kind: BlockKind,
    pub(crate) depth: u8,
}

impl Held {
    /// The state after `line`, of kind `kind`.
    pub(crate) fn next(&self, line: &str, kind: &BlockKind, math: bool) -> Held {
        self.step(line, kind, math).0
    }

    /// The state after `line`, of kind `kind`, and for a quote line how its
    /// quote reads it.
    pub(crate) fn step(&self, line: &str, kind: &BlockKind, math: bool) -> (Held, Option<Quoted>) {
        match kind {
            BlockKind::Quote(depth) => {
                let depth = (*depth).max(1);
                // Inside a quoted fence a line that keeps the fence's markers
                // is the fence's, whatever follows them: under `> ```` the
                // line `> > x` is the code `> x`, as in CommonMark.
                let fenced = self.quote > 0 && matches!(self.inner.open, Open::InFence { .. });
                let depth = match fenced && depth > self.quote {
                    true => self.quote,
                    false => depth,
                };
                let inner = match depth == self.quote {
                    true => self.inner.clone(),
                    false => Carry::default(),
                };
                let body = quote_strip(line, depth);
                let (k, inner) = classify(&body, &inner, false, math, COMMONMARK_TAB);
                // A quoted list item, heading, fence, rule or table row keeps
                // its own line; only quoted text runs on into the next.
                let prose = k == BlockKind::Paragraph && !has_cell_separator(&body);
                let code = matches!(k, BlockKind::FenceOpen(_) | BlockKind::FenceBody { .. } | BlockKind::FenceClose);
                // Text at a shallower depth than quoted text goes on with
                // it, and the deeper quote stays open: CommonMark's lazy
                // continuation, `> > a` then `> b` being one paragraph.
                if prose && self.prose && depth < self.quote {
                    return (self.clone(), Some(Quoted { body, kind: k, depth: self.quote }));
                }
                (Held { quote: depth, prose, inner, code }, Some(Quoted { body, kind: k, depth }))
            }
            // An unmarked line goes on with whatever it continues, a quote
            // included: CommonMark's lazy continuation. A quoted fence is
            // not text, so nothing continues it: the line ends the quote.
            BlockKind::Paragraph if !self.code => (self.clone(), None),
            _ => (Held::default(), None),
        }
    }

    /// Whether `next` (of kind `next_kind`, followed by a line of kind
    /// `then`) carries on the paragraph held. A paragraph line does, unless
    /// it is a table's header row, which the delimiter row after it takes;
    /// a quote line does when it is text at the same depth as quoted text.
    pub(crate) fn joins(
        &self,
        next: &str,
        next_kind: &BlockKind,
        then: Option<&BlockKind>,
        md: &MarkdownConfig,
    ) -> bool {
        let table = |k: Option<&BlockKind>| md.tables && k == Some(&BlockKind::Table);
        match next_kind {
            BlockKind::Paragraph => !(has_cell_separator(next) && table(then)),
            BlockKind::Quote(_) => {
                let after = self.next(next, next_kind, md.math);
                self.prose && after.prose && after.quote == self.quote
            }
            _ => false,
        }
    }
}

/// A quote line's text past its first `depth` markers, as CommonMark reads
/// it: each `>` may be followed by one space or tab, and a tab counts as the
/// columns to the next tab stop, so `>` and a tab leave two columns of
/// indent. The indent left is written as spaces, since its columns no longer
/// start where the line did. Spaces between markers are skipped the way
/// `block::classify` counts the depth.
pub(crate) fn quote_strip(line: &str, depth: u8) -> String {
    let width = |c: char, col: usize| match c {
        '\t' => 4 - col % 4,
        _ => 1,
    };
    let mut col = 0;
    let mut rest = line;
    // The column the text starts at, past the last marker and its space.
    let mut start = 0;
    for _ in 0..depth {
        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let Some(after) = rest[ws..].strip_prefix('>') else {
            break;
        };
        col = rest[..ws].chars().fold(col, |c, ch| c + width(ch, c)) + 1;
        rest = after;
        start = col;
        match rest.chars().next() {
            Some(' ') => {
                rest = &rest[1..];
                col += 1;
                start = col;
            }
            // One column of the tab is the marker's space; the rest is indent.
            Some('\t') => start = col + 1,
            _ => {}
        }
    }
    let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let end = rest[..ws].chars().fold(col, |c, ch| c + width(ch, c));
    format!("{}{}", " ".repeat(end.saturating_sub(start)), &rest[ws..])
}

/// `markdown` with every relative image path, written against `source`'s
/// folder, rewritten for a reader in another place: relative to the folder
/// `to` names, or absolute when `to` is `None` (what pandoc is given, since
/// it runs in the working directory). Unchanged when `to` is the source's
/// own folder.
///
/// A path that is not there beside the source but is from the working
/// directory is taken from there, as HTML export does: that is the spelling
/// an embed compiled before 0.2.4 arrives in. Addresses, absolute paths,
/// code and fenced blocks are left alone. A path with a space or a
/// parenthesis is written `<…>`; one that cannot be written that way (`<`,
/// `>`, `\`) is left as it was.
fn rebase_images(markdown: &str, source: &Path, to: Option<&Path>, md: &MarkdownConfig) -> String {
    let Some(from) = folder(source) else {
        return markdown.to_string();
    };
    let to = match to.map(folder) {
        None => None,
        Some(Some(to)) if to == from => return markdown.to_string(),
        Some(Some(to)) => Some(to),
        Some(None) => return markdown.to_string(),
    };
    let mut out = String::with_capacity(markdown.len());
    let mut carry = Carry::default();
    let mut held = Held::default();
    for (i, raw) in markdown.split_inclusive('\n').enumerate() {
        let line = raw.trim_end_matches(['\n', '\r']);
        let (kind, next) = classify(line, &carry, i == 0, md.math, COMMONMARK_TAB);
        carry = next;
        held = held.next(line, &kind, md.math);
        match kind {
            // A fence inside a quote is code too.
            _ if held.code => out.push_str(raw),
            BlockKind::FenceOpen(_)
            | BlockKind::FenceBody { .. }
            | BlockKind::FenceClose
            | BlockKind::FrontMatter
            | BlockKind::MathBlock => out.push_str(raw),
            _ => {
                out.push_str(&rebase_line(line, &from, to.as_deref(), md));
                out.push_str(&raw[line.len()..]);
            }
        }
    }
    out
}

/// The folder `path` is in, resolved on disk.
fn folder(path: &Path) -> Option<PathBuf> {
    let parent = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::canonicalize(parent).ok()
}

/// One line of `rebase_images`, reaching into emphasis the way export does.
fn rebase_line(line: &str, from: &Path, to: Option<&Path>, md: &MarkdownConfig) -> String {
    use crate::render::markdown::inline;
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    for sp in inline::scan(line, md) {
        if sp.outer.start < i {
            continue;
        }
        out.extend(&chars[i..sp.outer.start]);
        let whole: String = chars[sp.outer.clone()].iter().collect();
        match sp.kind {
            // A link's text may hold a picture: the README badge
            // `[![alt](img)](url)`.
            k if holds_prose(k) => {
                let inner: String = chars[sp.inner.clone()].iter().collect();
                out.extend(&chars[sp.outer.start..sp.inner.start]);
                out.push_str(&rebase_line(&inner, from, to, md));
                out.extend(&chars[sp.inner.end..sp.outer.end]);
            }
            Inline::Image => {
                let alt: String = chars[sp.inner.clone()].iter().collect();
                match inline::destination(&sp, line).and_then(|(url, title)| {
                    let url = rebased(&url, from, to)?;
                    Some((url, title))
                }) {
                    Some((url, title)) => {
                        out.push_str(&format!("![{alt}]({url}"));
                        if let Some(t) = title {
                            out.push_str(&format!(" \"{}\"", t.replace('\\', "\\\\").replace('"', "\\\"")));
                        }
                        out.push(')');
                    }
                    None => out.push_str(&whole),
                }
            }
            _ => out.push_str(&whole),
        }
        i = sp.outer.end;
    }
    out.extend(&chars[i.min(chars.len())..]);
    out
}

/// `url`, relative to `from`, made relative to `to` (absolute without one)
/// and written as an image destination; `None` when it is not a relative
/// path or cannot be written.
fn rebased(url: &str, from: &Path, to: Option<&Path>) -> Option<String> {
    let scheme = url.split(['/', '?', '#']).next().is_some_and(|head| head.contains(':'));
    if url.is_empty() || scheme || url.starts_with(['/', '#', '~']) {
        return None;
    }
    let mut target = normal(&from.join(url));
    if !target.exists() {
        if let Some(cwd) = std::env::current_dir().ok().map(|d| normal(&d.join(url))).filter(|p| p.exists()) {
            target = cwd;
        }
    }
    let rel = match to {
        None => target,
        Some(to) => {
            let to = normal(to);
            let common = target.components().zip(to.components()).take_while(|(a, b)| a == b).count();
            let mut rel = PathBuf::new();
            for _ in to.components().skip(common) {
                rel.push("..");
            }
            rel.extend(target.components().skip(common));
            rel
        }
    };
    let rel = rel.to_string_lossy().into_owned();
    if rel.is_empty() || rel.contains(['<', '>', '\\', '\n', '\r']) {
        return None;
    }
    Some(match rel.contains(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        true => format!("<{rel}>"),
        false => rel,
    })
}

/// `p` with `.` and `..` folded away, without touching the disk.
fn normal(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// A path as an error message should name it. The status line is one row and
/// right-aligned, so an absolute path spends the whole of it on directories the
/// reader already knows they are in.
fn short(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// PDF is produced by handing the flattened Markdown to an external converter.
///
const PDF_OUTPUT: &str = "document.pdf";

/// DELIBERATELY not built in. A PDF writer means a layout engine and embedded
/// fonts inside a terminal text editor, for output worse than what the tools
/// people already have produce. Shoin passes the Markdown to `pandoc`.
fn write_pdf(
    markdown: &str, source: &Path, md: &MarkdownConfig, renderer: crate::render::mermaid::MermaidRenderer,
) -> Result<(pdf_assets::Assets, Option<String>)> {
    let Some(tool) = which("pandoc") else {
        // Short on purpose: this is read on a one-row status line, and a
        // message that does not fit loses its ending — which is the half that
        // says what to do.
        bail!("needs pandoc on PATH — or export md instead");
    };
    // pandoc converts, but something else has to LAY OUT the page, and pandoc's
    // default is pdflatex — which most machines do not have. Picking an engine
    // that is actually installed turns "'pdflatex' not found" into a working
    // export on any machine with one of these.
    let Some(engine) = PDF_ENGINES.iter().find(|e| which(e).is_some()) else {
        bail!("{NO_ENGINE}");
    };

    let scratch = pdf_assets::Assets::create().context("reserving a PDF work directory")?;
    let note = write_pdf_with(markdown, source, &scratch.path().join(PDF_OUTPUT), md, &tool, engine, renderer)?;
    Ok((scratch, note))
}

fn write_pdf_with(
    markdown: &str,
    source: &Path,
    dest: &Path,
    md: &MarkdownConfig,
    tool: &Path,
    engine: &str,
    renderer: crate::render::mermaid::MermaidRenderer,
) -> Result<Option<String>> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    // Hold the generated PNGs through the converter and its page engine.
    let prepared = pdf_assets::prepare(markdown, md, renderer);
    if destination::cancelled() {
        bail!("export cancelled");
    }

    // Files let us poll and reap the child independently of how the layout
    // engine uses stdin and stderr.
    let scratch = pdf_assets::Assets::create().context("reserving Pandoc input and diagnostics")?;
    let input = scratch.path().join("stdin");
    std::fs::File::create(&input)?.write_all(prepared.markdown.as_bytes())?;
    let input = std::fs::File::open(input)?;
    let errors = scratch.path().join("stderr");
    let stderr = std::fs::File::create(&errors)?;

    let mut child = pandoc_command(tool)
        // pandoc reads `$…$` as math by default; with `math = false` followed
        // into export, it is the text the editor shows.
        .arg(if md.math { "--from=markdown" } else { "--from=markdown-tex_math_dollars" })
        .arg(format!("--pdf-engine={engine}"))
        .args(engine_args(engine))
        // Pictures arrive with absolute paths. The resource path covers any
        // left as written, for the engines that read it.
        .arg(resource_path(source))
        .arg("--output")
        .arg(dest)
        .stdin(input)
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .context("running pandoc")?;

    let status = loop {
        if destination::cancelled() {
            stop_pandoc(&mut child);
            bail!("export cancelled");
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(e) => {
                stop_pandoc(&mut child);
                return Err(e).context("waiting for pandoc");
            }
        }
    };

    if !status.success() {
        // Pass through the first diagnostic, which usually names the engine.
        let why = BufReader::new(std::fs::File::open(errors)?).lines().next()
            .and_then(std::io::Result::ok).unwrap_or_else(|| "pandoc failed".into());
        bail!("pandoc: {}", why.trim());
    }
    Ok(prepared.note)
}

fn pandoc_command(tool: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(tool);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Pandoc starts the PDF layout engine in the same process group.
        command.process_group(0);
    }
    command
}

fn stop_pandoc(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // SAFETY: the child was spawned as the leader of this process group
        // and we have yet to reap it, so the ID cannot have been recycled.
        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

/// The HTML page's formulas as MathML, through `pandoc` (an
/// [`html::Convert`]).
///
/// A constant number of runs for the whole page, not one per formula: a
/// first, empty one asks pandoc which version of its document format it
/// reads ([`api_version`]); the second converts [`mathml_source`], a
/// document holding every formula, and [`mathml_split`] takes pandoc's page
/// apart again. MathML rather than a script such as MathJax: browsers draw
/// it natively, and the page keeps its promise of no script and nothing to
/// fetch.
///
/// Both runs share a deadline of [`PANDOC_LIMIT`], so a `pandoc` that hangs
/// cannot hang the export or the editor.
///
/// The `Err` notes are short for the same reason as `write_pdf`'s: they end
/// a one-row status line.
pub(crate) fn pandoc_mathml(formulas: &[html::Formula]) -> std::result::Result<Vec<Option<String>>, &'static str> {
    let Some(tool) = which("pandoc") else {
        return Err("math left as TeX: no pandoc");
    };
    mathml_with(&tool, formulas, PANDOC_LIMIT)
}

/// How long [`pandoc_mathml`] waits for `pandoc`, both runs together.
const PANDOC_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

const PANDOC_FAILED: &str = "math left as TeX: pandoc failed";
const PANDOC_TIMED_OUT: &str = "math left as TeX: pandoc timed out";

/// [`pandoc_mathml`] with the program and the time limit given.
fn mathml_with(tool: &Path, formulas: &[html::Formula], limit: std::time::Duration) -> std::result::Result<Vec<Option<String>>, &'static str> {
    let deadline = std::time::Instant::now() + limit;
    let empty = run_until(tool, &["--from=markdown", "--to=json"], String::new(), deadline)?;
    let api = api_version(&empty).ok_or(PANDOC_FAILED)?;
    // `--mathml` rather than 3.x's `--math-method=mathml`, which older
    // pandocs reject; newer ones accept it with a warning on stderr.
    let page = run_until(tool, &["--from=json", "--to=html", "--mathml"], mathml_source(formulas, api), deadline)?;
    Ok(mathml_split(&page, formulas.len()))
}

/// Run `tool` with `input` on its stdin and return its stdout, or a note
/// when it cannot be run, fails, or is not done by `deadline`; then it is
/// killed and reaped.
fn run_until(tool: &Path, args: &[&str], input: String, deadline: std::time::Instant) -> std::result::Result<String, &'static str> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::sync::mpsc;

    let mut child = pandoc_command(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // A formula pandoc cannot read is a warning there; it keeps its TeX.
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| PANDOC_FAILED)?;
    let (Some(mut stdin), Some(mut stdout)) = (child.stdin.take(), child.stdout.take()) else {
        stop_pandoc(&mut child);
        return Err(PANDOC_FAILED);
    };
    // Writing and reading on threads of their own: a long page cannot fill
    // both pipes at once, and a child that stops answering leaves them
    // blocked rather than the export. Killing the child normally ends them;
    // if something it started still holds a pipe, they are left behind, but
    // the export goes on.
    let (wrote_tx, wrote) = mpsc::channel();
    std::thread::spawn(move || wrote_tx.send(stdin.write_all(input.as_bytes()).is_ok()));
    let (read_tx, read) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let ok = stdout.read_to_string(&mut out).is_ok();
        read_tx.send(ok.then_some(out))
    });
    let left = || deadline.saturating_duration_since(std::time::Instant::now());
    let mut finish = || {
        let out = receive_pandoc(&read, left)?;
        let wrote = receive_pandoc(&wrote, left)?;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Ok((out, wrote, status)),
                Ok(None) if left().is_zero() || destination::cancelled() => return Err(PANDOC_TIMED_OUT),
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(2)),
                Err(_) => return Err(PANDOC_FAILED),
            }
        }
    };
    match finish() {
        Ok((Some(page), true, status)) if status.success() => Ok(page),
        Ok(_) => Err(PANDOC_FAILED),
        Err(note) => {
            stop_pandoc(&mut child);
            Err(note)
        }
    }
}

fn receive_pandoc<T>(
    receiver: &std::sync::mpsc::Receiver<T>, left: impl Fn() -> std::time::Duration,
) -> std::result::Result<T, &'static str> {
    use std::sync::mpsc::RecvTimeoutError;

    loop {
        if destination::cancelled() || left().is_zero() {
            return Err(PANDOC_TIMED_OUT);
        }
        match receiver.recv_timeout(left().min(std::time::Duration::from_millis(10))) {
            Ok(value) => return Ok(value),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(PANDOC_FAILED),
        }
    }
}

/// The version of pandoc's document format in `json`, pandoc's JSON
/// output for an empty document: `1,23,1,2` from
/// `{"pandoc-api-version":[1,23,1,2],…}`. pandoc reads a JSON document
/// only when its version matches its own, so [`mathml_source`] writes
/// this one.
fn api_version(json: &str) -> Option<&str> {
    const KEY: &str = "\"pandoc-api-version\":[";
    let start = json.find(KEY)? + KEY.len();
    let v = &json[start..start + json[start..].find(']')?];
    (!v.is_empty() && v.split(',').all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))).then_some(v)
}

/// The `id` of the `<div>` that holds formula `i` in [`mathml_source`]'s
/// document, as pandoc writes it into the page: `<div id="shoin-math-3">`.
const MATHML_DIV: &str = "<div id=\"shoin-math-";

/// One pandoc document (its JSON form, for pandoc-api-version `api`) that
/// holds every formula, each a math element in a `<div>` of its own.
///
/// JSON rather than Markdown: each formula is a string inside a math
/// element, so its text cannot end the element or start another, whatever
/// `$$` or HTML it contains. In pandoc's page it is MathML, or escaped
/// text when pandoc cannot read it, so it cannot forge a `<div>` either.
fn mathml_source(formulas: &[html::Formula], api: &str) -> String {
    let mut doc = format!("{{\"pandoc-api-version\":[{api}],\"meta\":{{}},\"blocks\":[");
    for (i, f) in formulas.iter().enumerate() {
        if i > 0 {
            doc.push(',');
        }
        let kind = if f.display { "DisplayMath" } else { "InlineMath" };
        doc.push_str(&format!(
            "{{\"t\":\"Div\",\"c\":[[\"shoin-math-{i}\",[],[]],[{{\"t\":\"Plain\",\"c\":[{{\"t\":\"Math\",\"c\":[{{\"t\":\"{kind}\"}},"
        ));
        json_string(&mut doc, &f.tex);
        doc.push_str("]}]}]]}");
    }
    doc.push_str("]}");
    doc
}

/// `s` as a JSON string, quotes included.
fn json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// pandoc's page cut back into formulas: what is inside each formula's
/// `<div>`, when it is one `<math>` element and nothing else; `None`
/// otherwise — a formula pandoc could not convert comes back as text, so it
/// keeps its TeX.
///
/// One pass over the page, each `<div>` checked as it comes: one missing,
/// repeated or out of order means the pieces cannot be matched to the
/// formulas, so none are converted.
fn mathml_split(page: &str, n: usize) -> Vec<Option<String>> {
    // Where each formula's `<div>` starts, and where its content does.
    let mut at: Vec<(usize, usize)> = Vec::with_capacity(n);
    let mut from = 0;
    while let Some(found) = page[from..].find(MATHML_DIV) {
        let start = from + found;
        let number = start + MATHML_DIV.len();
        let digits = page[number..].bytes().take_while(u8::is_ascii_digit).count();
        let content = number + digits + "\">".len();
        let in_step = page[number + digits..].starts_with("\">") && page[number..number + digits].parse() == Ok(at.len());
        if !in_step || at.len() == n {
            return vec![None; n];
        }
        at.push((start, content));
        from = content;
    }
    if at.len() != n {
        return vec![None; n];
    }
    (0..n)
        .map(|i| {
            let end = at.get(i + 1).map_or(page.len(), |&(start, _)| start);
            let piece = page[at[i].1..end].trim();
            let piece = piece.strip_suffix("</div>")?.trim();
            // Export puts its own wrapper around it, so one pandoc adds goes.
            let piece = ["<span class=\"math inline\">", "<span class=\"math display\">"]
                .iter()
                .find_map(|open| piece.strip_prefix(open)?.strip_suffix("</span>"))
                .unwrap_or(piece);
            let one = piece.starts_with("<math")
                && piece.ends_with("</math>")
                && piece.matches("<math").count() == 1
                && piece.matches("</math>").count() == 1;
            // In an HTML page the parser puts `<math>` in the MathML
            // namespace by itself, and the page names no address.
            one.then(|| piece.replacen(" xmlns=\"http://www.w3.org/1998/Math/MathML\"", "", 1))
        })
        .collect()
}

/// `--resource-path=<document's folder>:<working directory>`.
fn resource_path(source: &Path) -> std::ffi::OsString {
    let folder = source.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    let dirs = std::env::join_paths([folder.as_path(), Path::new(".")])
        .unwrap_or_else(|_| folder.clone().into_os_string());
    let mut arg = std::ffi::OsString::from("--resource-path=");
    arg.push(dirs);
    arg
}

/// What to say when pandoc is there but no engine is. Homebrew is the easy
/// way to get typst on macOS; on Linux, distros differ (Fedora has no typst
/// package at all), and the release binary from GitHub works everywhere.
const NO_ENGINE: &str = if cfg!(target_os = "macos") {
    "pandoc has no PDF engine — try: brew install typst"
} else {
    "no PDF engine — get typst at github.com/typst/typst"
};

/// PDF engines pandoc can drive, cheapest first.
///
/// `typst` leads because it is a single ~30MB binary with fonts built in,
/// against a TeX distribution's gigabytes; `tectonic` is the self-contained TeX
/// if someone wants LaTeX semantics. The rest are what a machine is likely to
/// already have.
const PDF_ENGINES: &[&str] = &[
    "typst",
    "tectonic",
    "xelatex",
    "lualatex",
    "pdflatex",
    "weasyprint",
    "wkhtmltopdf",
];

/// What pandoc needs to be told for `engine` to work out of the box.
///
/// pandoc's Typst template passes `font: ()` when no main font is given, and
/// Typst 0.15 rejects an empty font list, so every export failed with "font
/// fallback list must not be empty". Libertinus Serif is built into the typst
/// binary, so naming it works on any machine with typst, fonts installed or not.
///
/// typst also gets `/` as its root, so the absolute picture paths export
/// writes are found.
fn engine_args(engine: &str) -> &'static [&'static str] {
    match engine {
        // typst reads files only under its root, which pandoc leaves at its
        // temporary file's folder, and reads an absolute path from there.
        "typst" => &["--variable=mainfont:Libertinus Serif", "--pdf-engine-opt=--root=/"],
        _ => &[],
    }
}

/// The first executable named `name` on `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    which_in(name, &std::env::var_os("PATH")?)
}

/// [`which`] in the folders of `path`, a `PATH`-style list.
fn which_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|p| runnable(p))
}

/// A regular file (or a link to one) that may be run: not a folder, nor,
/// on Unix, a file without an execute bit, which `exec` would refuse.
fn runnable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    p.is_file()
}

/// The text of a list item, past its marker and any checkbox, read the way
/// `block::classify` reads them: `-`, `*`, `+` or digits with `.`/`)`, then
/// one space or tab; a `[ ]`/`[x]` box counts only when whitespace or the end
/// of the line follows it. A line that is not an item comes back trimmed.
fn item_body(line: &str) -> &str {
    let t = line.trim_start();
    let after_marker = if let Some(rest) = t.strip_prefix(['-', '*', '+']) {
        rest
    } else {
        let digits = t.bytes().take_while(u8::is_ascii_digit).count();
        match t[digits..].strip_prefix(['.', ')']) {
            Some(rest) if digits > 0 => rest,
            _ => return t,
        }
    };
    let Some(after) = after_marker.strip_prefix([' ', '\t']) else {
        return after_marker.trim_start();
    };
    for b in ["[ ]", "[x]", "[X]"] {
        if let Some(rest) = after.strip_prefix(b) {
            if rest.is_empty() || rest.starts_with([' ', '\t']) {
                return rest.trim_start();
            }
        }
    }
    after.trim_start()
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_item_body_skips_the_marker_as_the_classifier_reads_it() {
        for (line, want) in [
            ("- foo bar", "foo bar"),
            ("-\tfoo bar", "foo bar"),
            ("  * a", "a"),
            ("+   spaced", "spaced"),
            ("1.\tone two", "one two"),
            ("12) twelve", "twelve"),
            ("- [x]\tdone", "done"),
            ("- [ ] open", "open"),
            ("- [X]", ""),
            ("- [x]y", "[x]y"),
            ("-  [x] two spaces", "[x] two spaces"),
            ("-", ""),
            ("1.", ""),
        ] {
            assert_eq!(item_body(line), want, "{line:?}");
        }
    }

    use super::*;

    /// A picture inside a link's text — the README badge — is rebased like
    /// any other; the link's own address is left as written.
    /// A quote's markers go as CommonMark reads them: one space or tab after
    /// each `>` is the marker's, and a tab counts to the next tab stop.
    #[test]
    fn a_quote_line_loses_its_markers_and_one_space_each() {
        for (line, depth, want) in [
            ("> a", 1, "a"),
            (">a", 1, "a"),
            (">  a", 1, " a"),
            (">\ta", 1, "  a"),
            (">\t\ta", 1, "      a"),
            ("> >\ta", 2, "a"),
            ("> > >   a", 3, "  a"),
            ("  > a", 1, "a"),
            ("\t> \ta", 1, "  a"),
            ("> > a", 1, "> a"),
            (">", 1, ""),
            (">\t", 1, "  "),
        ] {
            assert_eq!(quote_strip(line, depth), want, "{line:?} at {depth}");
        }
    }

    /// A quoted fence's lines are code to every walker that skips code; the
    /// quote ends it, and text after it is not code.
    #[test]
    fn a_fence_in_a_quote_is_code_until_the_quote_ends() {
        let code = |doc: &str| -> Vec<bool> {
            let md = MarkdownConfig::default();
            let (mut carry, mut held) = (Carry::default(), Held::default());
            doc.lines()
                .enumerate()
                .map(|(i, line)| {
                    let (kind, next) = classify(line, &carry, i == 0, md.math, COMMONMARK_TAB);
                    carry = next;
                    held = held.next(line, &kind, md.math);
                    held.code
                })
                .collect()
        };
        assert_eq!(code("> ```\n> a\n>\n> ```\n> b\n"), [true, true, true, true, false]);
        assert_eq!(code("> ```\n> a\nb\n> c\n"), [true, true, false, false]);
        assert_eq!(code("> ```\n> a\n\n> b\n"), [true, true, false, false]);
        // Deeper markers inside the fence are its code; fewer end it.
        assert_eq!(code("> ~~~\n> > a\n> ~~~\n"), [true, true, true]);
        assert_eq!(code("> > ```\n> a\n"), [true, false]);
        assert_eq!(code("```\n> a\n```\n> b\n"), [false, false, false, false]);
    }

    #[test]
    fn a_badge_in_a_link_is_rebased() {
        let md = MarkdownConfig::default();
        let from = Path::new("/notes");
        let got = rebase_line("[![ci](img/b.svg)](docs/x.md) and **![p](q.png)**", from, None, &md);
        assert_eq!(got, "[![ci](/notes/img/b.svg)](docs/x.md) and **![p](/notes/q.png)**");
    }

    /// The five export walkers reach into a span's body exactly when
    /// `holds_prose` says it is prose: a picture inside emphasis or a
    /// link's text is found, rebased, drawn and named by every one of them,
    /// and one inside code or math by none.
    #[test]
    fn every_walker_reaches_into_the_same_spans() {
        use crate::render::markdown::inline;
        let md = MarkdownConfig::default();
        let d = vault("prose-spans");
        std::fs::write(d.join("pic.png"), b"x").unwrap();
        let pic = "![p](pic.png)";
        for (kind, line) in [
            (Inline::Bold, format!("**{pic}**")),
            (Inline::Italic, format!("*{pic}*")),
            (Inline::BoldItalic, format!("***{pic}***")),
            (Inline::Strikethrough, format!("~~{pic}~~")),
            (Inline::Highlight, format!("=={pic}==")),
            (Inline::Link, format!("[{pic}](u)")),
            (Inline::Code, format!("`{pic}`")),
            (Inline::Latex, format!("${pic}$")),
        ] {
            let spans = inline::scan(&line, &md);
            assert_eq!(spans.first().map(|s| s.kind), Some(kind), "{line}");
            let want = holds_prose(kind);

            let mut found = Vec::new();
            pictures_in(&line, &d, &md, &mut found);
            assert_eq!(!found.is_empty(), want, "pictures_in {line}");
            let moved = rebase_line(&line, Path::new("/notes"), None, &md);
            assert_eq!(moved.contains("/notes/pic.png"), want, "rebase_line {line}: {moved}");
            let page = html::render(&line, "t", &html::Page::default());
            assert_eq!(page.contains("<img"), want, "inline_in {line}: {page}");
            let plain = html::plain_text(&line, &md);
            assert_eq!(!plain.contains("]("), want, "plain_text {line}: {plain}");
            let txt = plain::render(&line, 72, false, &md);
            assert_eq!(txt.contains("[image: p]"), want, "strip_in {line}: {txt}");
        }
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn formats_parse_by_extension_or_name() {
        assert_eq!(Format::parse("md"), Some(Format::Markdown));
        assert_eq!(Format::parse("markdown"), Some(Format::Markdown));
        assert_eq!(Format::parse("TXT"), Some(Format::Text));
        assert_eq!(Format::parse("text"), Some(Format::Text));
        assert_eq!(Format::parse("pdf"), Some(Format::Pdf));
        assert_eq!(Format::parse("html"), Some(Format::Html));
        assert_eq!(Format::parse("docx"), None);
    }

    #[test]
    fn the_suggested_path_is_never_the_source() {
        let src = Path::new("/tmp/notes.md");
        assert_eq!(default_path(src, Format::Text), Path::new("/tmp/notes.txt"));
        assert_eq!(default_path(src, Format::Pdf), Path::new("/tmp/notes.pdf"));
        assert_eq!(default_path(src, Format::Html), Path::new("/tmp/notes.html"));
        // Markdown would be the source itself, so it takes a suffix.
        assert_eq!(default_path(src, Format::Markdown), Path::new("/tmp/notes-export.md"));
        // Given relative, and with no folder at all.
        assert_eq!(default_path(Path::new("notes.md"), Format::Markdown), Path::new("notes-export.md"));
        assert_eq!(default_path(Path::new("a/b.md"), Format::Markdown), Path::new("a/b-export.md"));
        // A source with another extension keeps the plain name.
        assert_eq!(default_path(Path::new("notes.markdown"), Format::Markdown), Path::new("notes.md"));
    }

    /// Whether the plain name is the source is the file system's call, not
    /// the spelling's (a link, or `notes.MD` on a case-insensitive disk).
    #[test]
    fn the_default_path_asks_the_file_system() {
        let d = vault("default");
        let src = d.join("notes.md");
        std::fs::write(&src, "x\n").unwrap();
        #[cfg(unix)]
        {
            let other = d.join("other.markdown");
            std::fs::write(&other, "y\n").unwrap();
            std::os::unix::fs::symlink(&other, d.join("other.md")).unwrap();
            assert_eq!(default_path(&other, Format::Markdown), d.join("other-export.md"));
        }
        assert_eq!(default_path(&src, Format::Markdown), d.join("notes-export.md"));
        // And the default is accepted by `write`.
        let dest = default_path(&src, Format::Markdown);
        write(&src, &dest, Format::Markdown, &TranscludeConfig::default(), &html::Page::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "x\n");
        std::fs::remove_dir_all(&d).ok();
    }

    fn vault(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shoin-ex-{tag}-{t}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn exporting_markdown_flattens_the_embeds() {
        let d = vault("md");
        std::fs::write(d.join("frag.md"), "# Frag\n\nfragment body\n").unwrap();
        let src = d.join("note.md");
        std::fs::write(&src, "# Note\n\n![[frag]]\n").unwrap();

        let dest = d.join("out.md");
        write(&src, &dest, Format::Markdown, &TranscludeConfig::default(), &html::Page::default()).unwrap();
        let got = std::fs::read_to_string(&dest).unwrap();
        assert!(got.contains("fragment body"));
        assert!(got.contains("## Frag"), "demoted, as the compiled form is");
        assert!(!got.contains("![["));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn exporting_text_strips_the_markup_too() {
        let d = vault("txt");
        std::fs::write(d.join("frag.md"), "# Frag\n\nwith **bold** in it\n").unwrap();
        let src = d.join("note.md");
        std::fs::write(&src, "# Note\n\n![[frag]]\n").unwrap();

        let dest = d.join("out.txt");
        write(&src, &dest, Format::Text, &TranscludeConfig::default(), &html::Page::default()).unwrap();
        let got = std::fs::read_to_string(&dest).unwrap();
        assert!(got.contains("with bold in it"), "markers gone");
        assert!(!got.contains("**"));
        assert!(!got.contains("##"), "and headings underlined instead");
        assert!(got.contains("----"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// `page.justify` reaches both formats that have a layout of their own to
    /// stretch: `txt` wraps and pads with spaces (`export::plain`), `html`
    /// gets a class the stylesheet justifies (`export::html`). `md` has no
    /// visual layout of its own, so the flag has nothing to do there.
    #[test]
    fn justify_reaches_the_txt_and_html_exports() {
        let d = vault("justify");
        let src = d.join("note.md");
        std::fs::write(&src, "aaa bbb ccc ddd\n").unwrap();
        let page = html::Page { measure: 10, justify: true, ..html::Page::default() };

        let txt = d.join("out.txt");
        write(&src, &txt, Format::Text, &TranscludeConfig::default(), &page).unwrap();
        let got = std::fs::read_to_string(&txt).unwrap();
        assert!(got.contains("aaa    bbb"), "stretched to the measure: {got:?}");

        let html_dest = d.join("out.html");
        write(&src, &html_dest, Format::Html, &TranscludeConfig::default(), &page).unwrap();
        let got = std::fs::read_to_string(&html_dest).unwrap();
        assert!(got.contains("<main class=\"justify\">"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// HTML needs nothing installed — that is the whole reason it is not
    /// another pandoc call — and it flattens the embeds like every other
    /// format.
    #[test]
    fn exporting_html_needs_no_tools_and_flattens_the_embeds() {
        let d = vault("html");
        std::fs::write(d.join("frag.md"), "# Frag\n\nfragment **body**\n").unwrap();
        let src = d.join("note.md");
        std::fs::write(&src, "# Note\n\n![[frag]]\n").unwrap();

        let dest = d.join("out.html");
        write(&src, &dest, Format::Html, &TranscludeConfig::default(), &html::Page::default())
            .unwrap();
        let got = std::fs::read_to_string(&dest).unwrap();

        assert!(got.starts_with("<!doctype html>"));
        assert!(got.contains("<title>Note</title>"), "named by its own first heading");
        assert!(got.contains("<strong>body</strong>"), "the embed was flattened in");
        assert!(!got.contains("![["), "and its link is gone");
        std::fs::remove_dir_all(&d).ok();
    }

    /// The destination is typed into a dialog, so a folder that does not exist
    /// yet is a request, not a mistake.
    #[test]
    fn export_makes_the_folder_you_asked_for() {
        let d = vault("mkdir");
        let src = d.join("note.md");
        std::fs::write(&src, "body\n").unwrap();

        let dest = d.join("out/deeper/final.txt");
        write(&src, &dest, Format::Text, &TranscludeConfig::default(), &html::Page::default()).unwrap();
        assert!(dest.is_file(), "the path was created on the way");
        assert!(std::fs::read_to_string(&dest).unwrap().contains("body"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// PDF goes through pandoc plus a layout engine. Both are optional on any
    /// given machine, so this asserts the RIGHT THING for whichever is present:
    /// a real PDF when the tools are there, and an error that names what is
    /// missing when they are not. A test that silently skips would tell us
    /// nothing on the machine that matters.
    #[test]
    fn pdf_export_either_works_or_says_what_is_missing() {
        let d = vault("pdf");
        let src = d.join("note.md");
        std::fs::write(&src, "# Title\n\nSome prose.\n").unwrap();
        let dest = d.join("out.pdf");

        let have_pandoc = which("pandoc").is_some();
        let have_engine = PDF_ENGINES.iter().any(|e| which(e).is_some());
        let result = write(&src, &dest, Format::Pdf, &TranscludeConfig::default(), &html::Page::default());

        match (have_pandoc, have_engine) {
            (true, true) => {
                result.expect("pandoc and an engine are installed");
                let bytes = std::fs::read(&dest).unwrap();
                assert!(bytes.starts_with(b"%PDF-"), "a real PDF, not an empty file");
                assert!(bytes.len() > 1000, "…with something in it");
            }
            (false, _) => {
                let e = result.unwrap_err().to_string();
                assert!(e.contains("pandoc"), "name the missing tool: {e}");
                assert!(e.len() < 64, "short enough for the status line: {e}");
            }
            (true, false) => {
                let e = result.unwrap_err().to_string();
                assert!(e.contains("engine"), "name what is missing: {e}");
                assert!(e.contains("typst"), "…and the cheapest fix: {e}");
                assert!(e.len() < 64, "short enough for the status line: {e}");
            }
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// The status line is one row, so the hint must fit it whole.
    #[test]
    fn the_no_engine_hint_names_typst_and_fits_the_status_line() {
        assert!(NO_ENGINE.contains("engine") && NO_ENGINE.contains("typst"), "{NO_ENGINE}");
        assert!(NO_ENGINE.len() < 64, "{NO_ENGINE}");
    }

    #[test]
    fn typst_is_given_a_font_and_the_others_are_left_alone() {
        assert!(engine_args("typst").iter().any(|a| a.starts_with("--variable=mainfont:")));
        assert!(engine_args("typst").contains(&"--pdf-engine-opt=--root=/"));
        assert!(engine_args("xelatex").is_empty());
    }

    /// A valid 1x1 PNG, which a PDF engine has to decode for real.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE, 0x00,
        0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01,
        0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60,
        0x82,
    ];

    #[cfg(unix)]
    #[test]
    fn pdf_mermaid_pngs_live_through_pandoc_and_are_cleaned_after_failure() {
        const TEST_PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
            0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00,
            0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0x0f, 0x04, 0x00,
            0x09, 0xfb, 0x03, 0xfd, 0xfb, 0x5e, 0x6b, 0x2b, 0x00, 0x00, 0x00, 0x00,
            0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];

        let d = vault("pdf-mermaid");
        std::fs::write(d.join("diagram.png"), TEST_PNG).unwrap();
        let renderer = script(
            &d,
            "renderer",
            "#!/bin/sh\n[ -n \"$SHOIN_PROBE\" ] && exit 0\ncase $(cat) in *broken*) exit 1;; esac\ncat \"$(dirname \"$0\")/diagram.png\"\n",
            true,
        );
        let pandoc = script(
            &d,
            "pandoc",
            r#"#!/bin/sh
[ -n "$SHOIN_PROBE" ] && exit 0
while [ "$#" -gt 1 ] && [ "$1" != --output ]; do shift; done
dest=$2
cat > "$dest.md"
sed -n 's/.*](<\(.*\.png\)>).*/\1/p' "$dest.md" > "$dest.paths"
while IFS= read -r p; do
    cmp "$p" "$(dirname "$0")/diagram.png" || exit 9
    [ "$(ls -ld "$p" | cut -c 5-10)" = '------' ] || exit 9
done < "$dest.paths"
case "$dest" in *fail.pdf) echo 'fake engine failed' >&2; exit 8;; esac
printf '%%PDF-fake\n' > "$dest"
"#,
            true,
        );
        let md = MarkdownConfig {
            mermaid_renderer: Some(renderer.to_str().unwrap().into()),
            ..Default::default()
        };
        let markdown = concat!(
            "Before\n```mermaid\nflowchart LR\nA --> B\n```\nBetween\n\n",
            "```mermaid\nflowchart LR\nA --> B\n```\n\n~~~mermaid\nbroken\n~~~\n",
        );
        for filename in ["out.pdf", "fail.pdf"] {
            let dest = d.join(filename);
            let result = write_pdf_with(markdown, &d.join("source.md"), &dest, &md, &pandoc, "typst", Default::default());
            if filename == "out.pdf" {
                let message = result.unwrap().unwrap();
                assert!(message.contains("rendering failed"), "{message}");
                assert!(dest.is_file());
            } else {
                let error = result.unwrap_err().to_string();
                assert_eq!(error, "pandoc: fake engine failed");
            }
            let input = std::fs::read_to_string(d.join(format!("{filename}.md"))).unwrap();
            assert!(input.starts_with("Before\n\n![Mermaid"), "{input}");
            assert!(input.contains(">)\n\nBetween"), "{input}");
            assert!(!input.contains("A --> B"));
            assert!(input.contains("~~~mermaid\nbroken\n~~~"));
            let paths = std::fs::read_to_string(d.join(format!("{filename}.paths"))).unwrap();
            let paths: Vec<_> = paths.lines().collect();
            assert_eq!(paths.len(), 2);
            assert_eq!(paths[0], paths[1], "duplicate diagrams share their PNG");
            let path = Path::new(paths[0]);
            assert!(!path.exists() && !path.parent().unwrap().exists());
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// `notes/a.md` with a picture in `notes/img/`, and `notes/my pic (1).png`.
    fn document_with_pictures(d: &Path) -> PathBuf {
        let notes = d.join("notes");
        std::fs::create_dir_all(notes.join("img")).unwrap();
        std::fs::write(notes.join("img/p.png"), PNG).unwrap();
        std::fs::write(notes.join("my pic (1).png"), PNG).unwrap();
        let src = notes.join("a.md");
        std::fs::write(
            &src,
            "# A\n\n![pic](img/p.png) and **![t](<my pic (1).png> \"A \\\"title\\\"\")**\n\n\
             ![web](https://x.y/p.png) ![abs](/abs/p.png) `![code](img/p.png)`\n\n\
             ```\n![fenced](img/p.png)\n```\n\n> ```\n> ![quoted](img/p.png)\n> ```\n",
        )
        .unwrap();
        src
    }

    /// pandoc runs in the working directory, not the document's: the
    /// document's folder is on its resource path, so a relative picture
    /// is found from anywhere.
    #[test]
    fn pdf_export_finds_pictures_beside_the_document() {
        if which("pandoc").is_none() || !PDF_ENGINES.iter().any(|e| which(e).is_some()) {
            return;
        }
        let d = vault("pdf-img");
        let src = d.join("notes/a.md");
        std::fs::create_dir_all(d.join("notes/img")).unwrap();
        std::fs::write(d.join("notes/img/p.png"), PNG).unwrap();
        std::fs::write(d.join("notes/my pic (1).png"), PNG).unwrap();
        std::fs::write(&src, "# A\n\n![pic](img/p.png) ![b](<my pic (1).png>)\n").unwrap();
        let dest = d.join("out/a.pdf");
        write(&src, &dest, Format::Pdf, &TranscludeConfig::default(), &html::Page::default()).unwrap();
        let bytes = std::fs::read(&dest).unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert!(
            bytes.windows(13).any(|w| w == b"/Subtype /Ima") || bytes.windows(14).any(|w| w == b"/Subtype/Image"),
            "the picture is in the PDF"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// Exported elsewhere, a relative picture is rewritten to be relative to
    /// the export; beside the source, the text is left exactly as it was.
    #[test]
    fn markdown_export_keeps_pictures_working_from_another_folder() {
        let d = vault("md-img");
        let src = document_with_pictures(&d);
        let cfg = TranscludeConfig::default();
        let page = html::Page::default();

        let beside = d.join("notes/flat.md");
        write(&src, &beside, Format::Markdown, &cfg, &page).unwrap();
        assert_eq!(
            std::fs::read_to_string(&beside).unwrap(),
            std::fs::read_to_string(&src).unwrap(),
            "nothing to rewrite beside the source"
        );

        let dest = d.join("out/deeper/a.md");
        write(&src, &dest, Format::Markdown, &cfg, &page).unwrap();
        let got = std::fs::read_to_string(&dest).unwrap();
        assert!(got.contains("![pic](../../notes/img/p.png)"), "{got}");
        assert!(got.contains("**![t](<../../notes/my pic (1).png> \"A \\\"title\\\"\")**"), "{got}");
        assert!(got.contains("![web](https://x.y/p.png) ![abs](/abs/p.png)"), "addresses stay: {got}");
        assert!(got.contains("`![code](img/p.png)`"), "code is text: {got}");
        assert!(got.contains("\n![fenced](img/p.png)\n"), "so is a fence: {got}");
        assert!(got.contains("\n> ![quoted](img/p.png)\n"), "and a fence in a quote: {got}");
        // Every rewritten path names a real file from the export's folder.
        for rel in ["../../notes/img/p.png", "../../notes/my pic (1).png"] {
            assert!(d.join("out/deeper").join(rel).is_file(), "{rel}");
        }

        // A sibling folder of the source, given relative to nothing.
        let up = d.join("a.md");
        write(&src, &up, Format::Markdown, &cfg, &page).unwrap();
        let got = std::fs::read_to_string(&up).unwrap();
        assert!(got.contains("![pic](notes/img/p.png)"), "{got}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A picture written `<…>` (spaces, parentheses) reaches the HTML page as
    /// a `data:` URI, title and all.
    #[test]
    fn html_export_inlines_a_picture_with_spaces_in_its_name() {
        let d = vault("html-img");
        let src = document_with_pictures(&d);
        let page = html::Page { base: d.join("notes"), ..html::Page::default() };
        let dest = d.join("out/a.html");
        write(&src, &dest, Format::Html, &TranscludeConfig::default(), &page).unwrap();
        let got = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(got.matches("<img src=\"data:image/png;base64,").count(), 2, "{got}");
        assert!(!got.contains("my pic"), "{got}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// Exporting over the document you are exporting would destroy it.
    #[test]
    fn refuses_to_write_over_the_source() {
        let d = vault("clobber");
        let src = d.join("note.md");
        std::fs::write(&src, "# Note\n").unwrap();

        let err = write(&src, &src, Format::Markdown, &TranscludeConfig::default(), &html::Page::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("overwrite"), "got: {err}");
        assert_eq!(std::fs::read_to_string(&src).unwrap(), "# Note\n");
        std::fs::remove_dir_all(&d).ok();
    }

    /// HTML carries the pictures a document links to inside the page, so it
    /// reads them; writing the page over one would destroy it. A picture
    /// named only by a remote address is never read, and nothing local is
    /// refused because of it.
    #[test]
    fn html_refuses_to_write_over_a_linked_picture() {
        let d = vault("clobber-linked");
        std::fs::create_dir_all(d.join("img")).unwrap();
        std::fs::write(d.join("img/p.png"), PNG).unwrap();
        std::fs::write(d.join("p.png"), PNG).unwrap();
        let src = d.join("note.md");
        std::fs::write(&src, "# Note\n\n![alt](img/p.png) ![web](https://example.com/p.png)\n").unwrap();
        let page = html::Page { base: d.clone(), ..html::Page::default() };
        let err = write(&src, &d.join("img/p.png"), Format::Html, &TranscludeConfig::default(), &page)
            .unwrap_err()
            .to_string();
        assert!(err.contains("overwrite p.png"), "got: {err}");
        assert_eq!(std::fs::read(d.join("img/p.png")).unwrap(), PNG);
        // `p.png` beside the note shares the remote picture's name only.
        write(&src, &d.join("p.png"), Format::Html, &TranscludeConfig::default(), &page).unwrap();
        assert!(std::fs::read_to_string(d.join("p.png")).unwrap().starts_with("<!doctype html>"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// pandoc reads a linked picture to lay it out, so PDF refuses it the
    /// same way. The refusal comes before pandoc is looked for.
    #[test]
    fn pdf_refuses_to_write_over_a_linked_picture() {
        let d = vault("clobber-linked-pdf");
        std::fs::create_dir_all(d.join("img")).unwrap();
        std::fs::write(d.join("img/p.png"), PNG).unwrap();
        let src = d.join("note.md");
        std::fs::write(&src, "# Note\n\n*see ![alt](img/p.png)*\n").unwrap();
        let page = html::Page { base: d.clone(), ..html::Page::default() };
        let err = write(&src, &d.join("img/p.png"), Format::Pdf, &TranscludeConfig::default(), &page)
            .unwrap_err()
            .to_string();
        assert!(err.contains("overwrite p.png"), "got: {err}");
        assert_eq!(std::fs::read(d.join("img/p.png")).unwrap(), PNG);
        std::fs::remove_dir_all(&d).ok();
    }

    /// Plain text and HTML read a document's paragraphs the same way: a
    /// line ending in a backslash is a hard break in one exactly when it
    /// is in the other (the backslash goes from `txt`, `<br>` in `html`).
    #[test]
    fn text_and_html_agree_on_what_continues_a_paragraph() {
        let md = MarkdownConfig::default();
        for (doc, joins) in [
            ("x\\\nnext\n", true),
            ("x\\\n\nnext\n", false),
            ("x\\\n# Heading\n", false),
            ("> x\\\n> next\n", true),
            ("> x\\\n>\n> next\n", false),
            ("> x\\\n> > next\n", false),
            ("> x\\\nlazy\n", true),
            ("> > x\\\n> next\n", true),
            ("> > x\\\nnext\n", true),
            ("> > x\\\n>\n> next\n", false),
            ("> > x\\\n> - next\n", false),
            ("> a\nx\\\n> next\n", true),
            ("x\\\n> next\n", false),
            ("> - x\\\n> - next\n", false),
            ("> x\\\n> # next\n", false),
            ("x\\\n| a | b |\n|---|---|\n| 1 | 2 |\n", false),
            ("x\\\n| a | b |\nnext\n", true),
            ("- x\\\n  next\n", true),
        ] {
            let txt = plain::render(doc, 72, false, &md);
            let page = html::render(doc, "t", &html::Page::default());
            assert_eq!(!txt.contains("x\\"), joins, "txt {doc:?}: {txt:?}");
            assert_eq!(page.contains("x<br>"), joins, "html {doc:?}: {page}");
        }
    }

    /// An embedded note or picture is as much the document as its own file:
    /// exporting onto one would overwrite it. The check is by file, not by
    /// spelling, so a path through `..`, a symlink or a hard link is caught.
    #[test]
    fn refuses_to_write_over_anything_the_document_embeds() {
        let d = vault("clobber-embeds");
        std::fs::create_dir_all(d.join("parts")).unwrap();
        std::fs::write(d.join("parts/frag.md"), "Fragment.\n").unwrap();
        std::fs::write(d.join("pic.png"), PNG).unwrap();
        let src = d.join("note.md");
        std::fs::write(&src, "# Note\n\n![[parts/frag]]\n\n![[pic.png]]\n").unwrap();
        let mut dests = vec![
            (d.join("parts/frag.md"), Format::Markdown),
            (d.join("parts/../parts/frag.md"), Format::Text),
            (d.join("pic.png"), Format::Html),
        ];
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.join("parts/frag.md"), d.join("link.md")).unwrap();
            std::fs::hard_link(&src, d.join("hard.md")).unwrap();
            dests.push((d.join("link.md"), Format::Markdown));
            dests.push((d.join("hard.md"), Format::Markdown));
        }
        for (dest, format) in dests {
            let err = write(&src, &dest, format, &TranscludeConfig::default(), &html::Page::default())
                .expect_err(&format!("{}", dest.display()))
                .to_string();
            assert!(err.contains("overwrite"), "{}: {err}", dest.display());
        }
        assert_eq!(std::fs::read_to_string(d.join("parts/frag.md")).unwrap(), "Fragment.\n");
        assert_eq!(std::fs::read(d.join("pic.png")).unwrap(), PNG);
        assert!(std::fs::read_to_string(&src).unwrap().starts_with("# Note"));
        // A file beside them that the document does not read is fine.
        write(&src, &d.join("parts/out.md"), Format::Markdown, &TranscludeConfig::default(), &html::Page::default())
            .unwrap();
        std::fs::remove_dir_all(&d).ok();
    }

    // -- MathML through pandoc ------------------------------------------------

    fn formula(tex: &str, display: bool) -> html::Formula {
        html::Formula { tex: tex.into(), display }
    }

    #[test]
    fn the_formulas_go_to_pandoc_as_one_document() {
        let doc = mathml_source(&[formula(" x^2 ", false), formula("a\n\n\"b\\c\u{1}", true)], "1,23,1,2");
        assert_eq!(
            doc,
            "{\"pandoc-api-version\":[1,23,1,2],\"meta\":{},\"blocks\":[\
             {\"t\":\"Div\",\"c\":[[\"shoin-math-0\",[],[]],[{\"t\":\"Plain\",\"c\":[{\"t\":\"Math\",\"c\":[{\"t\":\"InlineMath\"},\" x^2 \"]}]}]]},\
             {\"t\":\"Div\",\"c\":[[\"shoin-math-1\",[],[]],[{\"t\":\"Plain\",\"c\":[{\"t\":\"Math\",\"c\":[{\"t\":\"DisplayMath\"},\"a\\n\\n\\\"b\\\\c\\u0001\"]}]}]]}\
             ]}"
        );
        assert_eq!(mathml_source(&[], "1,23"), "{\"pandoc-api-version\":[1,23],\"meta\":{},\"blocks\":[]}");
    }

    /// The review's case: TeX holding `$$`, a marker of the old Markdown
    /// framing and a `<div>` stays inside its one JSON string.
    #[test]
    fn formula_text_stays_one_json_string() {
        let tex = "x\ny $$ remainder\n<!-- shoin-math 1 -->\n<div id=\"shoin-math-1\">\nz";
        let doc = mathml_source(&[formula(tex, true), formula("y", false)], "1,23");
        assert!(doc.contains(r#"{"t":"DisplayMath"},"x\ny $$ remainder\n<!-- shoin-math 1 -->\n<div id=\"shoin-math-1\">\nz"]"#), "{doc}");
        assert_eq!(doc.matches("\"t\":\"Div\"").count(), 2);
    }

    #[test]
    fn the_api_version_comes_from_pandocs_empty_document() {
        assert_eq!(api_version("{\"pandoc-api-version\":[1,23,1,2],\"meta\":{},\"blocks\":[]}\n"), Some("1,23,1,2"));
        assert_eq!(api_version("{\"pandoc-api-version\":[1,17]}"), Some("1,17"));
        for bad in ["", "[]", "{\"blocks\":[]}", "{\"pandoc-api-version\":[]}", "{\"pandoc-api-version\":[1,,2]}", "{\"pandoc-api-version\":[1,\"x\"]}", "{\"pandoc-api-version\":[1,2"] {
            assert_eq!(api_version(bad), None, "{bad}");
        }
    }

    /// pandoc 3.14's page for a converted inline and display formula and one
    /// it could not convert; an older pandoc's span wrapper.
    #[test]
    fn pandocs_page_is_cut_back_into_formulas() {
        let page = "<div id=\"shoin-math-0\">\n\
            <math display=\"inline\" xmlns=\"http://www.w3.org/1998/Math/MathML\"><semantics><mi>x</mi>\
            <annotation encoding=\"application/x-tex\">x</annotation></semantics></math>\n</div>\n\
            <div id=\"shoin-math-1\">\n\
            <math display=\"block\" xmlns=\"http://www.w3.org/1998/Math/MathML\"><mfrac><mi>a</mi><mi>b</mi></mfrac></math>\n</div>\n\
            <div id=\"shoin-math-2\">\n<span class=\"math inline\">$x \\foo{y} &amp; &lt;b&gt;$</span>\n</div>\n\
            <div id=\"shoin-math-3\">\n<span class=\"math display\"><math>a</math></span>\n</div>\n";
        let got = mathml_split(page, 4);
        assert_eq!(
            got,
            vec![
                Some("<math display=\"inline\"><semantics><mi>x</mi><annotation encoding=\"application/x-tex\">x</annotation></semantics></math>".into()),
                Some("<math display=\"block\"><mfrac><mi>a</mi><mi>b</mi></mfrac></math>".into()),
                None,
                Some("<math>a</math>".into()),
            ]
        );
        // Two formulas in one piece are not one formula, nor is one whose
        // `<div>` does not close.
        assert_eq!(mathml_split("<div id=\"shoin-math-0\">\n<math>a</math> <math>b</math>\n</div>", 1), vec![None]);
        assert_eq!(mathml_split("<div id=\"shoin-math-0\">\n<math>a</math>", 1), vec![None]);
    }

    fn mathml_div(i: impl std::fmt::Display) -> String {
        format!("<div id=\"shoin-math-{i}\">\n<math>{i}</math>\n</div>\n")
    }

    /// `<div>`s that cannot be matched to formulas, one by one, convert none.
    #[test]
    fn divs_out_of_step_convert_nothing() {
        let m = |i: u32| mathml_div(i);
        assert_eq!(mathml_split(&(m(0) + &m(1)), 2), vec![Some("<math>0</math>".into()), Some("<math>1</math>".into())]);
        assert_eq!(mathml_split(&m(0), 2), vec![None, None], "one missing");
        assert_eq!(mathml_split(&(m(0) + &m(1)), 1), vec![None], "one too many");
        assert_eq!(mathml_split(&(m(1) + &m(0)), 2), vec![None, None], "out of order");
        assert_eq!(mathml_split(&(m(0) + &m(0) + &m(1)), 2), vec![None, None], "repeated");
        assert_eq!(mathml_split(&(m(0) + &mathml_div("1x")), 2), vec![None, None], "not a number");
        assert_eq!(mathml_split(&(m(0) + &mathml_div("")), 2), vec![None, None], "no number");
        assert_eq!(mathml_split("", 0), Vec::<Option<String>>::new());
    }

    /// A file at `dir/name` holding `body`, made executable or not.
    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str, exec: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
        if exec {
            // Another test thread forking while the file was open for writing
            // makes exec fail with "text file busy" for a moment.
            for _ in 0..100 {
                match std::process::Command::new(&p).env("SHOIN_PROBE", "1").status() {
                    Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(std::time::Duration::from_millis(10)),
                    _ => break,
                }
            }
        }
        p
    }

    /// A `pandoc` that never answers is killed at the deadline, and the math
    /// stays TeX.
    #[cfg(unix)]
    #[test]
    fn a_stalled_pandoc_times_out() {
        let d = vault("stall");
        let tool = script(&d, "pandoc", "#!/bin/sh\n[ -n \"$SHOIN_PROBE\" ] && exit 0\nexec sleep 30\n", true);
        let t = std::time::Instant::now();
        let got = mathml_with(&tool, &[formula("x", false)], std::time::Duration::from_millis(300));
        assert_eq!(got, Err("math left as TeX: pandoc timed out"));
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
        std::fs::remove_dir_all(&d).ok();
    }

    /// The runner end to end with a `pandoc` that answers like the real one:
    /// two runs, the second reading the JSON document; a failing one leaves
    /// the math as TeX.
    #[cfg(unix)]
    #[test]
    fn a_fake_pandoc_converts_in_two_runs() {
        let d = vault("fake");
        let log = d.join("log");
        let body = format!(
            "#!/bin/sh\n[ -n \"$SHOIN_PROBE\" ] && exit 0\necho \"$*\" >> '{}'\n\
             case \"$1\" in --from=markdown) cat >/dev/null; echo '{{\"pandoc-api-version\":[1,23,1,2],\"meta\":{{}},\"blocks\":[]}}';;\n\
             *) grep -q '\"pandoc-api-version\":\\[1,23,1,2\\]' || exit 3; printf '<div id=\"shoin-math-0\">\\n<math>x</math>\\n</div>\\n';;\nesac\n",
            log.display()
        );
        let tool = script(&d, "pandoc", &body, true);
        let limit = std::time::Duration::from_secs(10);
        assert_eq!(mathml_with(&tool, &[formula("x", false)], limit), Ok(vec![Some("<math>x</math>".into())]));
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "--from=markdown --to=json\n--from=json --to=html --mathml\n");

        let broken = script(&d, "broken", "#!/bin/sh\n[ -n \"$SHOIN_PROBE\" ] && exit 0\nexit 1\n", true);
        assert_eq!(mathml_with(&broken, &[formula("x", false)], limit), Err("math left as TeX: pandoc failed"));
        assert_eq!(mathml_with(&d.join("missing"), &[formula("x", false)], limit), Err("math left as TeX: pandoc failed"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// A file named `pandoc` that cannot be run is passed over for the next
    /// folder's.
    #[cfg(unix)]
    #[test]
    fn path_lookup_skips_files_that_cannot_run() {
        let d = vault("which");
        let (a, b, c) = (d.join("a"), d.join("b"), d.join("c"));
        for dir in [&a, &b, &c] {
            std::fs::create_dir_all(dir).unwrap();
        }
        script(&a, "pandoc", "not a program", false);
        std::fs::create_dir_all(b.join("pandoc")).unwrap();
        let good = script(&c, "pandoc", "#!/bin/sh\n", true);
        let path = std::env::join_paths([&a, &b, &c]).unwrap();
        assert_eq!(which_in("pandoc", &path), Some(good));
        assert_eq!(which_in("pandoc", &std::env::join_paths([&a, &b]).unwrap()), None);
        std::fs::remove_dir_all(&d).ok();
    }

    /// Cutting pandoc's page apart takes one pass, however many formulas.
    #[test]
    fn many_formulas_split_in_one_pass() {
        let n = 20_000;
        let mut page = String::new();
        for i in 0..n {
            page.push_str(&format!("<div id=\"shoin-math-{i}\">\n<math display=\"inline\"><mi>x</mi></math>\n</div>\n"));
        }
        let t = std::time::Instant::now();
        let got = mathml_split(&page, n);
        assert!(t.elapsed() < std::time::Duration::from_secs(2), "{:?}", t.elapsed());
        assert!(got.iter().all(|m| m.as_deref() == Some("<math display=\"inline\"><mi>x</mi></math>")));
    }

    /// The review's case: a display formula whose text holds `$$` and a
    /// marker. It may stay TeX, but the next formula still converts.
    #[test]
    #[ignore = "needs pandoc on PATH"]
    fn pandoc_formula_text_cannot_change_the_framing() {
        let text = "$$x\ny $$ remainder\n<!-- shoin-math 1 -->\nz$$\n\n$y$\n";
        let (page, note) = html::render_math(text, "t", &html::Page::default());
        assert_eq!(note, None, "pandoc is not on PATH");
        assert!(page.contains("<span class=\"math inline\"><math"), "$y$ stayed TeX: {page}");
    }

    /// The real thing, where pandoc is installed: `cargo test -- --ignored
    /// pandoc`. Every formula of `examples/math.md` is MathML.
    #[test]
    #[ignore = "needs pandoc on PATH"]
    fn pandoc_turns_the_example_math_into_mathml() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/math.md");
        let text = std::fs::read_to_string(&src).unwrap();
        let (page, note) = html::render_math(&text, "math", &html::Page::default());
        assert_eq!(note, None, "pandoc is not on PATH");
        assert!(!page.contains("\\(") && !page.contains("\\["), "a formula stayed TeX: {page}");
        let formulas = page.matches("class=\"math ").count();
        assert!(formulas > 10, "{page}");
        assert_eq!(page.matches("<math ").count(), formulas, "{page}");
        assert!(!page.contains("<script") && !page.contains("http"), "{page}");
    }
}
