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

use crate::config::schema::TranscludeConfig;

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

/// Where an export defaults to: beside the source, same stem, new extension.
///
/// Never the source itself — that is checked at the call site as well, because
/// a suggestion the reader can edit is only a suggestion.
pub fn default_path(source: &Path, format: Format) -> PathBuf {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "export".into());
    source.with_file_name(format!("{stem}.{}", format.extension()))
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
    if crate::transclude::same_file(dest, source) {
        bail!("that would overwrite the document you are exporting");
    }
    let flat = crate::transclude::compile::compile(source, cfg)?;
    // The destination is typed into a dialog, so a folder that does not exist
    // yet is a reasonable thing to ask for rather than a mistake to report.
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", short(parent)))?;
    }
    match format {
        Format::Markdown => std::fs::write(dest, flat).with_context(|| short(dest))?,
        Format::Text => {
            std::fs::write(dest, plain::render(&flat)).with_context(|| short(dest))?
        }
        Format::Html => {
            let title = source
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "document".into());
            std::fs::write(dest, html::render(&flat, &title, page)).with_context(|| short(dest))?
        }
        Format::Pdf => write_pdf(&flat, dest)?,
    }
    Ok(())
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
fn write_pdf(markdown: &str, dest: &Path) -> Result<()> {
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
        bail!("pandoc has no PDF engine — try: brew install typst");
    };

    let mut child = Command::new(tool)
        .arg("--from=markdown")
        .arg(format!("--pdf-engine={engine}"))
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

/// The first executable named `name` on `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
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
        // Markdown is the one that could collide, and does not: the flattened
        // document is a different file from the composition.
        let md = default_path(src, Format::Markdown);
        assert_eq!(md, Path::new("/tmp/notes.md"));
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
}
