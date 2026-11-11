//! `:export` — write the finished document out. SPEC.md §14.4.
//!
//! "Finished" means every `![[…]]` resolved and flattened, which is the same
//! text `:embed full` shows on screen. The four formats are the four things
//! people do with a finished piece: keep it as Markdown, read it as plain text,
//! put it on the web, or hand it to someone who wants a printed page.

pub mod html;
pub mod plain;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::schema::{MarkdownConfig, TranscludeConfig};
use crate::render::markdown::block::{classify, BlockKind, Carry};

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
/// `page` only reaches HTML, which is the one format with a look of its own to
/// get right; the others are text and are the same on every machine.
pub fn write(
    source: &Path,
    dest: &Path,
    format: Format,
    cfg: &TranscludeConfig,
    page: &html::Page,
) -> Result<()> {
    // Every file the compile read is part of what is being exported, so
    // writing onto any of them would destroy the document: the source, an
    // embedded note, an embedded picture. `sources` starts with the source.
    let (flat, sources) = crate::transclude::compile::compile_with_sources(source, cfg)?;
    if let Some(i) = sources.iter().position(|s| is_same_file(dest, s)) {
        match i {
            0 => bail!("that would overwrite the document you are exporting"),
            _ => bail!("that would overwrite {}, which the document embeds", short(&sources[i])),
        }
    }
    // The destination is typed into a dialog, so a folder that does not exist
    // yet is a reasonable thing to ask for rather than a mistake to report.
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", short(parent)))?;
    }
    match format {
        Format::Markdown => {
            let flat = rebase_images(&flat, source, Some(dest), &page.md);
            std::fs::write(dest, flat).with_context(|| short(dest))?
        }
        Format::Text => {
            std::fs::write(dest, plain::render(&flat, page.measure, page.justify, &page.md))
                .with_context(|| short(dest))?
        }
        Format::Html => {
            let title = source
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "document".into());
            std::fs::write(dest, html::render(&flat, &title, page)).with_context(|| short(dest))?
        }
        Format::Pdf => {
            let flat = rebase_images(&flat, source, None, &page.md);
            write_pdf(&flat, source, dest, page.md.math)?
        }
    }
    Ok(())
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
    for (i, raw) in markdown.split_inclusive('\n').enumerate() {
        let line = raw.trim_end_matches(['\n', '\r']);
        let (kind, next) = classify(line, &carry, i == 0, md.math);
        carry = next;
        match kind {
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
    use crate::render::markdown::inline::{self, Inline};
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
            Inline::Bold | Inline::Italic | Inline::BoldItalic | Inline::Strikethrough | Inline::Highlight => {
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

/// Whether writing to `dest` would write to `file`. A hard link is a second
/// name for the same file, which only the inode shows; elsewhere the resolved
/// paths are compared. A `dest` that does not exist yet is no file at all.
fn is_same_file(dest: &Path, file: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(dest), std::fs::metadata(file)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    dest.exists() && crate::transclude::same_file(dest, file)
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
/// DELIBERATELY not built in. A PDF writer means a layout engine and embedded
/// fonts inside a terminal text editor, for output worse than what the tools
/// people already have produce. `pandoc` was named as the pipeline in SPEC
/// §14.4 before any of this existed; this just runs it for you.
fn write_pdf(markdown: &str, source: &Path, dest: &Path, math: bool) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

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

    let mut child = Command::new(tool)
        // pandoc reads `$…$` as math by default; with `math = false` followed
        // into export, it is the text the editor shows.
        .arg(if math { "--from=markdown" } else { "--from=markdown-tex_math_dollars" })
        .arg(format!("--pdf-engine={engine}"))
        .args(engine_args(engine))
        // pandoc runs here, not in the document's folder. Pictures arrive
        // with absolute paths (`rebase_images`); the resource path covers
        // any left as written, for the engines that read it.
        .arg(resource_path(source))
        .arg("--output")
        .arg(dest)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("running pandoc")?;

    child
        .stdin
        .as_mut()
        .context("pandoc took no input")?
        .write_all(markdown.as_bytes())
        .context("sending the document to pandoc")?;

    let out = child.wait_with_output().context("waiting for pandoc")?;
    if !out.status.success() {
        // pandoc's own diagnostics are far better than anything we could infer
        // — a missing LaTeX engine, say — so they are passed through.
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.lines().next().unwrap_or("pandoc failed").trim();
        bail!("pandoc: {why}");
    }
    Ok(())
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
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
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
             ```\n![fenced](img/p.png)\n```\n",
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
}
