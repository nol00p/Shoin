//! The file-tree explorer pane (`<leader>fe` / `<leader>fE`), Neo-tree style.
//!
//! `fe` roots it at the edited file's own folder, `fE` at `$HOME`, and `-` / `=`
//! move the root itself from there.
//!
//! A flattened list of the visible entries is rebuilt from an `expanded` set of
//! directories, so expand/collapse is just a set toggle + rebuild. Dotfiles are
//! hidden until `H` asks for them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The tree pane's column width (capped to half the terminal at render time).
pub const WIDTH: u16 = 32;

pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
    /// Last of its siblings — draws the `└` elbow instead of `├`.
    pub last: bool,
    /// One flag per ANCESTOR level (root excluded): whether that ancestor still
    /// has siblings below it, i.e. whether this row draws a `│` at that column.
    /// Carried on the entry because the flattened list has no parent links, and
    /// rendering only ever sees the window of rows on screen.
    pub guides: Vec<bool>,
}

pub struct FileTree {
    pub root: PathBuf,
    expanded: HashSet<PathBuf>,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// Whether the tree currently has input focus (vs. the editor).
    pub focused: bool,
    /// `H`: whether dotfiles are listed. Off by default — a notes folder's
    /// dotfiles are almost never what the reader came for.
    pub show_hidden: bool,
}

/// What activating (`l`/Enter) the selected entry did.
pub enum Activate {
    /// A directory was expanded or collapsed.
    Toggled,
    /// A file should be opened by the caller.
    Open(PathBuf),
}

impl FileTree {
    pub fn open(root: PathBuf) -> FileTree {
        let mut t = FileTree {
            expanded: HashSet::from([root.clone()]),
            root,
            entries: Vec::new(),
            selected: 0,
            focused: true,
            show_hidden: false,
        };
        t.rebuild();
        t
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.contains(path)
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn down(&mut self) {
        self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1));
    }

    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    pub fn select_last(&mut self) {
        self.selected = self.entries.len().saturating_sub(1);
    }

    pub fn refresh(&mut self) {
        self.rebuild();
    }

    /// Set the dotfile state outright, for a tree that is opening with one the
    /// caller remembers from last time. Rebuilds only when it actually changes.
    pub fn set_hidden(&mut self, hidden: bool) {
        if self.show_hidden != hidden {
            self.show_hidden = hidden;
            self.rebuild();
        }
    }

    /// `H`: list dotfiles, or stop listing them. Returns the new state so the
    /// caller can say which way it went — with no dotfiles in the directory
    /// the screen does not change, and silence would read as a dead key.
    ///
    /// The cursor keeps its ENTRY rather than its row index, since the rows
    /// above it have just moved.
    pub fn toggle_hidden(&mut self) -> bool {
        let was = self.selected_entry().map(|e| e.path.clone());
        self.show_hidden = !self.show_hidden;
        self.rebuild();
        if let Some(path) = was {
            if !self.select_path(&path) {
                self.selected = self.selected.min(self.entries.len().saturating_sub(1));
            }
        }
        self.show_hidden
    }

    /// The directory a new entry should land in, given where the cursor is.
    ///
    /// A directory row means "inside this one" — that is what a reader means by
    /// pointing at it, whether or not it happens to be expanded. A file row
    /// means "beside this file".
    pub fn target_dir(&self) -> PathBuf {
        match self.selected_entry() {
            Some(e) if e.is_dir => e.path.clone(),
            Some(e) => e.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.root.clone()),
            None => self.root.clone(),
        }
    }

    /// Expand every directory between the root and `path`, so a newly created
    /// entry is actually on screen rather than inside a collapsed parent.
    pub fn reveal(&mut self, path: &Path) {
        let mut dir = path.parent();
        while let Some(d) = dir {
            self.expanded.insert(d.to_path_buf());
            if d == self.root {
                break;
            }
            dir = d.parent();
        }
        self.rebuild();
        self.select_path(path);
    }

    /// `-`: re-root one level UP, so the tree can leave `$HOME` entirely.
    ///
    /// The old root stays expanded and selected, so the view keeps its place
    /// instead of dropping the reader into a stranger's directory listing.
    /// Returns false at `/`, which has no parent to climb to.
    pub fn root_up(&mut self) -> bool {
        let Some(parent) = self.root.parent().map(Path::to_path_buf) else {
            return false;
        };
        let was = std::mem::replace(&mut self.root, parent);
        self.expanded.insert(self.root.clone());
        self.expanded.insert(was.clone());
        self.rebuild();
        self.select_path(&was);
        true
    }

    /// `=`: re-root INTO the selected directory — the inverse of `root_up`, so
    /// a deep tree does not cost a screen of ancestors to read. A file row and
    /// the root itself have nowhere to descend to.
    pub fn root_into(&mut self) -> bool {
        let Some(entry) = self.selected_entry() else {
            return false;
        };
        if !entry.is_dir || entry.path == self.root {
            return false;
        }
        self.root = entry.path.clone();
        self.expanded.insert(self.root.clone());
        self.rebuild();
        self.select_first();
        true
    }

    /// Put the cursor on `path` if it is visible. Leaves it alone otherwise, so
    /// a failed lookup never scrolls the tree somewhere surprising.
    pub fn select_path(&mut self, path: &Path) -> bool {
        match self.entries.iter().position(|e| e.path == path) {
            Some(i) => {
                self.selected = i;
                true
            }
            None => false,
        }
    }

    /// After a deletion: the row that took the deleted one's place, clamped.
    pub fn select_nearest(&mut self, was: usize) {
        self.selected = was.min(self.entries.len().saturating_sub(1));
    }

    /// The root row cannot be renamed, moved or deleted from inside the tree —
    /// it is the frame of reference every other path is expressed against.
    pub fn selected_is_root(&self) -> bool {
        self.selected_entry().is_some_and(|e| e.path == self.root)
    }

    /// `path` written the way the tree shows it — relative to the root, so a
    /// move prompt is editable rather than an absolute path to scroll through.
    ///
    /// The ROOT itself relativizes to the empty string, which reads as nothing
    /// at all; it gets its own name instead, the same one the top row shows.
    pub fn relative(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        if rel.as_os_str().is_empty() {
            return self
                .root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.root.display().to_string());
        }
        rel.to_string_lossy().into_owned()
    }

    /// Resolve what the user typed in a move prompt: relative to the root.
    pub fn resolve(&self, input: &str) -> PathBuf {
        let p = Path::new(input.trim());
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.root.join(p)
        }
    }

    /// `l`/Enter: expand/collapse a directory, or hand a file back to open.
    pub fn activate(&mut self) -> Option<Activate> {
        let entry = self.entries.get(self.selected)?;
        if entry.is_dir {
            let path = entry.path.clone();
            if !self.expanded.insert(path.clone()) {
                self.expanded.remove(&path);
            }
            self.rebuild();
            Some(Activate::Toggled)
        } else {
            Some(Activate::Open(entry.path.clone()))
        }
    }

    /// `h`/Left: collapse an expanded directory, else jump to the parent entry.
    pub fn collapse_or_parent(&mut self) {
        let Some(entry) = self.entries.get(self.selected) else {
            return;
        };
        if entry.is_dir && self.expanded.contains(&entry.path) && entry.depth > 0 {
            let path = entry.path.clone();
            self.expanded.remove(&path);
            self.rebuild();
            return;
        }
        // Jump to the nearest shallower entry above.
        let depth = entry.depth;
        if depth == 0 {
            return;
        }
        for i in (0..self.selected).rev() {
            if self.entries[i].depth < depth {
                self.selected = i;
                return;
            }
        }
    }

    fn rebuild(&mut self) {
        let keep = self.selected_entry().map(|e| e.path.clone());
        self.entries.clear();
        let name = self
            .root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());
        self.entries.push(Entry {
            path: self.root.clone(),
            name,
            depth: 0,
            is_dir: true,
            last: true,
            guides: Vec::new(),
        });
        self.push_children(&self.root.clone(), 1, &mut Vec::new());

        // Keep the selection on the same path if it survived the rebuild.
        self.selected = keep
            .and_then(|p| self.entries.iter().position(|e| e.path == p))
            .unwrap_or(self.selected)
            .min(self.entries.len().saturating_sub(1));
    }

    /// `guides` is the ancestor chain built so far — pushed on the way down and
    /// popped on the way back up, so each row records the columns its own
    /// ancestors still occupy.
    fn push_children(&mut self, dir: &Path, depth: usize, guides: &mut Vec<bool>) {
        if !self.expanded.contains(dir) {
            return;
        }
        let items = read_dir_sorted(dir, self.show_hidden);
        let last_index = items.len().saturating_sub(1);
        for (i, (path, is_dir, name)) in items.into_iter().enumerate() {
            let last = i == last_index;
            self.entries.push(Entry {
                path: path.clone(),
                name,
                depth,
                is_dir,
                last,
                guides: guides.clone(),
            });
            if is_dir {
                // A last child's column is empty below it; anything else keeps
                // its `│` running down past its own subtree.
                guides.push(!last);
                self.push_children(&path, depth + 1, guides);
                guides.pop();
            }
        }
    }
}


/// What a file's icon means, so the renderer can color it from the theme's
/// existing palette rather than carrying a color table of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconKind {
    /// Prose the editor itself is for — Markdown and plain text.
    Prose,
    Code,
    /// Config, data, lockfiles.
    Data,
    Media,
    Plain,
}

/// The Nerd Font icon for a file name, with the color role it should take.
/// Extension-driven, with a few exact names worth recognizing; anything unknown
/// gets the generic document glyph.
///
/// Written as `\u{…}` escapes ON PURPOSE: these live in the Private Use Area,
/// where the literal characters are invisible in most editors and diffs, and
/// several tools quietly strip them. The comment is the glyph's real name.
pub fn file_icon(name: &str) -> (&'static str, IconKind) {
    let lower = name.to_lowercase();
    if lower.starts_with("cargo.") {
        return (RUST, IconKind::Code);
    }
    if lower.starts_with("readme") {
        return (MARKDOWN, IconKind::Prose);
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "md" | "markdown" | "mdx" => (MARKDOWN, IconKind::Prose),
        "txt" | "text" | "org" | "rst" => ("\u{f15c}", IconKind::Prose), // file-text
        "rs" => (RUST, IconKind::Code),
        "py" => ("\u{e73c}", IconKind::Code),           // dev-python
        "js" | "mjs" | "cjs" => ("\u{e74e}", IconKind::Code), // dev-javascript
        "ts" | "tsx" => ("\u{e628}", IconKind::Code),   // seti-typescript
        "html" | "htm" => ("\u{f13b}", IconKind::Code), // fa-html5
        "css" | "scss" => ("\u{f13c}", IconKind::Code), // fa-css3
        "sh" | "bash" | "zsh" | "fish" => ("\u{f489}", IconKind::Code), // oct-terminal
        "c" | "h" | "cpp" | "hpp" | "cc" | "go" | "lua" | "rb" | "java" | "swift" => {
            ("\u{f121}", IconKind::Code) // fa-code, the catch-all for source
        }
        "json" => ("\u{e60b}", IconKind::Data), // seti-json
        "toml" | "conf" | "ini" | "cfg" | "yaml" | "yml" => ("\u{f013}", IconKind::Data), // fa-cog
        "lock" => ("\u{f023}", IconKind::Data), // fa-lock
        "zip" | "gz" | "tar" | "xz" | "zst" => ("\u{f1c6}", IconKind::Data), // fa-file-archive
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => {
            ("\u{f1c5}", IconKind::Media) // fa-file-image
        }
        "pdf" => ("\u{f1c1}", IconKind::Media),                    // fa-file-pdf
        "mp4" | "mov" | "mkv" => ("\u{f1c8}", IconKind::Media),    // fa-file-video
        "mp3" | "wav" | "flac" => ("\u{f1c7}", IconKind::Media),   // fa-file-audio
        _ => ("\u{f15b}", IconKind::Plain),                        // fa-file
    }
}

const MARKDOWN: &str = "\u{f48a}"; // oct-markdown
const RUST: &str = "\u{e7a8}"; // dev-rust

/// Directory contents, directories first then case-insensitive. Dotfiles are
/// dropped unless `hidden` asks for them.
///
/// Shared with the fuzzy finder's walk, so both panels order files the same way.
pub fn read_dir_sorted(dir: &Path, hidden: bool) -> Vec<(PathBuf, bool, String)> {
    let mut items: Vec<(PathBuf, bool, String)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !hidden && name.starts_with('.') {
                return None;
            }
            let is_dir = e.path().is_dir();
            Some((e.path(), is_dir, name))
        })
        .collect();
    items.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.2.to_lowercase().cmp(&b.2.to_lowercase()))
    });
    items
}
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("shoin-tree-{t}"));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("a.md"), "").unwrap();
        std::fs::write(d.join("sub").join("b.md"), "").unwrap();
        std::fs::write(d.join(".hidden"), "").unwrap();
        d
    }

    #[test]
    fn relative_and_target_dir_name_the_right_places() {
        let d = fixture();
        let mut t = FileTree::open(d.clone());
        t.select_path(&d.join("a.md"));

        assert_eq!(t.target_dir(), d, "a file's neighbours are its parent dir");
        assert_eq!(
            t.relative(&t.target_dir()),
            d.file_name().unwrap().to_string_lossy(),
            "the ROOT shows its own name, not an empty string"
        );
        assert_eq!(t.relative(&d.join("sub/b.md")), "sub/b.md");

        t.select_path(&d.join("sub"));
        assert_eq!(t.target_dir(), d.join("sub"), "a dir row means inside it");
        assert_eq!(t.relative(&t.target_dir()), "sub");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn dirs_first_dotfiles_hidden() {
        let d = fixture();
        let t = FileTree::open(d.clone());
        assert_eq!(t.entries[0].depth, 0); // the root itself
        assert!(!t.entries.iter().any(|e| e.name == ".hidden"));
        let sub = t.entries.iter().position(|e| e.name == "sub").unwrap();
        let file = t.entries.iter().position(|e| e.name == "a.md").unwrap();
        assert!(sub < file, "directories sort before files");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn guides_track_the_ancestor_chain() {
        let d = fixture();
        let mut t = FileTree::open(d.clone());
        // Root: "sub" (a dir, sorted first) then "a.md" — the last sibling.
        let sub = t.entries.iter().position(|e| e.name == "sub").unwrap();
        let a = t.entries.iter().position(|e| e.name == "a.md").unwrap();
        assert!(!t.entries[sub].last, "a.md still follows it");
        assert!(t.entries[a].last, "nothing after a.md");
        assert!(t.entries[sub].guides.is_empty(), "depth 1 has no ancestor columns");

        // Expanding "sub": its child sits under a parent that has a sibling
        // below, so its column keeps a running guide.
        t.selected = sub;
        t.activate();
        let b = t.entries.iter().position(|e| e.name == "b.md").unwrap();
        assert_eq!(t.entries[b].guides, vec![true], "a `│` runs past sub's subtree");
        assert!(t.entries[b].last);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn icons_follow_the_file_type() {
        assert_eq!(file_icon("notes.md").1, IconKind::Prose);
        assert_eq!(file_icon("README").1, IconKind::Prose);
        assert_eq!(file_icon("app.rs"), (RUST, IconKind::Code));
        assert_eq!(file_icon("Cargo.lock"), (RUST, IconKind::Code));
        assert_eq!(file_icon("theme.conf").1, IconKind::Data);
        assert_eq!(file_icon("shot.PNG").1, IconKind::Media, "extension match is case-blind");
        assert_eq!(file_icon("mystery").1, IconKind::Plain);
        // Every icon is exactly one cell wide, or the tree's columns drift.
        for name in ["notes.md", "app.rs", "x.json", "x.png", "mystery"] {
            assert_eq!(file_icon(name).0.chars().count(), 1);
        }
    }

    #[test]
    fn h_reveals_dotfiles_and_keeps_the_cursor_put() {
        let d = fixture();
        let mut t = FileTree::open(d.clone());
        t.select_path(&d.join("a.md"));

        assert!(!t.entries.iter().any(|e| e.name == ".hidden"), "hidden by default");
        assert!(t.toggle_hidden(), "H turns them on");
        assert!(t.entries.iter().any(|e| e.name == ".hidden"), "now listed");
        assert_eq!(
            t.selected_entry().unwrap().path,
            d.join("a.md"),
            "the cursor keeps its entry, not its row index"
        );

        assert!(!t.toggle_hidden(), "H turns them off again");
        assert!(!t.entries.iter().any(|e| e.name == ".hidden"));
        assert_eq!(t.selected_entry().unwrap().path, d.join("a.md"));
    }

    #[test]
    fn the_root_climbs_out_and_back_in() {
        let d = fixture();
        let mut t = FileTree::open(d.join("sub"));

        // `-` climbs to the parent, and keeps the old root in view rather than
        // dumping the reader into an unfamiliar listing.
        assert!(t.root_up());
        assert_eq!(t.root, d);
        assert_eq!(t.selected_entry().unwrap().path, d.join("sub"));

        // `=` descends into the selected directory — the exact inverse.
        assert!(t.root_into());
        assert_eq!(t.root, d.join("sub"));

        // A file row has nowhere to descend to, and neither has the root.
        t.select_path(&d.join("sub").join("b.md"));
        assert!(!t.root_into());
        t.select_first();
        assert!(!t.root_into());
    }

    #[test]
    fn climbing_stops_at_the_filesystem_root() {
        let mut t = FileTree::open(PathBuf::from("/"));
        assert!(!t.root_up(), "/ has no parent to climb to");
        assert_eq!(t.root, PathBuf::from("/"));
    }

    #[test]
    fn expand_then_open_a_file() {
        let d = fixture();
        let mut t = FileTree::open(d.clone());
        t.selected = t.entries.iter().position(|e| e.name == "sub").unwrap();
        assert!(matches!(t.activate(), Some(Activate::Toggled)));
        assert!(t.entries.iter().any(|e| e.name == "b.md"), "child now visible");

        t.selected = t.entries.iter().position(|e| e.name == "b.md").unwrap();
        match t.activate() {
            Some(Activate::Open(p)) => assert!(p.ends_with("b.md")),
            _ => panic!("activating a file should open it"),
        }
        std::fs::remove_dir_all(&d).ok();
    }
}
