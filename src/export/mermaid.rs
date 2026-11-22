//! Find closed Mermaid fences in flattened Markdown and render a bounded
//! batch of static PNGs for HTML or PDF.

use std::ops::Range;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::schema::MarkdownConfig;
use crate::render::markdown::block::{classify, BlockKind, Carry, COMMONMARK_TAB};
use crate::render::mermaid::{Failure, MermaidRenderer, PicturePreview, MAX_CACHE_ENTRIES, MAX_SOURCE_BYTES};

const BATCH_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_DOCUMENT_PNG_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct RenderedFence<'a> {
    /// Zero-based line numbers, including the opening and closing lines.
    pub(crate) lines: Range<usize>,
    /// The complete fence, including any line ending on its closing line.
    pub(crate) bytes: Range<usize>,
    pub(crate) indent: &'a str,
    pub(crate) alt: &'static str,
    pub(crate) png: Result<Arc<[u8]>, Failure>,
}

struct Fence<'a> {
    lines: Range<usize>,
    bytes: Range<usize>,
    indent: &'a str,
    body: &'a str,
}

pub(crate) fn render<'a>(markdown: &'a str, md: &MarkdownConfig) -> Vec<RenderedFence<'a>> {
    let Some(executable) = md.mermaid_renderer.as_deref().filter(|path| !path.trim().is_empty()) else {
        return Vec::new();
    };

    let mut sources = Vec::new();
    let prepared: Vec<_> = fences(markdown, md.math)
        .map(|fence| {
            let job = prepare(&fence, &mut sources);
            (fence, job)
        })
        .collect();

    let images = raster_batch(executable, &sources);
    let mut remaining = MAX_DOCUMENT_PNG_BYTES;
    prepared.into_iter().map(|(fence, job)| {
        let png = job.and_then(|job| {
            let bytes = images[job].clone().unwrap_or(Err(Failure::Timeout))?;
            // Count repeated diagrams too. Each instance can contribute its
            // payload again when serialized into an HTML document.
            remaining = remaining.checked_sub(bytes.len()).ok_or(Failure::TooLarge)?;
            Ok(bytes)
        });
        RenderedFence {
            lines: fence.lines,
            bytes: fence.bytes,
            indent: fence.indent,
            alt: diagram_alt(fence.body),
            png,
        }
    }).collect()
}

pub(crate) fn diagnostic(fences: &[RenderedFence<'_>]) -> Option<&'static str> {
    let has = |failure| fences.iter().any(|fence| fence.png.as_ref().err() == Some(&failure));
    if has(Failure::MissingExecutable) {
        Some("Mermaid left as source. Install merman-cli or check mermaid_renderer.")
    } else if has(Failure::Timeout) {
        Some("Mermaid diagrams left as source because rendering timed out")
    } else if has(Failure::TooLarge) {
        Some("Mermaid diagrams left as source because a size or count limit was reached")
    } else if fences.iter().any(|fence| fence.png.is_err()) {
        Some("Mermaid diagrams left as source because rendering failed")
    } else {
        None
    }
}

fn fences(markdown: &str, math: bool) -> impl Iterator<Item = Fence<'_>> {
    let mut carry = Carry::default();
    let mut open = None;
    let mut byte = 0;
    markdown.split_inclusive('\n').enumerate().filter_map(move |(index, raw)| {
        let start = byte;
        byte += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        let (kind, next) = classify(line, &carry, index == 0, math, COMMONMARK_TAB);
        carry = next;
        match kind {
            BlockKind::FenceOpen(info) => {
                open = (info.split_whitespace().next() == Some("mermaid")).then(|| {
                    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
                    (index, start, byte, &line[..indent])
                });
                None
            }
            BlockKind::FenceClose => {
                let (line, opener, body, indent) = open.take()?;
                Some(Fence {
                    lines: line..index + 1,
                    bytes: opener..byte,
                    indent,
                    body: &markdown[body..start],
                })
            }
            BlockKind::FenceBody { .. } => None,
            _ => {
                open = None;
                None
            }
        }
    })
}

fn prepare(fence: &Fence<'_>, sources: &mut Vec<String>) -> Result<usize, Failure> {
    if fence.body.len() > MAX_SOURCE_BYTES {
        return Err(Failure::TooLarge);
    }
    // A list fence's body is indented against the same container as its
    // opener. Remove the opener's indentation and normalize line endings
    // before comparing bodies or passing one to the renderer.
    let mut source = String::with_capacity(fence.body.len());
    for line in fence.body.lines() {
        let prefix = line.bytes().take(fence.indent.len())
            .take_while(|ch| matches!(ch, b' ' | b'\t')).count();
        source.push_str(&line[prefix..]);
        source.push('\n');
    }
    if let Some(job) = sources.iter().position(|known| known == &source) {
        return Ok(job);
    }
    // Every distinct command occupies at most one entry in the existing PNG
    // cache. Keeping the batch within it prevents evictions before collection.
    if sources.len() == MAX_CACHE_ENTRIES {
        return Err(Failure::TooLarge);
    }
    let job = sources.len();
    sources.push(source);
    Ok(job)
}

type Raster = Result<Arc<[u8]>, Failure>;

fn raster_batch(executable: &str, sources: &[String]) -> Vec<Option<Raster>> {
    let mut renderer = MermaidRenderer::default();
    let mut results = vec![None; sources.len()];
    let deadline = Instant::now() + BATCH_TIMEOUT;
    loop {
        renderer.poll();
        let mut pending = false;
        for (source, result) in sources.iter().zip(&mut results) {
            if result.is_some() {
                continue;
            }
            // The async worker enforces its own per-call timeout. Finish the
            // export when the overall budget expires even if more jobs remain.
            if Instant::now() >= deadline {
                return results;
            }
            match renderer.request_png(executable, source) {
                PicturePreview::Ready(png) => *result = Some(Ok(png)),
                PicturePreview::Failed(failure) => *result = Some(Err(failure)),
                PicturePreview::Pending => pending = true,
            }
        }
        if !pending {
            return results;
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

fn diagram_alt(source: &str) -> &'static str {
    let diagram = source.lines().map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("%%"))
        .and_then(|line| line.split_whitespace().next());
    match diagram {
        Some("graph" | "flowchart" | "flowchart-v2") => "Mermaid flowchart",
        Some("sequenceDiagram") => "Mermaid sequence diagram",
        Some("classDiagram" | "classDiagram-v2") => "Mermaid class diagram",
        Some("stateDiagram" | "stateDiagram-v2") => "Mermaid state diagram",
        Some("erDiagram") => "Mermaid entity relationship diagram",
        Some("gantt") => "Mermaid Gantt chart",
        Some("pie") => "Mermaid pie chart",
        _ => "Mermaid diagram",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_closed_fences_and_preserves_offsets() {
        let markdown = concat!(
            "---\r\n```mermaid\r\n---\r\n",
            "heading é\r\n\r\n",
            "````md\r\n```mermaid\r\n````\r\n",
            "> ```mermaid\r\n> graph LR\r\n> ```\r\n\r\n",
            "- item\r\n  ~~~mermaid details\r\n  graph LR\r\n  ~~~~\r\n",
            "\r\n```mermaid\r\nsequenceDiagram",
        );
        let found: Vec<_> = fences(markdown, true).collect();
        assert_eq!(found.len(), 1);
        let fence = &found[0];
        assert_eq!(fence.lines, 13..16);
        assert_eq!(&markdown[fence.bytes.clone()], "  ~~~mermaid details\r\n  graph LR\r\n  ~~~~\r\n");
        assert_eq!(fence.indent, "  ");
        let mut sources = Vec::new();
        assert_eq!(prepare(fence, &mut sources), Ok(0));
        assert_eq!(sources, ["graph LR\n"]);
    }

    #[test]
    fn normalizes_and_deduplicates_sources_with_a_unique_limit() {
        let markdown = "```mermaid\r\ngraph LR\r\n```\r\n\n~~~mermaid\ngraph LR\n~~~";
        let mut sources = Vec::new();
        let jobs: Vec<_> = fences(markdown, true).map(|fence| prepare(&fence, &mut sources)).collect();
        assert_eq!(jobs, [Ok(0), Ok(0)]);
        let mut sources = (0..MAX_CACHE_ENTRIES).map(|n| n.to_string()).collect();
        assert_eq!(prepare(&fences(markdown, true).next().unwrap(), &mut sources), Err(Failure::TooLarge));
    }

    #[test]
    fn reports_missing_executable_for_a_configured_renderer() {
        let source = "```mermaid\nflowchart LR\n```";
        let mut md = MarkdownConfig::default();
        assert!(render(source, &md).is_empty());
        let missing = std::env::temp_dir()
            .join(format!("shoin-merman-missing-{}", std::process::id()))
            .join("merman-cli");
        md.mermaid_renderer = Some(missing.to_string_lossy().into_owned());
        let results = render(source, &md);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].png, Err(Failure::MissingExecutable));
        assert_eq!(results[0].alt, "Mermaid flowchart");
        assert!(diagnostic(&results).unwrap().contains("Install merman-cli"));
    }
}
