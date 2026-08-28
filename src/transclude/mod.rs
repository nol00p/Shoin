//! Transclusion — `![[note]]` embedding. SPEC.md §14.
//!
//! Three layers, and they are separable on purpose:
//!   * `link` turns the text between the brackets into a path and a slice of
//!     that file. It touches the filesystem and nothing else.
//!   * `compile` walks those links recursively to produce a flat document.
//!     It keeps that name because flattening is what it does — the command a
//!     reader types is `:export`, in `crate::export`, which adds the formats.
//!   * live preview (in `render/`) renders the same expansion on screen.

/// How much of an embed live preview shows.
///
/// A ladder: each step shows more than the one before it. `Short` is the
/// default. `Full` is a preview of exactly what `:export` writes, which is why
/// it drops the frame — the point of it is to read the finished document, not
/// to see its seams.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// `![[…]]` stays as written, styled as a link and nothing more — what
    /// the editor did before preview existed.
    ///
    /// Spelled `none` to the reader: the ladder reads none · short · rec ·
    /// full. `off` is accepted too, and was the original name.
    #[default]
    Off,
    /// The target's own text, in a labelled box. Nested embeds stay as links.
    Short,
    /// Everything the target pulls in, recursively, still in a box.
    Rec,
    /// The same, with no box — the composition reads as one document.
    Full,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "false" | "no" => Mode::Off,
            "short" | "on" | "true" | "yes" => Mode::Short,
            // `long` is accepted as well as `rec`: it is the obvious word to
            // reach for, and refusing it would be a papercut for nothing.
            "rec" | "recursive" | "long" => Mode::Rec,
            "full" => Mode::Full,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Off => "none",
            Mode::Short => "short",
            Mode::Rec => "rec",
            Mode::Full => "full",
        }
    }

    pub fn is_on(self) -> bool {
        !matches!(self, Mode::Off)
    }

    /// Whether nested embeds inside the target are expanded too.
    pub fn recurses(self) -> bool {
        matches!(self, Mode::Rec | Mode::Full)
    }

    /// Whether the box is drawn. `Full` never draws one, whatever
    /// `transclude.border` says — a frame would defeat the mode.
    pub fn framed(self) -> bool {
        matches!(self, Mode::Short | Mode::Rec)
    }
}

/// Whether the line being read sits inside a fenced code block.
///
/// Markdown inside a fence is LITERAL, and three passes need to know it:
/// `demote` must not renumber a `#` that is a shell comment, `strip_block_ids`
/// must not eat a `^` that is an anchor in a regex, and `compile::expand` must
/// not embed an `![[…]]` that is being DOCUMENTED rather than used. All three
/// asked the same question and two of them had their own answer; this is the
/// one answer.
#[derive(Default)]
pub struct Fences {
    open: Option<String>,
}

impl Fences {
    /// Feed lines in order. True when this line's markup is literal — which
    /// includes the delimiters themselves, so a fence line is never mistaken
    /// for content.
    pub fn literal(&mut self, line: &str) -> bool {
        let t = line.trim_start();
        match &self.open {
            Some(open) => {
                // A closing fence is at least as long as the one it closes, so
                // `starts_with` is the whole rule.
                if t.starts_with(open.as_str()) {
                    self.open = None;
                }
                true
            }
            None => {
                if t.starts_with("```") || t.starts_with("~~~") {
                    self.open =
                        Some(t.chars().take_while(|c| *c == '`' || *c == '~').collect());
                    return true;
                }
                false
            }
        }
    }
}

pub mod compile;
pub mod link;
pub mod preview;

use std::path::Path;

/// Whether two paths name the same file, resolving symlinks and `..` where the
/// files exist. Used to keep `:export` from writing over its own source —
/// comparing the strings would miss `./notes.md` against `notes.md`.
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        // A destination that does not exist yet cannot be the source.
        _ => a == b,
    }
}
