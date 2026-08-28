//! Parsing and resolving `![[…]]` embeds. SPEC.md §14.2.
//!
//! This module knows nothing about rendering or compiling — it turns the text
//! between the brackets into a path on disk and the slice of that file the
//! embed asked for. Everything above it (the export walk, and live preview)
//! is built on these two answers.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

/// Which part of the target a link asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Section {
    /// The whole file.
    All,
    /// `#heading` — that heading and everything under it, up to the next
    /// heading of the same or higher level.
    Heading(String),
    /// `#^blockid` — the one block tagged with that id.
    Block(String),
}

/// A parsed `![[target#section]]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: String,
    pub section: Section,
}

impl Link {
    /// Parse the text BETWEEN the brackets — `note`, `note#heading`,
    /// `note#^blockid`, optionally with an Obsidian `|alias` suffix.
    ///
    /// The alias is dropped rather than rejected. SPEC §14.2 asks for Obsidian
    /// syntax so vaults stay portable, and a vault full of `![[note|Title]]`
    /// should not turn into a document full of error placeholders.
    pub fn parse(body: &str) -> Option<Link> {
        let body = body.split('|').next().unwrap_or(body).trim();
        if body.is_empty() {
            return None;
        }
        let (target, section) = match body.split_once('#') {
            None => (body, Section::All),
            Some((t, s)) => {
                let s = s.trim();
                if s.is_empty() {
                    (t, Section::All)
                } else if let Some(id) = s.strip_prefix('^') {
                    (t, Section::Block(id.trim().to_string()))
                } else {
                    (t, Section::Heading(s.to_string()))
                }
            }
        };
        let target = target.trim();
        if target.is_empty() {
            return None;
        }
        Some(Link { target: target.to_string(), section })
    }

    /// The name to show in an embed's border, and in error messages.
    pub fn label(&self) -> String {
        match &self.section {
            Section::All => self.target.clone(),
            Section::Heading(h) => format!("{}#{h}", self.target),
            Section::Block(b) => format!("{}#^{b}", self.target),
        }
    }
}

/// Why a link could not be turned into a file.
///
/// Named cases rather than one string because §14.2 requires an error
/// PLACEHOLDER in the document — never a silent empty — and the placeholder
/// reads better when it can say which kind of wrong this is.
#[derive(Debug)]
pub enum Unresolved {
    Missing(String),
    /// More than one file under the search root has this name; naming them is
    /// the only way the reader can fix it.
    Ambiguous(String, Vec<PathBuf>),
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unresolved::Missing(t) => write!(f, "no note called {t:?}"),
            Unresolved::Ambiguous(t, found) => {
                let names: Vec<String> = found
                    .iter()
                    .take(3)
                    .map(|p| p.display().to_string())
                    .collect();
                write!(f, "{t:?} is ambiguous — {}", names.join(", "))
            }
        }
    }
}

/// Files whose TEXT can be embedded — pasted into the document as prose.
const EMBEDDABLE: &[&str] = &["md", "markdown", "txt"];

/// Everything a `![[…]]` may point at. An image is embeddable too, it just
/// expands into a picture rather than into text (`crate::image`), so it belongs
/// in resolution but not in `EMBEDDABLE`.
///
/// Text extensions come FIRST: a bare `![[diagram]]` beside both `diagram.md`
/// and `diagram.png` means the note, because that is the file you can edit.
fn linkable() -> Vec<String> {
    EMBEDDABLE
        .iter()
        .chain(crate::image::IMAGE_EXTENSIONS.iter())
        .map(|e| (*e).to_string())
        .collect()
}

/// Turn a link's target into a path on disk.
///
/// Order per SPEC §14.2: relative to the composition file first, then by unique
/// filename anywhere under `root`. The relative try comes first so a vault can
/// have several `index.md` and each folder's own one still wins locally.
pub fn resolve(link: &Link, from: &Path, root: &Path) -> Result<PathBuf, Unresolved> {
    let dir = from.parent().unwrap_or(Path::new("."));
    for cand in candidates(&link.target) {
        let p = dir.join(&cand);
        if p.is_file() {
            return Ok(p);
        }
    }
    // Bare-name search. Matches on the file NAME, with or without its
    // extension, so `![[attention]]` finds `fragments/attention.md`.
    let wanted = link.target.trim_end_matches('/');
    let stem = Path::new(wanted)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase());
    let mut found = Vec::new();
    // Hoisted: this is the predicate for EVERY file under the root, and
    // rebuilding the extension list inside it allocated eight Strings per file
    // in the vault.
    let exts = linkable();
    walk(root, 0, &mut |p| {
        let is_embeddable = p
            .extension()
            .map(|e| exts.contains(&e.to_string_lossy().to_lowercase()))
            .unwrap_or(false);
        if !is_embeddable {
            return;
        }
        let s = p.file_stem().map(|s| s.to_string_lossy().to_lowercase());
        if s == stem {
            found.push(p.to_path_buf());
        }
    });
    match found.len() {
        0 => Err(Unresolved::Missing(link.target.clone())),
        1 => Ok(found.remove(0)),
        _ => {
            found.sort();
            Err(Unresolved::Ambiguous(link.target.clone(), found))
        }
    }
}

/// The filenames a target could mean: as written, and with each embeddable
/// extension appended when it has none.
fn candidates(target: &str) -> Vec<PathBuf> {
    let p = Path::new(target);
    if p.extension().is_some() {
        return vec![p.to_path_buf()];
    }
    linkable()
        .iter()
        .map(|e| PathBuf::from(format!("{target}.{e}")))
        .collect()
}

/// Depth-capped directory walk, skipping dotfiles and build output — the same
/// exclusions the fuzzy finder uses, for the same reason.
fn walk(dir: &Path, depth: usize, f: &mut impl FnMut(&Path)) {
    if depth > 16 {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            walk(&path, depth + 1, f);
        } else {
            f(&path);
        }
    }
}

/// The slice of `text` a link asks for, with front matter already removed by
/// the caller.
pub fn extract(text: &str, section: &Section) -> Result<String> {
    match section {
        Section::All => Ok(text.to_string()),
        Section::Heading(want) => heading_section(text, want),
        Section::Block(id) => block_with_id(text, id),
    }
}

/// A heading and everything under it, to the next heading of the same or higher
/// level. Matched case-insensitively on the trimmed title, as Obsidian does.
fn heading_section(text: &str, want: &str) -> Result<String> {
    let want_lc = want.trim().to_lowercase();
    let mut out: Vec<&str> = Vec::new();
    let mut level = 0usize;

    for line in text.lines() {
        if let Some((l, title)) = heading_of(line) {
            if level == 0 {
                if title.trim().to_lowercase() == want_lc {
                    level = l;
                    out.push(line);
                }
                continue;
            }
            // A sibling or a shallower heading closes the section.
            if l <= level {
                break;
            }
        }
        if level > 0 {
            out.push(line);
        }
    }
    if level == 0 {
        bail!("no heading {want:?}");
    }
    Ok(out.join("\n"))
}

/// `## Title` -> (2, "Title").
fn heading_of(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &t[hashes..];
    if !rest.starts_with(' ') && !rest.is_empty() {
        return None;
    }
    Some((hashes, rest.trim()))
}

/// The block tagged `^id`. A block is a run of non-blank lines; the id sits at
/// the end of its last line and is stripped from the result, since it is
/// plumbing rather than prose.
fn block_with_id(text: &str, id: &str) -> Result<String> {
    let marker = format!("^{id}");
    let lines: Vec<&str> = text.lines().collect();
    let Some(end) = lines.iter().position(|l| l.trim_end().ends_with(&marker)) else {
        bail!("no block ^{id}");
    };
    let mut start = end;
    while start > 0 && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    let mut out: Vec<String> = lines[start..=end].iter().map(|s| s.to_string()).collect();
    if let Some(last) = out.last_mut() {
        *last = last.trim_end().trim_end_matches(&marker).trim_end().to_string();
    }
    Ok(out.join("\n"))
}

/// Remove Obsidian block ids (`^abc123` at the end of a line).
///
/// A block id is an ADDRESS, not prose — the anchor another note points at with
/// `![[note#^abc123]]`. It is the same class of thing as front matter, and
/// `:export` strips it for the same reason: the exported document is meant to
/// be read or piped to pandoc, and every renderer that does not know Obsidian's
/// convention shows `^abc123` as literal text in the middle of a paragraph.
///
/// Three things keep this from eating real text:
///   * the id must be the LAST thing on its line;
///   * it must be preceded by whitespace, so `2^10` survives;
///   * fenced code is skipped entirely, where a trailing `^foo` may be syntax.
pub fn strip_block_ids(text: &str) -> String {
    let mut out: Vec<String> = Vec::with_capacity(text.lines().count());
    let mut fences = super::Fences::default();
    for line in text.lines() {
        if fences.literal(line) {
            out.push(line.to_string());
        } else {
            out.push(without_block_id(line));
        }
    }
    let mut joined = out.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// One line with its trailing `^id` removed, if it has one.
fn without_block_id(line: &str) -> String {
    let trimmed = line.trim_end();
    let Some(caret) = trimmed.rfind('^') else {
        return line.to_string();
    };
    let id = &trimmed[caret + 1..];
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return line.to_string();
    }
    // `2^10` is arithmetic, not an anchor. A caret opening the line is one too.
    let before = &trimmed[..caret];
    if !before.ends_with(char::is_whitespace) {
        return line.to_string();
    }
    let kept = before.trim_end();
    // A line that was ONLY an id becomes empty rather than vanishing, so the
    // paragraph around it keeps its shape.
    kept.to_string()
}

/// Strip a leading `---` front-matter block. Returns the body unchanged when
/// there is none.
pub fn strip_front_matter(text: &str) -> &str {
    let rest = match text.strip_prefix("---\n") {
        Some(r) => r,
        None => return text,
    };
    // The closing fence must be its own line.
    let mut idx = 0usize;
    for line in rest.split_inclusive('\n') {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            return rest[idx + line.len()..].trim_start_matches('\n');
        }
        idx += line.len();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(s: &str) -> Link {
        Link::parse(s).expect("parses")
    }

    #[test]
    fn parses_every_form_in_the_spec() {
        assert_eq!(link("note"), Link { target: "note".into(), section: Section::All });
        assert_eq!(
            link("note#Heading"),
            Link { target: "note".into(), section: Section::Heading("Heading".into()) }
        );
        assert_eq!(
            link("note#^abc123"),
            Link { target: "note".into(), section: Section::Block("abc123".into()) }
        );
        // Paths, spaces, and an Obsidian alias that we drop rather than choke on.
        assert_eq!(link("fragments/one two").target, "fragments/one two");
        assert_eq!(link("note|Nice Title").target, "note");
        assert_eq!(link("note|Nice").section, Section::All);

        assert_eq!(Link::parse(""), None);
        assert_eq!(Link::parse("   "), None);
        assert_eq!(Link::parse("#heading"), None, "a section with no file is not a link");
        // A trailing `#` is a whole-file embed, not an empty heading.
        assert_eq!(link("note#").section, Section::All);
    }

    #[test]
    fn label_reads_back_as_written() {
        assert_eq!(link("a").label(), "a");
        assert_eq!(link("a#B").label(), "a#B");
        assert_eq!(link("a#^b").label(), "a#^b");
    }

    #[test]
    fn a_heading_section_stops_at_its_first_sibling() {
        let doc = "\
# One
intro

## Two
body of two

### Deeper
still two

## Three
body of three
";
        let got = extract(doc, &Section::Heading("Two".into())).unwrap();
        assert!(got.starts_with("## Two"));
        assert!(got.contains("### Deeper"), "a deeper heading stays inside");
        assert!(got.contains("still two"));
        assert!(!got.contains("Three"), "a sibling heading ends the section");

        // Case-insensitive, as Obsidian is.
        assert!(extract(doc, &Section::Heading("two".into())).is_ok());
        // The top heading takes everything below it.
        let all = extract(doc, &Section::Heading("One".into())).unwrap();
        assert!(all.contains("Three"));

        assert!(extract(doc, &Section::Heading("Nope".into())).is_err());
    }

    #[test]
    fn a_block_id_takes_its_paragraph_and_drops_the_marker() {
        let doc = "\
first para

second para
runs two lines ^keep

third para
";
        let got = extract(doc, &Section::Block("keep".into())).unwrap();
        assert_eq!(got, "second para\nruns two lines");
        assert!(extract(doc, &Section::Block("nope".into())).is_err());
    }

    fn vault(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shoin-res-{tag}-{t}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn put(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    /// SPEC §14.2: relative to the composition file FIRST, then by unique name.
    /// The order is what lets a vault have several `index.md` and each folder's
    /// own one still win locally.
    #[test]
    fn resolution_prefers_the_neighbour_over_the_search() {
        let d = vault("order");
        put(&d, "here/index.md", "local\n");
        put(&d, "far/index.md", "distant\n");
        let from = d.join("here/note.md");

        let got = resolve(&link("index"), &from, &d).unwrap();
        assert_eq!(std::fs::read_to_string(got).unwrap(), "local\n");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A bare name found once anywhere under the root resolves; found twice it
    /// is an error that NAMES the candidates, because that is the only way to
    /// fix it.
    #[test]
    fn a_bare_name_resolves_once_and_complains_twice() {
        let d = vault("bare");
        put(&d, "a/only.md", "x\n");
        let from = d.join("note.md");
        assert!(resolve(&link("only"), &from, &d).is_ok());

        put(&d, "b/only.md", "y\n");
        match resolve(&link("only"), &from, &d) {
            Err(Unresolved::Ambiguous(t, found)) => {
                assert_eq!(t, "only");
                assert_eq!(found.len(), 2);
                let msg = Unresolved::Ambiguous(t, found).to_string();
                assert!(msg.contains("ambiguous") && msg.contains("only.md"), "{msg}");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }

        match resolve(&link("nothing"), &from, &d) {
            Err(Unresolved::Missing(t)) => assert_eq!(t, "nothing"),
            other => panic!("expected Missing, got {other:?}"),
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// The extension is optional, and an image resolves like a note — it is
    /// what happens NEXT that differs (a picture, not pasted text). A bare name
    /// prefers the note, because that is the file you can edit.
    #[test]
    fn only_text_files_are_embeddable() {
        let d = vault("ext");
        put(&d, "notes.md", "m\n");
        put(&d, "plain.txt", "t\n");
        std::fs::write(d.join("pic.png"), [0u8, 1, 2]).unwrap();
        let from = d.join("comp.md");

        assert!(resolve(&link("notes"), &from, &d).is_ok(), "extension optional");
        assert!(resolve(&link("notes.md"), &from, &d).is_ok(), "…or explicit");
        assert!(resolve(&link("plain"), &from, &d).is_ok(), ".txt counts");
        assert!(resolve(&link("pic"), &from, &d).is_ok(), "an image resolves too");
        assert_eq!(
            resolve(&link("pic"), &from, &d).unwrap().extension().unwrap(),
            "png"
        );

        // Both a note and a picture by that name: the note wins.
        put(&d, "both.md", "note\n");
        std::fs::write(d.join("both.png"), [0u8, 1, 2]).unwrap();
        assert_eq!(
            resolve(&link("both"), &from, &d).unwrap().extension().unwrap(),
            "md",
            "a bare name prefers the file you can edit"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// A block id is an address, not prose: it goes on compile, the way front
    /// matter does. The care is all in NOT eating text that merely has a caret.
    #[test]
    fn block_ids_go_but_carets_that_are_not_ids_stay() {
        let doc = "\
para one ^abc123

para two
last line ^keep-me

2^10 is a thousand
a caret ^ on its own
mid ^line text
^leading

```sh
echo done ^notanid
```
";
        let got = strip_block_ids(doc);
        assert!(!got.contains("^abc123"), "an id at the end of a line goes");
        assert!(!got.contains("^keep-me"), "…hyphens included");
        assert!(got.contains("para one"), "…and the prose stays");
        assert!(got.contains("last line"));

        assert!(got.contains("2^10"), "arithmetic is not an anchor");
        assert!(got.contains("a caret ^ on its own"), "a bare caret is not an id");
        assert!(got.contains("mid ^line text"), "an id must end the line");
        assert!(got.contains("^leading"), "…and must follow whitespace");
        assert!(got.contains("echo done ^notanid"), "fenced code is left alone");
    }

    /// The document's shape survives: no line disappears, so paragraphs and
    /// blank-line separation are unchanged.
    #[test]
    fn stripping_ids_keeps_the_line_count() {
        let doc = "a ^one\n\nb\nc ^two\n";
        let got = strip_block_ids(doc);
        assert_eq!(got.lines().count(), doc.lines().count());
        assert_eq!(got, "a\n\nb\nc\n");
    }

    #[test]
    fn front_matter_is_stripped_only_when_it_is_there() {
        assert_eq!(strip_front_matter("---\ntitle: x\n---\nbody\n"), "body\n");
        assert_eq!(strip_front_matter("body\n"), "body\n");
        // A rule that is not front matter must survive.
        assert_eq!(strip_front_matter("text\n---\nmore\n"), "text\n---\nmore\n");
        // An unterminated block is not front matter either.
        assert_eq!(strip_front_matter("---\nnope\n"), "---\nnope\n");
    }
}
