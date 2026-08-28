//! The fuzzy file finder overlay (`<leader>ff`).
//!
//! A centered spotlight box listing every file under its root, narrowed
//! as you type. The candidate list is walked ONCE when the overlay opens (it is
//! a toggled panel — reopening rewalks), so typing only ever rescores an
//! in-memory list. Matching is a subsequence match with fzf-style bonuses:
//! consecutive characters, path-segment and word starts, and hits inside the
//! file name all score higher than a scattered match deep in a long path.

use std::path::{Path, PathBuf};

use crate::tree::read_dir_sorted;

/// Result rows the overlay shows at once.
pub const VISIBLE_ROWS: usize = 10;

/// Ceiling on the walk, so opening the finder in `/` or a home directory stays
/// responsive rather than enumerating the disk.
const MAX_FILES: usize = 20_000;
/// Directory depth cap — also what stops a symlink cycle from walking forever.
const MAX_DEPTH: usize = 16;
/// Build directories that are never interesting to open by hand. Dotfiles (and
/// so `.git`) are hidden here always — the finder has no `H` to reveal them.
const IGNORED_DIRS: &[&str] = &["target", "node_modules"];

/// What the overlay is picking from — files on disk, or the open buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Files,
    Buffers,
}

/// One candidate: the string that is matched and displayed, pre-split into
/// chars (matching indexes by char, not byte), plus what picking it means.
struct Candidate {
    /// The file to open. Empty for an unnamed buffer, which has no path.
    path: PathBuf,
    /// The buffer index, for `Kind::Buffers`.
    id: usize,
    rel: String,
    chars: Vec<char>,
    lower: Vec<char>,
}

impl Candidate {
    fn new(rel: String, path: PathBuf, id: usize) -> Candidate {
        let chars: Vec<char> = rel.chars().collect();
        // Folded one char at a time, NOT via `to_lowercase()` on the whole
        // string: a few characters lowercase into two, and `positions`
        // indexes `chars` and `lower` interchangeably.
        let lower: Vec<char> = chars.iter().map(fold).collect();
        Candidate { path, id, rel, chars, lower }
    }
}

/// A candidate that survived the current query.
pub struct Match {
    /// Index into `Finder::files`.
    file: usize,
    /// Char offsets in the relative path that the query matched, for highlight.
    positions: Vec<usize>,
    score: i32,
}

pub struct Finder {
    pub kind: Kind,
    files: Vec<Candidate>,
    /// True when the walk hit `MAX_FILES` and the list is a prefix of the tree.
    pub truncated: bool,
    pub query: String,
    /// Candidates matching `query`, best first.
    pub matches: Vec<Match>,
    pub selected: usize,
}

impl Finder {
    pub fn open(root: PathBuf) -> Finder {
        let files = walk(&root);
        let mut f = Finder {
            kind: Kind::Files,
            truncated: files.len() >= MAX_FILES,
            files,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
        };
        f.refilter();
        f
    }

    /// The buffer switcher (`<leader>fb`): the same overlay over the open
    /// documents instead of the filesystem. No walk, so no `root`.
    pub fn buffers(entries: Vec<(usize, String)>) -> Finder {
        let files = entries
            .into_iter()
            .map(|(id, name)| Candidate::new(name, PathBuf::new(), id))
            .collect();
        let mut f = Finder {
            kind: Kind::Buffers,
            files,
            truncated: false,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
        };
        f.refilter();
        f
    }

    /// The buffer index of the selected row, for `Kind::Buffers`.
    pub fn selected_id(&self) -> Option<usize> {
        let m = self.matches.get(self.selected)?;
        Some(self.files[m.file].id)
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// The relative path and matched char offsets of the `i`th result row.
    pub fn row(&self, i: usize) -> Option<(&str, &[usize])> {
        let m = self.matches.get(i)?;
        Some((self.files[m.file].rel.as_str(), m.positions.as_slice()))
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        let m = self.matches.get(self.selected)?;
        Some(self.files[m.file].path.clone())
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refilter();
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
        self.refilter();
    }

    pub fn clear_query(&mut self) {
        self.query.clear();
        self.refilter();
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn down(&mut self) {
        self.selected = (self.selected + 1).min(self.matches.len().saturating_sub(1));
    }

    /// Rescore every candidate against the query. The selection returns to the
    /// best match: after a keystroke the ranking has changed under it, so
    /// holding the old row would be holding a row the user never chose.
    fn refilter(&mut self) {
        // Smart case: an all-lowercase query matches case-insensitively; one
        // typed with any capital means the case was deliberate.
        let sensitive = self.query.chars().any(|c| c.is_uppercase());
        let query: Vec<char> = if sensitive {
            self.query.chars().collect()
        } else {
            self.query.chars().map(|c| fold(&c)).collect()
        };

        self.matches.clear();
        for (i, cand) in self.files.iter().enumerate() {
            let hay = if sensitive { &cand.chars } else { &cand.lower };
            if let Some((score, positions)) = score_match(&query, hay, &cand.chars) {
                self.matches.push(Match { file: i, positions, score });
            }
        }
        // With no query there is nothing to rank by, so the source order stands
        // — the walk's own sort for files, the buffer order for buffers, which
        // is what `:b <n>` numbers.
        if self.query.is_empty() {
            self.selected = 0;
            return;
        }
        // Best score first; ties go to the shorter path, then alphabetically, so
        // the order never depends on the walk's arbitrary interleaving.
        self.matches.sort_by(|a, b| {
            let (ca, cb) = (&self.files[a.file], &self.files[b.file]);
            b.score
                .cmp(&a.score)
                .then_with(|| ca.chars.len().cmp(&cb.chars.len()))
                .then_with(|| ca.rel.cmp(&cb.rel))
        });
        self.selected = 0;
    }
}

/// Case-fold one char, keeping it one char (see `walk`).
fn fold(c: &char) -> char {
    c.to_lowercase().next().unwrap_or(*c)
}

/// Score `query` (already case-folded to match `hay`) against one candidate.
/// `raw` is the un-folded path, for the camelCase boundary test. `None` when
/// the query is not a subsequence of the path at all.
fn score_match(query: &[char], hay: &[char], raw: &[char]) -> Option<(i32, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }

    // Leftmost subsequence match, to find the earliest end position...
    let mut end = 0;
    let mut from = 0;
    for &q in query {
        end = (from..hay.len()).find(|&j| hay[j] == q)?;
        from = end + 1;
    }

    // ...then tighten it by matching backwards from that end, so `app` prefers
    // `src/app.rs` over the scattered `a`…`p`…`p` a greedy forward walk finds.
    let mut positions = vec![0usize; query.len()];
    let mut j = end;
    for (k, &q) in query.iter().enumerate().rev() {
        // The forward pass proved a match exists at or before `j`.
        let found = (0..=j).rev().find(|&p| hay[p] == q)?;
        positions[k] = found;
        j = found.saturating_sub(1);
    }

    let name_start = raw.iter().rposition(|&c| c == '/').map(|i| i + 1).unwrap_or(0);
    let mut score = 0i32;
    let mut prev: Option<usize> = None;
    for &p in &positions {
        match prev {
            Some(pv) if p == pv + 1 => score += 8,
            // A gap costs, but only up to a point — one long path segment in
            // the middle should not sink an otherwise tight match.
            Some(pv) => score -= (p - pv - 1).min(6) as i32,
            None => score -= p.min(12) as i32,
        }
        if p == 0 {
            score += 10;
        } else if matches!(raw[p - 1], '/' | '_' | '-' | '.' | ' ') {
            score += 8;
        } else if raw[p].is_uppercase() && raw[p - 1].is_lowercase() {
            score += 6;
        }
        if p >= name_start {
            score += 4;
        }
        prev = Some(p);
    }
    // All else equal, the shorter path is the one you meant.
    score -= (raw.len() / 8) as i32;
    Some((score, positions))
}

/// Every file under `root`, relative-path form, dotfiles and build directories
/// skipped. Directory order comes from the tree's own sort, so the unfiltered
/// list reads the same way the tree pane does.
fn walk(root: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let mut subdirs = Vec::new();
        for (path, is_dir, name) in read_dir_sorted(&dir, false) {
            if is_dir {
                if depth + 1 < MAX_DEPTH && !IGNORED_DIRS.contains(&name.as_str()) {
                    subdirs.push((path, depth + 1));
                }
                continue;
            }
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().into_owned();
            out.push(Candidate::new(rel, path, 0));
            if out.len() >= MAX_FILES {
                return out;
            }
        }
        // Pushed in reverse so the stack pops them back in sorted order.
        while let Some(d) = subdirs.pop() {
            stack.push(d);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let d = std::env::temp_dir().join(format!("shoin-finder-{t}"));
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("target")).unwrap();
        std::fs::create_dir_all(d.join("notes")).unwrap();
        std::fs::write(d.join("README.md"), "").unwrap();
        std::fs::write(d.join("src").join("app.rs"), "").unwrap();
        std::fs::write(d.join("notes").join("a-plan.md"), "").unwrap();
        std::fs::write(d.join("target").join("build.rs"), "").unwrap();
        std::fs::write(d.join(".hidden"), "").unwrap();
        d
    }

    fn rels(f: &Finder) -> Vec<String> {
        (0..f.matches.len()).map(|i| f.row(i).unwrap().0.to_string()).collect()
    }

    #[test]
    fn walk_skips_dotfiles_and_build_dirs() {
        let d = fixture();
        let f = Finder::open(d.clone());
        let list = rels(&f);
        assert!(list.iter().any(|p| p.ends_with("app.rs")));
        assert!(!list.iter().any(|p| p.contains(".hidden")), "dotfiles hidden");
        assert!(!list.iter().any(|p| p.contains("target")), "build dirs skipped");
        assert_eq!(f.file_count(), 3);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn typing_narrows_and_ranks_the_file_name_first() {
        let d = fixture();
        let mut f = Finder::open(d.clone());
        for c in "app".chars() {
            f.push_char(c);
        }
        assert_eq!(rels(&f), vec!["src/app.rs".to_string()]);
        assert_eq!(f.selected_path().unwrap(), d.join("src").join("app.rs"));

        // `an` matches "a-plan.md" as a subsequence but not "app.rs".
        f.clear_query();
        for c in "an".chars() {
            f.push_char(c);
        }
        assert_eq!(rels(&f), vec!["notes/a-plan.md".to_string()]);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn backspace_widens_again_and_selection_stays_in_range() {
        let d = fixture();
        let mut f = Finder::open(d.clone());
        for c in "zzz".chars() {
            f.push_char(c);
        }
        assert!(f.matches.is_empty());
        assert!(f.selected_path().is_none(), "no match, nothing to open");
        f.down(); // must not run off the end of an empty list
        assert_eq!(f.selected, 0);

        f.pop_char();
        f.pop_char();
        f.pop_char();
        assert_eq!(f.matches.len(), 3, "empty query matches everything");
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn a_name_match_outranks_a_scattered_path_match() {
        // "rs" is a subsequence of both, but consecutive inside "app.rs".
        let hay: Vec<char> = "src/app.rs".chars().collect();
        let (tight, _) = score_match(&['r', 's'], &hay, &hay).unwrap();
        let other: Vec<char> = "readme/section.md".chars().collect();
        let (loose, _) = score_match(&['r', 's'], &other, &other).unwrap();
        assert!(tight > loose, "consecutive in-name match should win: {tight} vs {loose}");
    }

    #[test]
    fn positions_mark_the_matched_characters() {
        let hay: Vec<char> = "src/app.rs".chars().collect();
        let (_, positions) = score_match(&['a', 'p', 'p'], &hay, &hay).unwrap();
        assert_eq!(positions, vec![4, 5, 6]);
        assert!(score_match(&['q'], &hay, &hay).is_none());
    }

    #[test]
    fn smart_case_only_applies_when_the_query_has_a_capital() {
        let d = fixture();
        let mut f = Finder::open(d.clone());
        for c in "readme".chars() {
            f.push_char(c);
        }
        assert_eq!(rels(&f), vec!["README.md".to_string()], "lowercase is case-blind");

        f.clear_query();
        for c in "READ".chars() {
            f.push_char(c);
        }
        assert_eq!(rels(&f), vec!["README.md".to_string()]);

        f.clear_query();
        for c in "aP".chars() {
            f.push_char(c);
        }
        assert!(f.matches.is_empty(), "a capital makes the match case-sensitive");
        std::fs::remove_dir_all(&d).ok();
    }
}
