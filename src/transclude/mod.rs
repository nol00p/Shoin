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
    /// The open fence's character and length.
    open: Option<(char, usize)>,
}

impl Fences {
    /// Feed lines in order. True when this line's markup is literal — which
    /// includes the delimiters themselves, so a fence line is never mistaken
    /// for content.
    ///
    /// CommonMark's rules, as far as a line at a time can follow them: a fence
    /// is three or more of one character, indented at most three spaces; a
    /// backtick fence's info string has no backtick (so "```js``` is the
    /// syntax" is a line of text with code in it); and a closing fence uses
    /// the opener's character, is at least as long, and has nothing after it.
    pub fn literal(&mut self, line: &str) -> bool {
        let Some((c, len, rest)) = fence(line) else {
            return self.open.is_some();
        };
        match self.open {
            Some((open, at_least)) => {
                if c == open && len >= at_least && rest.trim().is_empty() {
                    self.open = None;
                }
                true
            }
            None => {
                if c == '`' && rest.contains('`') {
                    return false;
                }
                self.open = Some((c, len));
                true
            }
        }
    }
}

/// A fence run at the start of `line`: its character, its length and what
/// follows it. `None` when the line does not start with one.
fn fence(line: &str) -> Option<(char, usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let t = &line[indent..];
    let c = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = t.len() - t.trim_start_matches(c).len();
    (len >= 3).then(|| (c, len, &t[len..]))
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

#[cfg(test)]
mod tests {
    use super::Fences;

    /// Which of `lines` read as literal, fed in order.
    fn literal(lines: &[&str]) -> Vec<bool> {
        let mut f = Fences::default();
        lines.iter().map(|l| f.literal(l)).collect()
    }

    #[test]
    fn a_fence_opens_and_closes() {
        assert_eq!(literal(&["a", "```rust", "x", "```", "b"]), [false, true, true, true, false]);
        assert_eq!(literal(&["~~~", "x", "~~~~", "b"]), [true, true, true, false]);
        assert_eq!(literal(&["   ```", "x", "   ```", "b"]), [true, true, true, false]);
    }

    /// Backticks in the info string: inline code at the start of a line,
    /// not a fence, so nothing after it turns literal.
    #[test]
    fn backticks_in_the_info_string_are_no_fence() {
        assert_eq!(literal(&["```js``` is the syntax", "b"]), [false, false]);
        assert_eq!(literal(&["~~~ a`b", "x", "~~~"]), [true, true, true], "tildes may");
    }

    /// Four spaces is an indented code block, not a fence.
    #[test]
    fn a_fence_is_indented_three_spaces_at_most() {
        assert_eq!(literal(&["    ```", "b"]), [false, false]);
    }

    /// Only the same character, at least as long, with nothing after it,
    /// closes: the lines in between stay literal.
    #[test]
    fn only_a_matching_fence_closes() {
        let lines = ["````", "```", "~~~~", "```` rust", "x", "`````", "b"];
        assert_eq!(literal(&lines), [true, true, true, true, true, true, false]);
    }
}
