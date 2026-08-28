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
    let text = read(path)?;
    let root = search_root(path, cfg);
    let mut stack = vec![canonical(path)];
    let mut out = String::new();
    // The composition file keeps its own front matter (§14.4); only embedded
    // files are stripped.
    expand(&text, path, &root, cfg, 0, &mut stack, &mut out)?;
    // Block ids go from the WHOLE document, not just from embedded text. They
    // are addresses rather than prose, and the composition's own are as much
    // noise in a compiled document as an embed's. Done once at the end so the
    // rule is stated in one place.
    Ok(link::strip_block_ids(&out))
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

#[allow(clippy::too_many_arguments)]
fn expand(
    text: &str,
    from: &Path,
    root: &Path,
    cfg: &TranscludeConfig,
    depth: u8,
    stack: &mut Vec<PathBuf>,
    out: &mut String,
) -> Result<()> {
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

        let target = match link::resolve(&link, from, root) {
            Ok(p) => p,
            Err(e) => {
                out.push_str(&placeholder(&link, &e.to_string()));
                out.push('\n');
                continue;
            }
        };

        let canon = canonical(&target);
        if let Some(at) = stack.iter().position(|p| *p == canon) {
            let loop_path: Vec<String> = stack[at..]
                .iter()
                .chain(std::iter::once(&canon))
                .map(|p| name_of(p))
                .collect();
            bail!("embedding loop: {}", loop_path.join(" -> "));
        }

        let body = match shape(&target, &link, cfg, depth) {
            Ok(b) => b,
            Err(e) => {
                out.push_str(&placeholder(&link, &e.to_string()));
                out.push('\n');
                continue;
            }
        };

        stack.push(canon);
        expand(&body, &target, root, cfg, depth + 1, stack, out)?;
        stack.pop();
    }
    Ok(())
}

/// Read a target and shape it for insertion: front matter off, the requested
/// section taken, headings demoted for the depth it lands at.
///
/// LIVE PREVIEW CALLS THIS TOO. That is the point of it being one function —
/// preview and export drifted apart once already (an embedded `# H1` showed
/// at level one on screen and came out demoted in the file), and a shared path
/// is the only thing that stops it happening again.
pub(super) fn shape(
    target: &Path,
    link: &Link,
    cfg: &TranscludeConfig,
    depth: u8,
) -> Result<String> {
    // An image is not text to slice into sections: it becomes a Markdown
    // image, which every export format already knows — `<img>` in HTML, and
    // the link itself in Markdown and plain text.
    if crate::image::looks_like_image(target) {
        return Ok(format!("![{}]({})\n", link.label(), target.display()));
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
    let root = search_root(from, cfg);
    let target = link::resolve(link, from, &root).map_err(|e| anyhow::anyhow!("{e}"))?;
    let body = shape(&target, link, cfg, 0)?;
    if !recurse {
        return Ok(link::strip_block_ids(&body));
    }
    let mut stack = vec![canonical(from), canonical(&target)];
    let mut out = String::new();
    expand(&body, &target, &root, cfg, 1, &mut stack, &mut out)?;
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

}
