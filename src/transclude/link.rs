//! Parsing and resolving `![[…]]` embeds. SPEC.md §14.2.
//!
//! This module knows nothing about rendering or compiling — it turns the text
//! between the brackets into a path on disk and the slice of that file the
//! embed asked for. Everything above it (the export walk, and live preview)
//! is built on these two answers.

use std::path::{Component, Path, PathBuf};

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
    // Hoisted: the vault search below asks about EVERY file under the root,
    // and rebuilding the extension list per file allocated eight Strings each.
    let exts = linkable();
    for cand in candidates(&link.target, &exts) {
        let p = dir.join(&cand);
        if p.is_file() {
            return Ok(p);
        }
    }
    // Vault search, on the file NAME, case-insensitively, so `![[attention]]`
    // finds `fragments/attention.md`. What the link wrote is kept: a linkable
    // extension must match exactly (`diagram.png` never means `diagram.md`),
    // and folders must end the file's path (`archive/todo` is not
    // `sub/todo.md`).
    let wanted = link.target.trim_end_matches('/');
    let named_ext = has_linkable_ext(Path::new(wanted), &exts);
    let parts: Vec<String> = Path::new(wanted)
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    let Some((name, folders)) = parts.split_last() else {
        return Err(Unresolved::Missing(link.target.clone()));
    };
    let mut found = Vec::new();
    walk(root, 0, &mut |p| {
        if !has_linkable_ext(p, &exts) {
            return;
        }
        // Without a named extension, the name is everything before the
        // linkable one: `2024.01.15 meeting` is `2024.01.15 meeting.md`.
        let own = if named_ext { p.file_name() } else { p.file_stem() };
        if own.map(|s| s.to_string_lossy().to_lowercase()).as_ref() != Some(name) {
            return;
        }
        // Folders are read below the root only, so where the vault itself
        // lives never counts as a match.
        let rel = p.strip_prefix(root).unwrap_or(p);
        let mut dirs = rel
            .parent()
            .into_iter()
            .flat_map(|d| d.components().rev())
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase());
        if folders.iter().rev().all(|f| dirs.next().as_ref() == Some(f)) {
            found.push(p.to_path_buf());
        }
    });
    // A bare name prefers the note to the picture here too, as `candidates`
    // does beside the linking file: the note is the one you can edit.
    if !named_ext && found.iter().any(|p| is_text(p)) {
        found.retain(|p| is_text(p));
    }
    match found.len() {
        0 => Err(Unresolved::Missing(link.target.clone())),
        1 => Ok(found.remove(0)),
        _ => {
            found.sort();
            Err(Unresolved::Ambiguous(link.target.clone(), found))
        }
    }
}

/// Whether `target` ends in a linkable extension, in any case. A dot followed
/// by anything else is part of the name: `2024.01.15 meeting` has none.
fn has_linkable_ext(target: &Path, exts: &[String]) -> bool {
    target
        .extension()
        .is_some_and(|e| exts.contains(&e.to_string_lossy().to_lowercase()))
}

/// Whether a file's text can be embedded, rather than drawn as a picture.
fn is_text(p: &Path) -> bool {
    p.extension()
        .is_some_and(|e| EMBEDDABLE.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// The filenames a target could mean: as written when it names a linkable
/// extension, otherwise with each linkable extension appended.
///
/// A target with some other extension (`paper.pdf`) is tried as written
/// last, so `gx` on a link to a file beside the note still opens it.
fn candidates(target: &str, exts: &[String]) -> Vec<PathBuf> {
    if has_linkable_ext(Path::new(target), exts) {
        return vec![PathBuf::from(target)];
    }
    let mut out: Vec<PathBuf> =
        exts.iter().map(|e| PathBuf::from(format!("{target}.{e}"))).collect();
    if Path::new(target).extension().is_some() {
        out.push(PathBuf::from(target));
    }
    out
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

/// The 0-based line where a `Section` STARTS, for jumping to it.
///
/// The twin of `extract`, and deliberately a separate walk. `extract` answers
/// "which text does this link ask for", which is what an embed pastes into the
/// document; this answers "where does that text begin", which is where a reader
/// following the link should land. Sharing one function would mean returning
/// both from every caller, and the embed path has no use for a line number.
///
/// `None` means the section is not in this file — a heading that was renamed,
/// or a block id that never existed. The caller opens the file anyway: landing
/// at the top of the right note beats refusing to move.
pub fn section_line(text: &str, section: &Section) -> Option<usize> {
    match section {
        Section::All => Some(0),
        Section::Heading(want) => {
            let want = want.trim().to_lowercase();
            let mut fences = super::Fences::default();
            text.lines().position(|l| {
                !fences.literal(l)
                    && heading_of(l).is_some_and(|(_, t)| t.trim().to_lowercase() == want)
            })
        }
        // The same rule `block_with_id` matches on, so a link that resolves to
        // a block lands on the line that block ends at.
        Section::Block(id) => block_end(&text.lines().collect::<Vec<_>>(), id),
    }
}

/// A heading and everything under it, to the next heading of the same or higher
/// level. Matched case-insensitively on the trimmed title, as Obsidian does.
fn heading_section(text: &str, want: &str) -> Result<String> {
    let want_lc = want.trim().to_lowercase();
    let mut out: Vec<&str> = Vec::new();
    let mut level = 0usize;

    // A `#` inside a fence is code — a shell comment, most often. It is copied
    // into the section like any other line, but it never opens or closes one.
    let mut fences = super::Fences::default();
    for line in text.lines() {
        let heading = if fences.literal(line) { None } else { heading_of(line) };
        if let Some((l, title)) = heading {
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

/// `## Title` -> (2, "Title"). Four columns of indent make it an indented
/// code block instead, as `compile::demote` holds.
fn heading_of(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    if line.len() - t.len() >= 4 {
        return None;
    }
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
    let Some(end) = block_end(&lines, id) else {
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

/// The line a `^id` block ends on: the first line, outside fenced code, that
/// ends with the marker. Inside a fence a trailing `^abc` is code.
fn block_end(lines: &[&str], id: &str) -> Option<usize> {
    let marker = format!("^{id}");
    let mut fences = super::Fences::default();
    lines
        .iter()
        .position(|l| !fences.literal(l) && l.trim_end().ends_with(&marker))
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

    /// F06: a `#` comment inside a fenced block is code, not a heading. Read
    /// as one, it ended the section mid-fence and the export kept an opening
    /// fence with no close.
    #[test]
    fn a_hash_inside_a_fence_does_not_end_the_section() {
        let doc = "\
## Setup

```sh
# install it
make install
```

After the code.

## Next
more
";
        let got = extract(doc, &Section::Heading("Setup".into())).unwrap();
        assert_eq!(got, "## Setup\n\n```sh\n# install it\nmake install\n```\n\nAfter the code.\n");
        assert_eq!(section_line(doc, &Section::Heading("install it".into())), None);
        assert!(extract(doc, &Section::Heading("install it".into())).is_err());
        assert_eq!(section_line(doc, &Section::Heading("next".into())), Some(9));
    }

    /// An indented code block is literal too: a `#` four columns in is not a
    /// heading, as `demote` already holds.
    #[test]
    fn an_indented_hash_is_not_a_heading() {
        let doc = "## A\n\n    # not a heading\n\ntext\n\n   ## B\n";
        let got = extract(doc, &Section::Heading("A".into())).unwrap();
        assert!(got.contains("# not a heading") && got.contains("text"), "{got}");
        assert!(!got.contains("## B"), "three columns in is still a heading");
        assert_eq!(section_line(doc, &Section::Heading("not a heading".into())), None);
    }

    /// A `^id` at the end of a line inside a fence is code, not an anchor.
    #[test]
    fn a_block_id_inside_a_fence_is_not_found() {
        let doc = "```\necho ^abc\n```\n\nreal ^def\n";
        assert!(extract(doc, &Section::Block("abc".into())).is_err());
        assert_eq!(section_line(doc, &Section::Block("abc".into())), None);
        assert_eq!(extract(doc, &Section::Block("def".into())).unwrap(), "real");
        assert_eq!(section_line(doc, &Section::Block("def".into())), Some(4));
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

    /// F07: a dot in a note's name is part of the name, not an extension.
    /// Dated names are common in Obsidian vaults.
    #[test]
    fn a_dotted_name_finds_its_note() {
        let d = vault("dotted");
        put(&d, "2024.01.15 meeting.md", "met\n");
        put(&d, "far/2024.02.01 review.md", "rev\n");
        let from = d.join("comp.md");
        assert_eq!(
            resolve(&link("2024.01.15 meeting"), &from, &d).unwrap(),
            d.join("2024.01.15 meeting.md"),
            "beside the linking note"
        );
        assert_eq!(
            resolve(&link("2024.02.01 Review"), &from, &d).unwrap(),
            d.join("far/2024.02.01 review.md"),
            "and by name anywhere in the vault"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// F07: an extension the link names is kept. `![[diagram.png]]` with no
    /// such picture is missing, not `diagram.md`.
    #[test]
    fn a_named_extension_is_not_dropped() {
        let d = vault("named-ext");
        put(&d, "sub/diagram.md", "note\n");
        let from = d.join("comp.md");
        match resolve(&link("diagram.png"), &from, &d) {
            Err(Unresolved::Missing(t)) => assert_eq!(t, "diagram.png"),
            other => panic!("expected Missing, got {other:?}"),
        }
        assert!(resolve(&link("diagram.md"), &from, &d).is_ok());
        assert!(resolve(&link("Diagram.MD"), &from, &d).is_ok(), "extension case is ignored");
        assert!(resolve(&link("diagram"), &from, &d).is_ok());

        // Another extension: tried as written beside the note, so `gx` opens
        // it, and never read as `paper.md`.
        put(&d, "paper.md", "p\n");
        assert!(resolve(&link("paper.pdf"), &from, &d).is_err());
        put(&d, "paper.pdf", "%PDF\n");
        assert_eq!(resolve(&link("paper.pdf"), &from, &d).unwrap(), d.join("paper.pdf"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// F07: folders in a link narrow the vault search to paths that end in
    /// them; they are not dropped.
    #[test]
    fn folders_in_a_link_must_match() {
        let d = vault("folders");
        put(&d, "sub/todo.md", "t\n");
        put(&d, "a/Archive/todo.md", "old\n");
        let from = d.join("comp.md");
        match resolve(&link("archive/todo"), &from, &d) {
            Ok(p) => assert_eq!(p, d.join("a/Archive/todo.md")),
            other => panic!("expected a/Archive/todo.md, got {other:?}"),
        }
        match resolve(&link("other/todo"), &from, &d) {
            Err(Unresolved::Missing(t)) => assert_eq!(t, "other/todo"),
            other => panic!("expected Missing, got {other:?}"),
        }
        // A component must match whole: `chive/todo` is not `archive/todo`.
        assert!(resolve(&link("chive/todo"), &from, &d).is_err());
        assert_eq!(resolve(&link("sub/todo.md"), &from, &d).unwrap(), d.join("sub/todo.md"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// What F07 must not break: a bare name still finds a picture, matches
    /// case-insensitively, and two notes of one name still need a choice.
    #[test]
    fn the_vault_search_keeps_its_old_answers() {
        let d = vault("kept");
        std::fs::write(d.join("photo.png"), [0u8, 1, 2]).unwrap();
        put(&d, "deep/Note.md", "n\n");
        put(&d, "x/index.md", "1\n");
        put(&d, "y/index.md", "2\n");
        let from = d.join("comp.md");
        assert_eq!(resolve(&link("photo"), &from, &d).unwrap(), d.join("photo.png"));
        assert_eq!(resolve(&link("note"), &from, &d).unwrap(), d.join("deep/Note.md"));
        assert!(matches!(
            resolve(&link("index"), &from, &d),
            Err(Unresolved::Ambiguous(_, ref f)) if f.len() == 2
        ));
        assert_eq!(resolve(&link("x/index"), &from, &d).unwrap(), d.join("x/index.md"));

        // A bare name found as a note and a picture in the vault means the
        // note, as it does beside the linking file.
        put(&d, "p/chart.md", "c\n");
        std::fs::create_dir_all(d.join("q")).unwrap();
        std::fs::write(d.join("q/chart.png"), [0u8]).unwrap();
        assert_eq!(resolve(&link("chart"), &from, &d).unwrap(), d.join("p/chart.md"));
        assert_eq!(resolve(&link("chart.png"), &from, &d).unwrap(), d.join("q/chart.png"));
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
