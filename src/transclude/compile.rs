//! Flattening a composition into one document. SPEC.md §14.4.
//!
//! Walks `![[…]]` links recursively and returns one flat document — the
//! machinery under `:export`, and under `:embed rec`/`full`. Two rules shape it:
//!
//!   * **A cycle is a hard error, not a truncation.** Silently stopping at
//!     `max_depth` would produce a document that looks finished and is not.
//!     The error names the loop so it can be fixed.
//!   * **An unresolved link becomes a visible placeholder**, never an empty
//!     gap — the same reason. You should not be able to export a hole.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use super::link::{self, Link};
use super::Fences;
use crate::config::schema::TranscludeConfig;

/// One embed found in a line of source: where it sits, and what it asks for.
pub struct Found {
    /// Char range of the whole `![[…]]` construct within its line.
    pub range: std::ops::Range<usize>,
    pub link: Link,
}

/// Every `![[…]]` in a line, in order.
///
/// Deliberately its own scanner rather than a hook into `markdown::inline`:
/// compiling must work with no theme, no config and no render pass, and this is
/// four lines of the same logic.
pub fn embeds_in(line: &str) -> Vec<Found> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + 4 < chars.len() {
        if chars[i] == '!' && chars[i + 1] == '[' && chars[i + 2] == '[' {
            if let Some(end) = close_at(&chars, i + 3) {
                let body: String = chars[i + 3..end].iter().collect();
                if let Some(link) = Link::parse(&body) {
                    out.push(Found { range: i..end + 2, link });
                    i = end + 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// Index of the `]` opening the closing `]]`, searching from `from`.
fn close_at(chars: &[char], from: usize) -> Option<usize> {
    let mut j = from;
    while j + 1 < chars.len() {
        if chars[j] == ']' && chars[j + 1] == ']' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// A line that is nothing but one embed — the composition form from §14.1,
/// and the only shape that expands to a block rather than inline.
pub fn whole_line_embed(line: &str) -> Option<Link> {
    let t = line.trim();
    let found = embeds_in(t);
    match found.into_iter().next() {
        Some(f) if f.range.start == 0 && f.range.end == t.chars().count() => Some(f.link),
        _ => None,
    }
}

/// Flatten `path` into a single document.
pub fn compile(path: &Path, cfg: &TranscludeConfig) -> Result<String> {
    compile_with_sources(path, cfg).map(|(doc, _)| doc)
}

/// [`compile`], and the canonical paths of every file the document was built
/// from: `path` first, then each embedded note and picture once, in the
/// order they were first met. A target that failed to resolve or read (a
/// placeholder in the output) is not in the list.
pub fn compile_with_sources(path: &Path, cfg: &TranscludeConfig) -> Result<(String, Vec<PathBuf>)> {
    let text = read(path)?;
    let (doc, mut sources) = walk(&text, path, cfg)?;
    sources.insert(0, canonical(path));
    Ok((doc, sources))
}

/// [`compile`] for text that is not read from `path`, such as stdin. Links
/// resolve as though it were a file at `path`.
pub fn compile_text(text: &str, path: &Path, cfg: &TranscludeConfig) -> Result<String> {
    walk(text, path, cfg).map(|(doc, _)| doc)
}

/// The compile itself: the flat document and the embedded files it read.
fn walk(text: &str, path: &Path, cfg: &TranscludeConfig) -> Result<(String, Vec<PathBuf>)> {
    let mut w = Walk::new(path, cfg);
    w.stack.push(canonical(path));
    let mut out = String::new();
    // The composition file keeps its own front matter (§14.4); only embedded
    // files are stripped.
    w.expand(text, path, 0, &mut out)?;
    // Block ids go from the WHOLE document, not just from embedded text. They
    // are addresses rather than prose, and the composition's own are as much
    // noise in a compiled document as an embed's. Done once at the end so the
    // rule is stated in one place.
    Ok((link::strip_block_ids(&out), w.sources))
}

/// The directory bare-name resolution searches, from `transclude.root`
/// interpreted relative to the composition file.
pub fn search_root(from: &Path, cfg: &TranscludeConfig) -> PathBuf {
    let dir = from.parent().unwrap_or(Path::new("."));
    let r = Path::new(&cfg.root);
    if r.is_absolute() {
        r.to_path_buf()
    } else {
        dir.join(r)
    }
}

fn read(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
    String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("{} is not valid UTF-8", path.display()))
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The folder relative picture paths are written from: the top-level
/// composition's, canonical when it exists.
fn base_of(path: &Path) -> PathBuf {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    canonical(dir)
}

/// State for one walk over a composition.
struct Walk<'a> {
    cfg: &'a TranscludeConfig,
    /// Where bare names are searched for.
    root: PathBuf,
    /// The top-level composition's folder; see [`picture_dest`].
    base: PathBuf,
    /// The chain of files being expanded, for loop detection.
    stack: Vec<PathBuf>,
    /// Every embedded file read, canonical, each once.
    sources: Vec<PathBuf>,
}

impl<'a> Walk<'a> {
    fn new(top: &Path, cfg: &'a TranscludeConfig) -> Self {
        Walk {
            cfg,
            root: search_root(top, cfg),
            base: base_of(top),
            stack: Vec::new(),
            sources: Vec::new(),
        }
    }

    fn expand(&mut self, text: &str, from: &Path, depth: u8, out: &mut String) -> Result<()> {
        let cfg = self.cfg;
        let mut fences = Fences::default();
        for line in text.lines() {
            // A fence is literal. A `![[…]]` inside one is the syntax being
            // DOCUMENTED, not an embed being asked for, and expanding it destroys
            // the very line that explains the feature. `demote` and
            // `strip_block_ids` have always skipped fences; this is the third
            // pass over the same text and it follows the same rule.
            if fences.literal(line) {
                out.push_str(line);
                out.push('\n');
                continue;
            }
            let Some(link) = whole_line_embed(line) else {
                out.push_str(line);
                out.push('\n');
                continue;
            };

            // Depth is checked BEFORE resolving, so an over-deep document reports
            // the cap rather than a file-not-found from somewhere unexpected.
            if depth >= cfg.max_depth {
                bail!(
                    "embedding stopped at depth {} ({}): raise transclude.max_depth",
                    cfg.max_depth,
                    link.label()
                );
            }

            let target = match link::resolve(&link, from, &self.root) {
                Ok(p) => p,
                Err(e) => {
                    out.push_str(&placeholder(&link, &e.to_string()));
                    out.push('\n');
                    continue;
                }
            };

            let canon = canonical(&target);
            if let Some(at) = self.stack.iter().position(|p| *p == canon) {
                let loop_path: Vec<String> = self.stack[at..]
                    .iter()
                    .chain(std::iter::once(&canon))
                    .map(|p| name_of(p))
                    .collect();
                bail!("embedding loop: {}", loop_path.join(" -> "));
            }

            let body = match shape(&target, &link, cfg, depth, &self.base) {
                Ok(b) => b,
                Err(e) => {
                    out.push_str(&placeholder(&link, &e.to_string()));
                    out.push('\n');
                    continue;
                }
            };

            if !self.sources.contains(&canon) {
                self.sources.push(canon.clone());
            }
            self.stack.push(canon);
            self.expand(&body, &target, depth + 1, out)?;
            self.stack.pop();
        }
        Ok(())
    }
}

/// Read a target and shape it for insertion: front matter off, the requested
/// section taken, headings demoted for the depth it lands at.
///
/// LIVE PREVIEW CALLS THIS TOO. That is the point of it being one function —
/// preview and export drifted apart once already (an embedded `# H1` showed
/// at level one on screen and came out demoted in the file), and a shared path
/// is the only thing that stops it happening again.
///
/// `base` is the top-level composition's folder, from [`base_of`]: a
/// picture's path is written relative to it.
pub(super) fn shape(
    target: &Path,
    link: &Link,
    cfg: &TranscludeConfig,
    depth: u8,
    base: &Path,
) -> Result<String> {
    // An image is not text to slice into sections: it becomes a Markdown
    // image, which every export format already knows — `<img>` in HTML, and
    // the link itself in Markdown and plain text.
    if crate::image::looks_like_image(target) {
        return Ok(format!("![{}]({})\n", link.label(), picture_dest(target, base)));
    }
    let text = read(target)?;
    let text = if cfg.strip_frontmatter {
        link::strip_front_matter(&text).to_string()
    } else {
        text
    };
    let body = link::extract(&text, &link.section)?;
    Ok(demote(&body, cfg.heading_offset.saturating_mul(depth + 1)))
}

/// An embedded picture's destination in the flattened document: relative to
/// `base` (the top-level composition's folder) when the picture is inside it,
/// absolute otherwise, so the same document reads the same wherever the
/// compile was started from. A space or a parenthesis would end the
/// destination early, so such a path is written in `<…>`.
fn picture_dest(target: &Path, base: &Path) -> String {
    let abs = canonical(target);
    let path = match abs.strip_prefix(base) {
        Ok(rel) if !rel.as_os_str().is_empty() => rel.to_path_buf(),
        _ => abs,
    };
    let s = path.to_string_lossy().into_owned();
    if s.contains([' ', '(', ')']) {
        format!("<{s}>")
    } else {
        s
    }
}

/// Flatten ONE link, for live preview.
///
/// `recurse` is the difference between the `long`/`full` modes and `short`:
/// with it off, a nested `![[…]]` inside the target stays as its own text on
/// screen; with it on, the result is what `:export` would write for that link.
pub fn flatten_link(
    link: &Link,
    from: &Path,
    cfg: &TranscludeConfig,
    recurse: bool,
) -> Result<String> {
    let mut w = Walk::new(from, cfg);
    let target = link::resolve(link, from, &w.root).map_err(|e| anyhow::anyhow!("{e}"))?;
    let body = shape(&target, link, cfg, 0, &w.base)?;
    if !recurse {
        return Ok(link::strip_block_ids(&body));
    }
    w.stack = vec![canonical(from), canonical(&target)];
    let mut out = String::new();
    w.expand(&body, &target, 1, &mut out)?;
    Ok(link::strip_block_ids(&out))
}

/// What an unresolved embed leaves in the output. Loud on purpose: SPEC §14.2
/// forbids a silent gap, and this is what stops a hole reaching a publisher.
fn placeholder(link: &Link, why: &str) -> String {
    format!("> **Missing embed** `![[{}]]` — {why}", link.label())
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Demote every heading by `by` levels so the flattened outline stays coherent
/// (§14.4). Headings already at level 6 stay there — Markdown has no `#######`,
/// and dropping the heading entirely would lose the text.
///
/// Fenced code is skipped: a `#` at the start of a line inside a shell snippet
/// is a comment, not a heading.
pub(super) fn demote(text: &str, by: u8) -> String {
    if by == 0 {
        return text.to_string();
    }
    let mut out = Vec::new();
    let mut fences = super::Fences::default();
    for line in text.lines() {
        if fences.literal(line) {
            out.push(line.to_string());
            continue;
        }
        let t = line.trim_start();
        let hashes = t.chars().take_while(|c| *c == '#').count();
        let is_heading = (1..=6).contains(&hashes)
            && t[hashes..].starts_with(' ')
            && line.len() - line.trim_start().len() < 4;
        if is_heading {
            let want = (hashes + by as usize).min(6);
            let indent = &line[..line.len() - t.len()];
            out.push(format!("{indent}{}{}", "#".repeat(want), &t[hashes..]));
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transclude::link::Section;

    fn cfg() -> TranscludeConfig {
        TranscludeConfig::default()
    }

    fn vault(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shoin-tc-{tag}-{t}-{n}"));
        std::fs::create_dir_all(d.join("fragments")).unwrap();
        d
    }

    /// Text that was never a file compiles exactly as the same text in a file
    /// at that path: embeds resolve from where it is said to be.
    #[test]
    fn text_compiles_as_if_it_were_the_file() {
        let d = vault("text");
        write(&d, "fragments/a.md", "Alpha body.\n");
        let body = "# Doc\n\n![[fragments/a]]\n";
        let main = write(&d, "main.md", body);
        let from_file = compile(&main, &cfg()).unwrap();
        let from_text = compile_text(body, &d.join("stdin.md"), &cfg()).unwrap();
        assert_eq!(from_text, from_file);
        assert!(from_text.contains("Alpha body."), "{from_text}");
    }

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn finds_embeds_and_tells_them_from_plain_links() {
        let f = embeds_in("see ![[one]] and ![[two#Bit]] but not [[three]]");
        assert_eq!(f.len(), 2, "a plain [[link]] is not an embed");
        assert_eq!(f[0].link.target, "one");
        assert_eq!(f[1].link.section, Section::Heading("Bit".into()));

        // The range covers the whole construct, `!` included.
        let f = embeds_in("![[a]]");
        assert_eq!(f[0].range, 0..6);

        assert!(embeds_in("![alt](img.png)").is_empty(), "an image is not an embed");
        assert!(embeds_in("![[unclosed").is_empty());
    }

    #[test]
    fn only_a_line_that_is_nothing_but_an_embed_expands() {
        assert!(whole_line_embed("![[note]]").is_some());
        assert!(whole_line_embed("   ![[note]]  ").is_some(), "indentation is fine");
        assert!(whole_line_embed("text ![[note]]").is_none());
        assert!(whole_line_embed("![[a]] ![[b]]").is_none());
    }

    #[test]
    fn compiles_a_composition_in_order() {
        let d = vault("basic");
        write(&d, "fragments/one.md", "First fragment.\n");
        write(&d, "fragments/two.md", "Second fragment.\n");
        let comp = write(
            &d,
            "note.md",
            "# On attention\n\n![[fragments/one]]\n\n![[fragments/two]]\n",
        );

        let out = compile(&comp, &cfg()).unwrap();
        assert!(out.contains("# On attention"));
        let a = out.find("First fragment").unwrap();
        let b = out.find("Second fragment").unwrap();
        assert!(a < b, "fragments keep the order the composition gave them");
        assert!(!out.contains("![["), "no embed survives into the output");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn embedded_headings_are_demoted_so_the_outline_holds() {
        let d = vault("demote");
        write(&d, "frag.md", "# Fragment title\n\nbody\n\n## Sub\n");
        let comp = write(&d, "note.md", "# Top\n\n![[frag]]\n");

        let out = compile(&comp, &cfg()).unwrap();
        assert!(out.contains("# Top"));
        assert!(out.contains("## Fragment title"), "H1 -> H2 under an H1");
        assert!(out.contains("### Sub"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_hash_inside_fenced_code_is_not_a_heading() {
        let d = vault("fence");
        write(&d, "frag.md", "# Title\n\n```sh\n# not a heading\n```\n");
        let comp = write(&d, "note.md", "![[frag]]\n");

        let out = compile(&comp, &cfg()).unwrap();
        assert!(out.contains("## Title"), "the real heading moved");
        assert!(out.contains("# not a heading"), "the shell comment did not");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_cycle_is_a_named_error_not_a_truncation() {
        let d = vault("cycle");
        write(&d, "a.md", "![[b]]\n");
        write(&d, "b.md", "![[a]]\n");
        let comp = d.join("a.md");

        let err = compile(&comp, &cfg()).unwrap_err().to_string();
        assert!(err.contains("loop"), "got: {err}");
        assert!(err.contains("a.md") && err.contains("b.md"), "names the loop: {err}");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn depth_is_capped_and_says_so() {
        let d = vault("depth");
        // A chain longer than the cap, with no cycle in it.
        for i in 0..6 {
            write(&d, &format!("n{i}.md"), &format!("![[n{}]]\n", i + 1));
        }
        write(&d, "n6.md", "bottom\n");
        let mut c = cfg();
        c.max_depth = 3;

        let err = compile(&d.join("n0.md"), &c).unwrap_err().to_string();
        assert!(err.contains("depth 3"), "got: {err}");
        assert!(err.contains("max_depth"), "…and says how to fix it");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_missing_target_leaves_a_visible_placeholder() {
        let d = vault("missing");
        let comp = write(&d, "note.md", "before\n\n![[nope]]\n\nafter\n");

        let out = compile(&comp, &cfg()).unwrap();
        assert!(out.contains("Missing embed"), "never a silent gap");
        assert!(out.contains("nope"));
        assert!(out.contains("before") && out.contains("after"));
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn front_matter_of_embeds_goes_and_the_composition_keeps_its_own() {
        let d = vault("fm");
        write(&d, "frag.md", "---\ntitle: frag\n---\nfragment body\n");
        let comp = write(&d, "note.md", "---\ntitle: note\n---\n![[frag]]\n");

        let out = compile(&comp, &cfg()).unwrap();
        assert!(out.contains("title: note"), "the composition keeps its own");
        assert!(!out.contains("title: frag"), "the embed's is stripped");
        assert!(out.contains("fragment body"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// F08: an embedded note saved with Windows line endings loses its front
    /// matter like any other.
    #[test]
    fn front_matter_of_a_crlf_embed_goes() {
        let d = vault("fmcrlf");
        write(&d, "frag.md", "---\r\ntitle: frag\r\n---\r\nfragment body\r\n");
        let comp = write(&d, "note.md", "![[frag]]\n");

        let out = compile(&comp, &cfg()).unwrap();
        assert!(!out.contains("title:"), "{out:?}");
        assert!(out.contains("fragment body"), "{out:?}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// F10: an embedded picture's path is relative to the composition's
    /// folder when it is inside it, wherever the compile was started from,
    /// and absolute otherwise. A space or a parenthesis puts the destination
    /// in `<…>` so the image syntax holds.
    #[test]
    fn embedded_picture_paths_are_relative_to_the_composition() {
        let d = vault("pics");
        let outside = vault("pics-out");
        write(&d, "notes/img/p.png", "x");
        write(&d, "notes/my pic (1).png", "x");
        write(&outside, "far.png", "x");
        let far = outside.join("far.png");
        let main = write(
            &d,
            "notes/a.md",
            &format!("![[img/p.png]]\n\n![[my pic (1).png]]\n\n![[{}]]\n", far.display()),
        );
        // Given as a path with `..` and `.` in it, as a relative path from
        // the command line would be after joining.
        let given = d.join("notes/../notes/./a.md");
        let out = compile(&given, &cfg()).unwrap();
        assert!(out.contains("![img/p.png](img/p.png)"), "{out}");
        assert!(out.contains("![my pic (1).png](<my pic (1).png>)"), "{out}");
        let far = std::fs::canonicalize(&far).unwrap();
        assert!(out.contains(&format!("]({})", far.display())), "{out}");
        assert_eq!(compile(&main, &cfg()).unwrap(), out);
        std::fs::remove_dir_all(&d).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// A picture embedded by a nested note is still written relative to the
    /// TOP-LEVEL composition, not to the note that embeds it.
    #[test]
    fn a_nested_picture_is_relative_to_the_top_level_note() {
        let d = vault("nested-pic");
        write(&d, "fragments/pics/chart.png", "x");
        write(&d, "fragments/part.md", "Part.\n\n![[pics/chart.png]]\n");
        let main = write(&d, "main.md", "![[fragments/part]]\n");
        let out = compile(&main, &cfg()).unwrap();
        assert!(out.contains("](fragments/pics/chart.png)"), "{out}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// `compile_with_sources` names every file the document came from, once
    /// each and canonical, and leaves out what failed to resolve.
    #[test]
    fn compiling_lists_the_files_it_read() {
        let d = vault("sources");
        write(&d, "fragments/a.md", "A.\n\n![[b]]\n\n![[pic.png]]\n");
        write(&d, "fragments/b.md", "B.\n");
        write(&d, "fragments/pic.png", "x");
        let main = write(&d, "main.md", "![[fragments/a]]\n\n![[b]]\n\n![[nope]]\n");
        let given = d.join("fragments/../main.md");
        let (doc, sources) = compile_with_sources(&given, &cfg()).unwrap();
        assert_eq!(doc, compile(&main, &cfg()).unwrap());
        let c = |rel: &str| std::fs::canonicalize(d.join(rel)).unwrap();
        assert_eq!(
            sources,
            vec![c("main.md"), c("fragments/a.md"), c("fragments/b.md"), c("fragments/pic.png")]
        );
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_section_embed_takes_only_that_section() {
        let d = vault("section");
        write(&d, "frag.md", "# A\naaa\n\n# B\nbbb\n");
        let comp = write(&d, "note.md", "![[frag#B]]\n");

        let out = compile(&comp, &cfg()).unwrap();
        assert!(out.contains("bbb"));
        assert!(!out.contains("aaa"), "the other section stays out");
        std::fs::remove_dir_all(&d).ok();
    }

    /// Block ids are addresses, not prose. They go from the whole compiled
    /// document — the composition's own as well as its embeds' — because a
    /// renderer that does not know Obsidian shows `^id` as literal text.
    #[test]
    fn compiling_strips_block_ids_from_everywhere() {
        let d = vault("ids");
        write(&d, "frag.md", "embedded para ^fromfrag\n");
        let comp = write(
            &d,
            "note.md",
            "own para ^fromnote\n\n![[frag]]\n\n```sh\necho x ^notanid\n```\n",
        );

        let out = compile(&comp, &cfg()).unwrap();
        assert!(out.contains("own para"), "prose survives");
        assert!(out.contains("embedded para"));
        assert!(!out.contains("^fromnote"), "the composition's own id goes");
        assert!(!out.contains("^fromfrag"), "…and so does the embed's");
        assert!(out.contains("^notanid"), "but fenced code is untouched");
        std::fs::remove_dir_all(&d).ok();
    }


    /// A fence is literal. Documenting the embed syntax inside a code block
    /// must not embed the file — the same rule `demote` and `strip_block_ids`
    /// already follow.
    #[test]
    fn a_fenced_embed_is_literal() {
        let d = vault("fence");
        write(&d, "frag.md", "SECRET BODY\n");
        let comp = write(
            &d,
            "comp.md",
            "How to embed:\n\n```markdown\n![[frag]]\n```\n\n![[frag]]\n",
        );
        let out = compile(&comp, &cfg()).unwrap();
        assert!(
            out.contains("![[frag]]"),
            "the fenced example survives verbatim: {out:?}"
        );
        assert_eq!(
            out.matches("SECRET BODY").count(),
            1,
            "only the real embed expanded: {out:?}"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// F06 and F07 through the whole walk: a heading embed keeps its code
    /// block closed, a dated name resolves, and folders and extensions in a
    /// link are not dropped.
    #[test]
    fn embeds_keep_fences_and_resolve_by_full_name() {
        let d = vault("f06f07");
        write(&d, "setup.md", "## Setup\n\n```sh\n# install it\nmake install\n```\n\nAfter the code.\n\n## Next\nnot embedded\n");
        write(&d, "2024.01.15 meeting.md", "We met.\n");
        write(&d, "sub/diagram.md", "A note.\n");
        write(&d, "sub/todo.md", "A list.\n");
        let main = write(
            &d,
            "main.md",
            "![[setup#Setup]]\n\nTail text.\n\n![[2024.01.15 meeting]]\n\n![[diagram.png]]\n\n![[archive/todo]]\n",
        );
        let out = compile(&main, &cfg()).unwrap();
        assert_eq!(out.matches("```").count(), 2, "the fence is closed: {out}");
        assert!(out.contains("After the code.") && !out.contains("not embedded"), "{out}");
        assert!(out.contains("We met."), "{out}");
        assert!(!out.contains("A note."), "diagram.png is not diagram.md: {out}");
        assert!(!out.contains("A list."), "archive/todo is not sub/todo: {out}");
        std::fs::remove_dir_all(&d).ok();
    }
}
