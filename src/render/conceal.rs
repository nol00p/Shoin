//! Concealment — the Obsidian live-preview model. SPEC.md §2, §5.5.
//!
//! THE INVARIANT: the active line is rendered 1:1 with its source; every other
//! line may conceal markup.
//!
//! Because the cursor only ever sits on a 1:1 line, this mapping is needed for
//! non-active lines only, and only for read-only concerns. The one direction
//! anything asks for today is SCREEN -> SOURCE (`source_col`), for mouse
//! click-to-position — the case SPEC §5.5 anticipated. Frame overlays (search
//! matches, the selection, focus dim) are applied in SOURCE coordinates before
//! `render` remaps them, so they need no mapping of their own.

use std::ops::Range;

use unicode_width::UnicodeWidthStr;

use crate::config::schema::GlyphConfig;
use crate::input::mode::Mode;
use crate::text::cursor::Cursor;

use super::indent;
use super::layout::cell_width;
use super::markdown::block::{BlockKind, Marker};
use super::markdown::inline::InlineSpan;
use super::theme::{Attrs, Color, Style, Theme};
use super::StyledSpan;

/// One display-only transformation of a source range.
#[derive(Clone, Debug, PartialEq)]
pub enum Conceal {
    /// Drop these source chars from the frame entirely.
    Hide(Range<usize>),
    /// Draw this string in place of the range. Used for glyph substitution
    /// (`- ` -> ` `, `> ` -> `▎ `) and indent guides.
    ///
    /// `style` overrides what the glyph inherits: a quote bar should wear
    /// `theme.quote_bar` and a ticked task box `theme.task_done`, not the dim
    /// marker style of the source it replaces. `None` keeps the inherited one.
    Replace {
        range: Range<usize>,
        text: String,
        style: Option<Style>,
    },
}

impl Conceal {
    pub fn range(&self) -> &Range<usize> {
        match self {
            Conceal::Hide(r) => r,
            Conceal::Replace { range, .. } => range,
        }
    }

    /// A replacement with no style of its own.
    fn plain(range: Range<usize>, text: String) -> Self {
        Conceal::Replace {
            range,
            text,
            style: None,
        }
    }
}

/// What concealment needs to know beyond the line itself: the glyphs to
/// substitute, the colors they wear, and the width a full-line rule spans.
pub struct ConcealCtx<'a> {
    pub glyphs: &'a GlyphConfig,
    pub theme: &'a Theme,
    /// The text measure in cells — a fence delimiter conceals to a rule this
    /// wide, so an entry built with it is only valid at that measure.
    pub measure: u16,
    /// Columns per indent level, for the §5.4 guides.
    pub tab_width: usize,
}

/// Built per visible, NON-ACTIVE line. Active lines have no map at all —
/// `screen_col` is the identity for them.
#[derive(Default)]
pub struct ConcealMap {
    /// Sorted by start, non-overlapping. Enforced by `build`, asserted in tests.
    pub ops: Vec<Conceal>,
}

/// The set of lines rendered raw this frame. SPEC.md §2, "The active set".
///
/// Normal/Insert/Command/Search -> the cursor's line.
/// Visual/VisualLine            -> every line the selection touches, so you
///                                 always see the exact source you are about to
///                                 operate on.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ActiveSet {
    pub start: usize,
    /// Inclusive.
    pub end: usize,
}

impl ActiveSet {
    /// No line at all: past the end of any document. Read-only mode and
    /// `--render`, where there is no cursor for a line to be revealed under.
    pub fn none() -> Self {
        ActiveSet { start: usize::MAX, end: usize::MAX }
    }

    pub fn compute(mode: &Mode, cursor: &Cursor, selection_anchor: Option<Cursor>) -> Self {
        match (mode, selection_anchor) {
            (Mode::Visual | Mode::VisualLine, Some(anchor)) => ActiveSet {
                start: anchor.line.min(cursor.line),
                end: anchor.line.max(cursor.line),
            },
            _ => ActiveSet {
                start: cursor.line,
                end: cursor.line,
            },
        }
    }

    pub fn contains(&self, line: usize) -> bool {
        line >= self.start && line <= self.end
    }
}

impl ConcealMap {
    /// Derive the conceal set from inline spans and block kind.
    ///
    /// `outer - inner` of each inline span IS the conceal set — the scanner
    /// already computed it, so there is no second parsing pass. Block-level
    /// markers (`## `, `> `, `- `) contribute `Hide` or `Replace` per §5.2.
    pub fn build(line: &str, block: &BlockKind, spans: &[InlineSpan], ctx: &ConcealCtx) -> Self {
        let chars: Vec<char> = line.chars().collect();
        let mut ops: Vec<Conceal> = Vec::new();

        block_ops(&chars, block, ctx, &mut ops);

        // Inline: the markers are exactly `outer - inner`, already computed by
        // the scanner — no second parse.
        for sp in spans {
            if sp.inner.start > sp.outer.start {
                ops.push(Conceal::Hide(sp.outer.start..sp.inner.start));
            }
            if sp.outer.end > sp.inner.end {
                ops.push(Conceal::Hide(sp.inner.end..sp.outer.end));
            }
            if sp.kind == crate::render::markdown::inline::Inline::Latex {
                let inner: String = chars[sp.inner.clone()].iter().collect();
                let text = render_latex(&inner);
                if text != inner {
                    ops.push(Conceal::Replace {
                        range: sp.inner.clone(),
                        text,
                        style: None,
                    });
                }
            }
        }

        // Sort and drop any overlap (block vs inline never overlap in practice,
        // but the invariant must hold regardless).
        ops.sort_by_key(|o| o.range().start);
        let mut clean: Vec<Conceal> = Vec::with_capacity(ops.len());
        let mut last_end = 0usize;
        for op in ops {
            if op.range().start >= last_end {
                last_end = op.range().end;
                clean.push(op);
            }
        }

        ConcealMap { ops: clean }
    }

    /// Which SOURCE column a display column maps to — mouse click-to-position,
    /// the case SPEC §5.5 anticipated. A click inside a hidden or replaced
    /// region lands on that region's source start.
    ///
    /// Takes the source line so visible characters are measured at their true
    /// width: a click past a CJK glyph or an emoji lands where it looks like it
    /// should, not one column per char.
    pub fn source_col(&self, source: &str, display_col: u16) -> usize {
        let chars: Vec<char> = source.chars().collect();
        let cell = |c: char| cell_width(c) as u16;

        let mut disp: u16 = 0;
        let mut src = 0usize;
        // Walk the visible run before each op, then the op itself.
        for op in &self.ops {
            let r = op.range();
            while src < r.start {
                let w = cell(*chars.get(src).unwrap_or(&' '));
                if display_col < disp + w.max(1) {
                    return src;
                }
                disp += w;
                src += 1;
            }
            match op {
                Conceal::Hide(rr) => src = rr.end,
                Conceal::Replace { range, text, .. } => {
                    let w = UnicodeWidthStr::width(text.as_str()) as u16;
                    if display_col < disp + w {
                        return range.start;
                    }
                    disp += w;
                    src = range.end;
                }
            }
        }
        while src < chars.len() {
            let w = cell(chars[src]);
            if display_col < disp + w.max(1) {
                return src;
            }
            disp += w;
            src += 1;
        }
        src
    }

    /// Apply the map to a source line and its SOURCE-indexed styled spans,
    /// producing the concealed display string and spans re-indexed onto it.
    /// Replacement glyphs inherit the style of the source span they sit under.
    pub fn render(&self, source: &str, spans: &[StyledSpan]) -> (String, Vec<StyledSpan>) {
        let chars: Vec<char> = source.chars().collect();
        let plain = Style {
            fg: Color::None,
            bg: Color::None,
            attrs: Attrs::default(),
        };
        // The styles are looked up at source columns that only ever grow, and
        // the spans are sorted and disjoint — the cache and the frame overlays
        // keep them so — so one index that moves forward finds each in turn.
        // Searching from the start for every char made a long line with many
        // spans quadratic: 625 ms a frame for 200 KB of bold words.
        debug_assert!(
            spans.windows(2).all(|w| w[0].range.end <= w[1].range.start),
            "styled spans out of order"
        );
        let mut next = 0usize;
        let mut style_at = |src: usize| -> Style {
            while next < spans.len() && spans[next].range.end <= src {
                next += 1;
            }
            match spans.get(next) {
                Some(s) if s.range.start <= src => s.style,
                _ => plain,
            }
        };

        let mut text = String::new();
        let mut out: Vec<StyledSpan> = Vec::new();
        let mut disp = 0usize;
        let mut push = |ch: char, style: Style, text: &mut String, disp: &mut usize| {
            text.push(ch);
            match out.last_mut() {
                Some(last) if last.style == style && last.range.end == *disp => {
                    last.range.end = *disp + 1;
                }
                _ => out.push(StyledSpan {
                    range: *disp..*disp + 1,
                    style,
                }),
            }
            *disp += 1;
        };

        let mut i = 0usize;
        let mut op_idx = 0usize;
        while i < chars.len() {
            match self.ops.get(op_idx) {
                Some(op) if op.range().start == i => {
                    match op {
                        Conceal::Hide(r) => i = r.end,
                        Conceal::Replace { range, text: s, style } => {
                            let style = style.unwrap_or_else(|| style_at(range.start));
                            for ch in s.chars() {
                                push(ch, style, &mut text, &mut disp);
                            }
                            i = range.end;
                        }
                    }
                    op_idx += 1;
                }
                _ => {
                    push(chars[i], style_at(i), &mut text, &mut disp);
                    i += 1;
                }
            }
        }
        (text, out)
    }

    /// How many cells further right the body text sits in the SOURCE than it
    /// does in the concealed form — i.e. the width the markers take back when
    /// the cursor lands on this line.
    ///
    /// `layout.stable_gutter` reserves exactly this much for the active line so
    /// its text stays in the same column as it is revealed. SPEC.md §6.
    /// Every op in the LEADING RUN counts, not just the first: a line's markers
    /// can conceal in several pieces — an indent guide, then a bullet, then a
    /// task's checkbox — and missing the later ones let the body text jump as
    /// the cursor arrived. The run ends at the first gap holding a non-blank
    /// character, which is where the body text begins.
    pub fn leading_shift(&self, source: &str) -> u16 {
        let chars: Vec<char> = source.chars().collect();
        let width_of = |c: &char| cell_width(*c);

        let mut shift = 0usize;
        let mut src = 0usize;
        for op in &self.ops {
            let r = op.range();
            if chars[src..r.start].iter().any(|c| !c.is_whitespace()) {
                break; // body text — everything past here is not a marker
            }
            let hidden: usize = chars[r.clone()].iter().map(width_of).sum();
            let shown = match op {
                Conceal::Hide(_) => 0,
                Conceal::Replace { text, .. } => UnicodeWidthStr::width(text.as_str()),
            };
            shift += hidden.saturating_sub(shown);
            src = r.end;
        }
        shift as u16
    }

    /// The concealed display string alone, without re-styling. The render
    /// cache needs it to wrap a line; the spans come later, once the frame's
    /// overlays are known.
    pub fn display_text(&self, source: &str) -> String {
        if self.ops.is_empty() {
            return source.to_string();
        }
        let chars: Vec<char> = source.chars().collect();
        let mut text = String::with_capacity(source.len());
        let mut i = 0usize;
        let mut op_idx = 0usize;
        while i < chars.len() {
            match self.ops.get(op_idx) {
                Some(op) if op.range().start == i => {
                    match op {
                        Conceal::Hide(r) => i = r.end,
                        Conceal::Replace { range, text: s, .. } => {
                            text.push_str(s);
                            i = range.end;
                        }
                    }
                    op_idx += 1;
                }
                _ => {
                    text.push(chars[i]);
                    i += 1;
                }
            }
        }
        text
    }

}

fn render_latex(input: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = input.trim().chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        i += 1;
        if ch == '\\' {
            if i < chars.len() {
                match chars[i] {
                    '\\' => {
                        let is_trailing = chars[i + 1..].iter().all(|c| c.is_whitespace());
                        if !is_trailing {
                            out.push_str(" │ ");
                        }
                        i += 1;
                        continue;
                    }
                    ',' | ' ' => {
                        out.push(' ');
                        i += 1;
                        continue;
                    }
                    ';' => {
                        out.push_str("  ");
                        i += 1;
                        continue;
                    }
                    '!' => {
                        i += 1;
                        continue;
                    }
                    // An escaped special is that character, as written.
                    c if SPECIALS.contains(c) => {
                        out.push(c);
                        i += 1;
                        continue;
                    }
                    '|' => {
                        out.push('‖');
                        i += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            let end = name_end(&chars, i);
            let name: String = chars[i..end].iter().collect();
            i = end;
            // `\operatorname*` only moves where limits go, which one line
            // cannot show anyway.
            if name == "operatorname" && chars.get(i) == Some(&'*') {
                i += 1;
            }
            if matches!(name.as_str(), "frac" | "dfrac") {
                if let Some((numerator, next)) = braced(&chars, i) {
                    if let Some((denominator, end)) = braced(&chars, next) {
                        let num = if numerator.chars().count() > 1 {
                            format!("({numerator})")
                        } else {
                            numerator
                        };
                        let den = if denominator.chars().count() > 1 {
                            format!("({denominator})")
                        } else {
                            denominator
                        };
                        out.push_str(&format!("{num}⁄{den}"));
                        i = end;
                        continue;
                    }
                }
            }
            if name == "sqrt" {
                if let Some((value, end)) = braced(&chars, i) {
                    out.push('√');
                    if value.chars().count() > 1 {
                        out.push('(');
                        out.push_str(&value);
                        out.push(')');
                    } else {
                        out.push_str(&value);
                    }
                    i = end;
                    continue;
                }
            }
            if matches!(name.as_str(), "vec" | "mathbf" | "bm" | "boldsymbol") {
                if let Some((value, end)) = argument(&chars, i) {
                    out.push_str(&if name == "vec" { accent(&value, '\u{20D7}') } else { value });
                    i = end;
                    continue;
                }
            }
            if name == "hat" {
                if let Some((value, end)) = argument(&chars, i) {
                    out.push_str(&match value.as_str() {
                        // Precomposed, so the dot of i and j does not sit under the hat.
                        "i" => "î".to_string(),
                        "j" => "ĵ".to_string(),
                        _ => accent(&value, '\u{0302}'),
                    });
                    i = end;
                    continue;
                }
            }
            if matches!(name.as_str(), "text" | "mathrm") {
                if let Some((value, end)) = braced_raw(&chars, i) {
                    out.push_str(&text_of(&value));
                    i = end;
                    continue;
                }
            }
            if name == "operatorname" {
                if let Some((value, end)) = braced_raw(&chars, i) {
                    push_operator(&mut out, &text_of(&value), &chars, end);
                    i = end;
                    continue;
                }
            }
            if name == "begin" {
                if let Some((env, end)) = braced_raw(&chars, i) {
                    let env = env.trim();
                    let end_tag: Vec<char> = format!("\\end{{{env}}}").chars().collect();
                    match chars[end..].windows(end_tag.len()).position(|w| w == end_tag) {
                        Some(len) => {
                            let inner: String = chars[end..end + len].iter().collect();
                            out.push_str(&render_environment(env, &inner));
                            i = end + len + end_tag.len();
                        }
                        // The `\end` is on a later line: open what this line can.
                        None => {
                            out.push_str(env_delims(env).0);
                            i = end;
                        }
                    }
                    continue;
                }
            }
            if name == "end" {
                if let Some((env, end)) = braced_raw(&chars, i) {
                    out.push_str(env_delims(env.trim()).1);
                    i = end;
                    continue;
                }
            }
            // Sizing and limit placement: nothing a single line can show.
            if SILENT.contains(&name.as_str()) {
                continue;
            }
            if OPERATORS.contains(&name.as_str()) {
                push_operator(&mut out, &name, &chars, i);
                continue;
            }
            let replacement = match name.as_str() {
                "nabla" => "∇",
                "partial" => "∂",
                "oint" => "∮",
                "oiint" => "∯",
                "iint" => "∬",
                "iiint" => "∭",
                "int" => "∫",
                "sum" => "∑",
                "prod" => "∏",
                "coprod" => "∐",
                "rightarrow" | "to" => "→",
                "leftarrow" | "gets" => "←",
                "leftrightarrow" => "↔",
                "Rightarrow" | "implies" => "⇒",
                "Leftarrow" => "⇐",
                "Leftrightarrow" | "iff" => "⇔",
                "downarrow" => "↓",
                "uparrow" => "↑",
                "times" => "×",
                "cdot" => "·",
                "div" => "÷",
                "pm" => "±",
                "mp" => "∓",
                "neq" | "ne" => "≠",
                "leq" | "le" => "≤",
                "geq" | "ge" => "≥",
                "approx" => "≈",
                "equiv" => "≡",
                "sim" => "∼",
                "propto" => "∝",
                "infty" => "∞",
                "hbar" => "ℏ",
                "forall" => "∀",
                "exists" => "∃",
                "nexists" => "∄",
                "in" => "∈",
                "notin" => "∉",
                "subset" => "⊂",
                "supset" => "⊃",
                "subseteq" => "⊆",
                "supseteq" => "⊇",
                "cap" => "∩",
                "cup" => "∪",
                "land" | "wedge" => "∧",
                "lor" | "vee" => "∨",
                "neg" | "lnot" => "¬",
                "circ" => "∘",
                "bullet" => "•",
                "star" => "⋆",
                "perp" => "⊥",
                "parallel" => "∥",
                "dots" | "ldots" => "…",
                "cdots" => "⋯",
                "vdots" => "⋮",
                "ddots" => "⋱",
                "quad" => "  ",
                "qquad" => "    ",
                // Greek lowercase
                "alpha" => "α",
                "beta" => "β",
                "gamma" => "γ",
                "delta" => "δ",
                "epsilon" => "ϵ",
                "varepsilon" => "ε",
                "zeta" => "ζ",
                "eta" => "η",
                "theta" => "θ",
                "vartheta" => "ϑ",
                "iota" => "ι",
                "kappa" => "κ",
                "lambda" => "λ",
                "mu" => "μ",
                "nu" => "ν",
                "xi" => "ξ",
                "pi" => "π",
                "varpi" => "ϖ",
                "rho" => "ρ",
                "varrho" => "ϱ",
                "sigma" => "σ",
                "varsigma" => "ς",
                "tau" => "τ",
                "upsilon" => "υ",
                "phi" => "ϕ",
                "varphi" => "φ",
                "chi" => "χ",
                "psi" => "ψ",
                "omega" => "ω",
                // Greek uppercase
                "Gamma" => "Γ",
                "Delta" => "Δ",
                "Theta" => "Θ",
                "Lambda" => "Λ",
                "Xi" => "Ξ",
                "Pi" => "Π",
                "Sigma" => "Σ",
                "Upsilon" => "Υ",
                "Phi" => "Φ",
                "Psi" => "Ψ",
                "Omega" => "Ω",
                _ => "",
            };
            if replacement.is_empty() {
                out.push('\\');
                out.push_str(&name);
            } else {
                out.push_str(replacement);
            }
        } else if ch == '&' {
            out.push_str("  ");
        } else if ch == '^' || ch == '_' {
            let superscript = ch == '^';
            // Only a group that closes counts as braced: while `x^{2` is
            // being typed, its `{` is just the next character.
            let mut is_braced = false;
            let mut value = String::new();
            if let Some((group, end)) = braced(&chars, i) {
                value = group;
                i = end;
                is_braced = true;
            } else if chars.get(i) == Some(&'\\') {
                // An unbraced command, with its own arguments, is the whole
                // argument: `e^\pi` is e to the π, `a_\text{max}` is a sub max.
                let end = command_span(&chars, i);
                value = render_latex(&chars[i..end].iter().collect::<String>());
                i = end;
            } else if i < chars.len() {
                value.push(chars[i]);
                i += 1;
            }

            // Convert to Unicode scripts only if every character in the group has a script form.
            // Otherwise, keep the marker and value as written (e.g. F_z -> F_z, e^{i\pi} -> e^{iπ}).
            let mapped: Option<String> = value
                .chars()
                .map(|c| if superscript { superscript_char(c) } else { subscript_char(c) })
                .collect();

            if let Some(converted) = mapped {
                out.push_str(&converted);
            } else {
                out.push(ch);
                if is_braced {
                    out.push('{');
                    out.push_str(&value);
                    out.push('}');
                } else {
                    out.push_str(&value);
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Operator names, written upright as themselves (`\sin` → `sin`).
const OPERATORS: &[&str] = &[
    "sin", "cos", "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh",
    "tanh", "coth", "log", "ln", "lg", "exp", "lim", "liminf", "limsup", "max", "min", "sup",
    "inf", "det", "dim", "ker", "deg", "gcd", "arg", "hom", "Pr", "mod", "bmod",
];

/// `value` with a combining `mark` on its first character.
fn accent(value: &str, mark: char) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => format!("{first}{mark}{}", chars.as_str()),
        None => String::new(),
    }
}

/// The delimiters a matrix environment is drawn between.
fn env_delims(env: &str) -> (&'static str, &'static str) {
    match env {
        "vmatrix" => ("|", "|"),
        "Vmatrix" => ("‖", "‖"),
        "pmatrix" => ("(", ")"),
        "bmatrix" => ("[", "]"),
        "Bmatrix" => ("{", "}"),
        _ => ("", ""),
    }
}

/// The characters that `\` escapes into themselves, in math and in text.
const SPECIALS: &str = "{}&%$#_";

/// Commands that only size delimiters or place limits: dropped.
const SILENT: &[&str] = &["left", "right", "limits", "nolimits"];

/// Where a command name starting at `from` ends: its run of letters.
fn name_end(chars: &[char], from: usize) -> usize {
    from + chars[from..].iter().take_while(|c| c.is_ascii_alphabetic()).count()
}

/// Where the command at the backslash `start` ends, together with the
/// `{…}` arguments it takes (`\frac{1}{2}`, `\text{max}`); a command that is
/// not a run of letters is the one character after the backslash.
fn command_span(chars: &[char], start: usize) -> usize {
    let mut end = name_end(chars, start + 1);
    if end == start + 1 {
        return (start + 2).min(chars.len());
    }
    let arity = match chars[start + 1..end].iter().collect::<String>().as_str() {
        "frac" | "dfrac" => 2,
        "text" | "mathrm" | "operatorname" | "sqrt" | "vec" | "hat" | "mathbf" | "bm"
        | "boldsymbol" => 1,
        _ => 0,
    };
    for _ in 0..arity {
        match braced_raw(chars, end) {
            Some((_, next)) => end = next,
            None => break,
        }
    }
    end
}

/// A command's one argument, rendered: a `{…}` group, a command, or a single
/// character, after any spaces (`\vec v`, `\hat\theta`).
fn argument(chars: &[char], start: usize) -> Option<(String, usize)> {
    let at = start + chars[start..].iter().take_while(|c| **c == ' ').count();
    if let Some(group) = braced(chars, at) {
        return Some(group);
    }
    match chars.get(at) {
        Some('\\') if name_end(chars, at + 1) > at + 1 => {
            let end = command_span(chars, at);
            Some((render_latex(&chars[at..end].iter().collect::<String>()), end))
        }
        Some(c) if c.is_alphanumeric() => Some((c.to_string(), at + 1)),
        _ => None,
    }
}

/// Write an operator name upright, kept apart from the letters around it:
/// `a\sin x` → `a sin x`, `\sin\theta` → `sin θ`, but `\sin(x)` and
/// `\sin\left(x\right)` → `sin(x)`. `next` is where the source goes on.
fn push_operator(out: &mut String, name: &str, chars: &[char], next: usize) {
    if out.chars().last().is_some_and(|c| c.is_alphanumeric()) {
        out.push(' ');
    }
    out.push_str(name);
    let spaced = match chars.get(next) {
        Some(c) if c.is_alphanumeric() => true,
        Some('\\') => {
            let end = name_end(chars, next + 1);
            let follower: String = chars[next + 1..end].iter().collect();
            end > next + 1 && !SILENT.contains(&follower.as_str())
        }
        _ => false,
    };
    if spaced {
        out.push(' ');
    }
}

/// The text of a `\text{…}`-style argument: escaped specials become
/// themselves and the spacing commands become spaces.
fn text_of(value: &str) -> String {
    let mut out = String::new();
    let mut rest = value.chars().peekable();
    while let Some(c) = rest.next() {
        match (c, rest.peek().copied()) {
            ('\\', Some(n)) if SPECIALS.contains(n) => {
                out.push(n);
                rest.next();
            }
            ('\\', Some(',' | ' ' | ';')) => {
                out.push(' ');
                rest.next();
            }
            ('\\', Some('!')) => {
                rest.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// A matrix body as rows of cells: split on `\\` and `&`, but not on an
/// escaped `\&`, which stays in its cell for `render_latex` to write as `&`.
fn cells(content: &str) -> Vec<Vec<String>> {
    let chars: Vec<char> = content.chars().collect();
    let mut rows = vec![vec![String::new()]];
    let mut k = 0;
    while k < chars.len() {
        let row = rows.last_mut().expect("never empty");
        match chars[k] {
            '\\' if chars.get(k + 1) == Some(&'\\') => {
                rows.push(vec![String::new()]);
                k += 2;
                continue;
            }
            '\\' => {
                let cell = row.last_mut().expect("never empty");
                cell.push('\\');
                cell.extend(chars.get(k + 1));
                k += 2;
                continue;
            }
            '&' => row.push(String::new()),
            c => row.last_mut().expect("never empty").push(c),
        }
        k += 1;
    }
    rows
}

fn render_environment(env: &str, content: &str) -> String {
    let (open, close) = env_delims(env);
    let rendered_rows: Vec<String> = cells(content)
        .into_iter()
        .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()))
        .map(|row| {
            let cols: Vec<String> = row.iter().map(|cell| render_latex(cell.trim())).collect();
            cols.join("  ")
        })
        .collect();

    if open.is_empty() {
        rendered_rows.join(" │ ")
    } else {
        format!("{open} {} {close}", rendered_rows.join(" │ "))
    }
}

/// A `{…}` group starting at `start`, rendered, and the index after it.
fn braced(chars: &[char], start: usize) -> Option<(String, usize)> {
    braced_raw(chars, start).map(|(inner, end)| (render_latex(&inner), end))
}

/// A `{…}` group starting at `start`, as written, and the index after it.
fn braced_raw(chars: &[char], start: usize) -> Option<(String, usize)> {
    if chars.get(start) != Some(&'{') {
        return None;
    }
    let mut depth = 0;
    let mut out = String::new();
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            // An escaped brace is text, not nesting: `\text{a\}b}`.
            '\\' => {
                out.push('\\');
                out.extend(chars.get(i + 1));
                i += 2;
                continue;
            }
            '{' => {
                depth += 1;
                if depth > 1 {
                    out.push('{');
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((out, i + 1));
                }
                out.push('}');
            }
            c => out.push(c),
        }
        i += 1;
    }
    None
}

fn superscript_char(ch: char) -> Option<char> {
    match ch {
        '0' => Some('⁰'), '1' => Some('¹'), '2' => Some('²'), '3' => Some('³'), '4' => Some('⁴'),
        '5' => Some('⁵'), '6' => Some('⁶'), '7' => Some('⁷'), '8' => Some('⁸'), '9' => Some('⁹'),
        '+' => Some('⁺'), '-' => Some('⁻'), '=' => Some('⁼'), '(' => Some('⁽'), ')' => Some('⁾'),
        'n' => Some('ⁿ'), 'i' => Some('ⁱ'), _ => None,
    }
}

fn subscript_char(ch: char) -> Option<char> {
    match ch {
        '0' => Some('₀'), '1' => Some('₁'), '2' => Some('₂'), '3' => Some('₃'), '4' => Some('₄'),
        '5' => Some('₅'), '6' => Some('₆'), '7' => Some('₇'), '8' => Some('₈'), '9' => Some('₉'),
        '+' => Some('₊'), '-' => Some('₋'), '=' => Some('₌'), '(' => Some('₍'), ')' => Some('₎'),
        'a' => Some('ₐ'), 'e' => Some('ₑ'), 'h' => Some('ₕ'), 'i' => Some('ᵢ'), 'j' => Some('ⱼ'),
        'k' => Some('ₖ'), 'l' => Some('ₗ'), 'm' => Some('ₘ'), 'n' => Some('ₙ'), 'o' => Some('ₒ'),
        'p' => Some('ₚ'), 'r' => Some('ᵣ'), 's' => Some('ₛ'), 't' => Some('ₜ'), 'u' => Some('ᵤ'),
        'v' => Some('ᵥ'), 'x' => Some('ₓ'), 'y' => Some('ᵧ'), _ => None,
    }
}

/// The bullet a list item at this indent wears: the two glyphs alternate by
/// nesting level, the way a printed list alternates • and ◦, so a sublist reads
/// as a different rank at a glance. `depth` is the item's indent in COLUMNS.
fn bullet_for(depth: u8, ctx: &ConcealCtx) -> String {
    let level = depth as usize / ctx.tab_width.max(1);
    if level.is_multiple_of(2) {
        ctx.glyphs.bullet.clone()
    } else {
        ctx.glyphs.bullet_alt.clone()
    }
}

/// Depth-colored indent guides (SPEC §5.4): each completed indent level trades
/// its first space for the guide glyph. One char for one char, so the conceal
/// map costs nothing in width and every column downstream stays put.
///
/// The glyph carries NO style of its own — `indent::apply` has already colored
/// those columns by depth in the styled spans, and a `plain` replacement
/// inherits what it lands on. That split is the whole reason the two halves of
/// the feature can live in different modules.
///
/// Only concealed lines get the guides, like every other conceal op: the line
/// under the cursor shows its source, spaces and all.
fn indent_guide_ops(chars: &[char], block: &BlockKind, ctx: &ConcealCtx, ops: &mut Vec<Conceal>) {
    if ctx.glyphs.indent_guide.is_empty() {
        return;
    }
    let line: String = chars.iter().collect();
    for (col, _depth) in indent::guides_for(&line, block, ctx.tab_width).marks {
        // Never overwrite a tab: replacing it with a 1-cell glyph would move
        // every column after it.
        if chars.get(col) == Some(&' ') {
            ops.push(Conceal::plain(col..col + 1, ctx.glyphs.indent_guide.clone()));
        }
    }
}

/// Block-level marker concealment, at the start of the line only — except for
/// the fence delimiters, which take the whole line.
fn block_ops(chars: &[char], block: &BlockKind, ctx: &ConcealCtx, ops: &mut Vec<Conceal>) {
    let len = chars.len();
    let glyphs = ctx.glyphs;
    match block {
        // `## ` vanishes; the heading text keeps its styling.
        BlockKind::Heading(_) => {
            let mut m = chars.iter().take_while(|c| **c == '#').count();
            while m < len && (chars[m] == ' ' || chars[m] == '\t') {
                m += 1;
            }
            if m > 0 {
                ops.push(Conceal::Hide(0..m));
            }
        }
        // The `>` run becomes ONE BAR PER LEVEL, so a nested quote still reads
        // as nested instead of collapsing to a single bar.
        BlockKind::Quote(depth) => {
            let m = chars
                .iter()
                .take_while(|c| matches!(**c, ' ' | '\t' | '>'))
                .count();
            if m > 0 {
                let bars = format!("{} ", glyphs.quote_bar).repeat((*depth as usize).max(1));
                ops.push(Conceal::Replace {
                    range: 0..m,
                    text: bars,
                    style: Some(Style::fg(ctx.theme.quote_bar)),
                });
            }
        }
        // `- ` / `* ` become a real bullet — one char for one char, so the text
        // keeps its column. An ORDINAL is content, not markup ("2." says which
        // item this is), so a numbered list keeps its number. A TASK concealsr
        // its whole `- [ ] ` prefix to one checkbox, which then stands in the
        // bullet column like any other marker. The leading whitespace picks up
        // its indent guides on the way past (SPEC §5.4).
        BlockKind::ListItem { marker, checked, depth } => {
            let indent = chars.iter().take_while(|c| **c == ' ' || **c == '\t').count();
            indent_guide_ops(chars, block, ctx, ops);

            // End of the marker itself: `-`, or `12.`.
            let mut p = indent;
            match marker {
                Marker::Ordered => {
                    while p < len && chars[p].is_ascii_digit() {
                        p += 1;
                    }
                    p += 1; // the `.` or `)`
                }
                _ => p += 1,
            }
            p = p.min(len);

            // A checkbox directly after the marker, if this is a task.
            let mut q = p;
            if q < len && (chars[q] == ' ' || chars[q] == '\t') {
                q += 1;
            }
            let box_end = (q + 3 <= len && chars[q] == '[').then_some(q + 3);

            match (checked, box_end) {
                // `- [x] ` -> `☑`, marker and all. One op for the whole prefix
                // so `leading_shift` can give the columns back on the active
                // line — two ops used to leave the text jumping by two.
                (Some(done), Some(end)) => {
                    let (glyph, color) = if *done {
                        (&glyphs.task_done, ctx.theme.task_done)
                    } else {
                        (&glyphs.task_todo, ctx.theme.list_bullet)
                    };
                    // An ordered task keeps its number (and the space after it)
                    // in front of the box.
                    let mut text: String = match marker {
                        Marker::Ordered => chars[indent..q].iter().collect(),
                        _ => String::new(),
                    };
                    text.push_str(glyph);
                    ops.push(Conceal::Replace {
                        range: indent..end,
                        text,
                        style: Some(Style::fg(color)),
                    });
                }
                // A plain bullet. `bullet = ""` blanks instead — blank, never
                // dropped, or the text would sit one column left of its
                // neighbours with nothing to explain why.
                (_, _) if !matches!(marker, Marker::Ordered) && p > indent => {
                    let bullet = bullet_for(*depth, ctx);
                    if bullet.is_empty() {
                        ops.push(Conceal::plain(indent..p, " ".into()));
                    } else {
                        ops.push(Conceal::Replace {
                            range: indent..p,
                            text: bullet,
                            style: Some(Style::fg(ctx.theme.list_bullet)),
                        });
                    }
                }
                _ => {}
            }
        }
        // ``` and ~~~ conceal to a rule across the measure: the code block gets
        // a visible top and bottom without its delimiters shouting. SPEC §5.2.
        // A thematic break (`---`) is the same idea and gets the same drawing.
        BlockKind::FenceOpen(_) | BlockKind::FenceClose | BlockKind::Rule if len > 0 => {
            let color = if matches!(block, BlockKind::Rule) {
                ctx.theme.rule
            } else {
                ctx.theme.fence_bar
            };
            ops.push(Conceal::Replace {
                range: 0..len,
                text: rule(&glyphs.rule, ctx.measure),
                style: Some(Style::fg(color)),
            });
        }
        BlockKind::MathBlock => {
            math_block_ops(chars, ops);
        }
        _ => {}
    }
}

/// One line of a display block: its `$$` delimiters hide and what is between
/// them conceals the way inline math does.
fn math_block_ops(chars: &[char], ops: &mut Vec<Conceal>) {
    let blank = |c: &&char| **c == ' ' || **c == '\t';
    let start = chars.iter().take_while(blank).count();
    let end = chars.len() - chars[start..].iter().rev().take_while(blank).count();
    let body = &chars[start..end];
    if body.is_empty() {
        return;
    }
    if body == ['$', '$'] {
        ops.push(Conceal::Hide(0..chars.len()));
        return;
    }
    // The opening `$$` hides with the spaces after it, the closing one with
    // the spaces before it; leading indentation stays on every line, so a
    // block under a list item stays under it.
    let opens = body.starts_with(&['$', '$']);
    let from = if opens {
        start + 2 + chars[start + 2..end].iter().take_while(blank).count()
    } else {
        start
    };
    let closes = end - from >= 2
        && chars[from..end].ends_with(&['$', '$'])
        && !crate::render::markdown::inline::is_escaped(chars, end - 2);
    let to = if closes { end - 2 } else { chars.len() };
    if opens {
        ops.push(Conceal::Hide(start..from));
    }
    if from < to {
        let inner: String = chars[from..to].iter().collect();
        ops.push(Conceal::Replace { range: from..to, text: render_latex(&inner), style: None });
    }
    if closes {
        ops.push(Conceal::Hide(to..chars.len()));
    }
}

/// A horizontal rule `width` cells wide, drawn from `glyph` (which may itself
/// be more than one cell).
fn rule(glyph: &str, width: u16) -> String {
    let unit = UnicodeWidthStr::width(glyph).max(1);
    glyph.repeat((width as usize / unit).max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::MarkdownConfig;
    use crate::render::markdown::block::{classify, Carry, Open};
    use crate::render::markdown::inline;

    /// Build a conceal map the way the frame will: classify the line, scan it,
    /// then derive the map.
    const MEASURE: u16 = 20;

    fn map(line: &str) -> ConcealMap {
        map_in(line, &Carry::default())
    }

    /// Build a conceal map the way the frame will: classify the line, scan it,
    /// then derive the map. `carry` sets the state entering the line, which is
    /// what tells a fence delimiter from a paragraph of backticks.
    fn map_in(line: &str, carry: &Carry) -> ConcealMap {
        let (block, _) = classify(line, carry, false, true);
        let cfg = MarkdownConfig { math: true, ..Default::default() };
        let spans = if block.allows_inline() {
            inline::scan(line, &cfg)
        } else {
            Vec::new()
        };
        let glyphs = GlyphConfig::default();
        let theme = Theme::default();
        ConcealMap::build(
            line,
            &block,
            &spans,
            &ConcealCtx {
                glyphs: &glyphs,
                theme: &theme,
                measure: MEASURE,
                tab_width: 2,
            },
        )
    }

    /// The concealed display text, ignoring styling.
    fn shown(line: &str) -> String {
        shown_in(line, &Carry::default())
    }

    fn shown_in(line: &str, carry: &Carry) -> String {
        let styled = vec![StyledSpan {
            range: 0..line.chars().count(),
            style: Style {
                fg: Color::None,
                bg: Color::None,
                attrs: Attrs::default(),
            },
        }];
        map_in(line, carry).render(line, &styled).0
    }

    #[test]
    fn inline_markers_vanish() {
        assert_eq!(shown("a **bold** b"), "a bold b");
        assert_eq!(shown("some *italic* here"), "some italic here");
        assert_eq!(shown("call `code` now"), "call code now");
        assert_eq!(shown("a [label](http://x) b"), "a label b");
        assert_eq!(shown("a [[Wiki]] b"), "a Wiki b");
        assert_eq!(shown("tag #foo bar"), "tag foo bar");
        assert_eq!(shown(r"an escaped \* star"), "an escaped * star");
    }

    #[test]
    fn inline_latex_renders_common_math() {
        assert_eq!(shown("flow ($\\rightarrow$ or $\\downarrow$)"), "flow (→ or ↓)");
        assert_eq!(shown("formula $x^2 + H_2O$"), "formula x² + H₂O");
        assert_eq!(shown("$\\alpha_1 + \\beta^2$"), "α₁ + β²");
        assert_eq!(shown("$\\frac{a}{b} + \\sqrt{x}$"), "a⁄b + √x");
        assert_eq!(
            shown("$$\\nabla \\cdot \\vec{E} = \\frac{Q}{\\varepsilon_0}$$"),
            "∇ · E⃗ = Q⁄(ε₀)"
        );
        assert_eq!(
            shown("$$ \\oint \\vec{E} \\cdot d\\vec{A} = \\frac{Q}{\\varepsilon_0} $$"),
            "∮ E⃗ · dA⃗ = Q⁄(ε₀)"
        );
        assert_eq!(
            shown("$$ \\nabla \\times \\vec{F} = \\begin{vmatrix} \\hat{i} & \\hat{j} & \\hat{k} \\\\ \\partial_x & \\partial_y & \\partial_z \\\\ F_x & F_y & F_z \\end{vmatrix} $$"),
            "∇ × F⃗ = | î  ĵ  k̂ │ ∂ₓ  ∂ᵧ  ∂_z │ Fₓ  Fᵧ  F_z |"
        );
        assert_eq!(shown("$\\frac{a+b}{c}$"), "(a+b)⁄c");
        assert_eq!(shown("$\\sqrt{x+1}$"), "√(x+1)");
        assert_eq!(shown("$\\sum_{i=1}^{n} i$"), "∑ᵢ₌₁ⁿ i");
        assert_eq!(shown("$e^{i\\pi}$"), "e^{iπ}");
        assert_eq!(shown("$\\left( x \\right)$"), "( x )");
        assert_eq!(shown("$\\phi \\neq \\varphi$"), "ϕ ≠ φ");
    }

    #[test]
    fn operators_unbraced_scripts_and_escapes_render() {
        // Operator names lose the backslash and keep apart from what follows.
        assert_eq!(shown("$\\sin\\theta$"), "sin θ");
        assert_eq!(shown("$\\log x$"), "log x");
        assert_eq!(shown("$\\lim_{x \\to 0}$"), "lim_{x → 0}");
        // An unbraced command is a script's whole argument.
        assert_eq!(shown("$e^\\pi$"), "e^π");
        assert_eq!(shown("$x_\\alpha$"), "x_α");
        assert_eq!(shown("$\\sum_\\infty$"), "∑_∞");
        // `*` is not part of a command name.
        assert_eq!(shown("$\\sigma*\\tau$"), "σ*τ");
        // Escaped specials are the character itself, in math and in text.
        assert_eq!(shown("$a \\& b$"), "a & b");
        assert_eq!(shown("$50\\%$"), "50%");
        assert_eq!(shown("$\\text{R\\&D}$"), "R&D");
        assert_eq!(shown("$\\|v\\|$"), "‖v‖");
    }

    #[test]
    fn operators_keep_apart_from_their_neighbours() {
        assert_eq!(shown(r"$a\sin x$"), "a sin x");
        assert_eq!(shown(r"$\sin(x)$"), "sin(x)");
        assert_eq!(shown(r"$\sin\left(x\right)$"), "sin(x)");
        assert_eq!(shown(r"$\sin\,x$"), "sin x");
        assert_eq!(shown(r"$\lim\limits_{x}$"), "limₓ");
        assert_eq!(shown(r"$\operatorname{sgn}x$"), "sgn x");
        assert_eq!(shown(r"$\operatorname*{arg\,max}_x$"), "arg maxₓ");
    }

    #[test]
    fn a_command_argument_takes_the_command_and_its_own_arguments() {
        assert_eq!(shown(r"$a_\text{max}$"), "aₘₐₓ");
        assert_eq!(shown(r"$F_\text{grav}$"), "F_grav");
        assert_eq!(shown(r"$e^\frac{1}{2}$"), "e^1⁄2");
        assert_eq!(shown(r"$\hat\theta$"), "θ\u{0302}");
        assert_eq!(shown(r"$\vec v$"), "v\u{20D7}");
        assert_eq!(shown(r"$\mathbf\Omega$"), "Ω");
    }

    #[test]
    fn half_typed_and_escaped_input_shows_what_is_there() {
        // An unclosed group adds no brace of its own.
        assert_eq!(shown(r"$x^{2$"), "x^{2");
        // An escaped `&` in a matrix is a character, not a column.
        assert_eq!(shown(r"$\begin{pmatrix} a \& b \end{pmatrix}$"), "( a & b )");
        // An escaped `$` is inside the math, not its end.
        assert_eq!(shown(r"$a\$b$"), "a$b");
    }

    #[test]
    fn math_block_lines_keep_their_indentation() {
        assert_eq!(shown_in(r"  \frac{a}{b}", &Carry::from(Open::InMath)), "  a⁄b");
        assert_eq!(shown_in(r"  x = 1 $$", &Carry::from(Open::InMath)), "  x = 1");
        assert_eq!(shown("  $$ x"), "  x");
    }

    #[test]
    fn a_matrix_closed_on_a_later_line_still_opens_here() {
        assert_eq!(shown("$$\\begin{pmatrix} a & b$$"), "( a    b");
    }

    #[test]
    fn multiline_math_block_renders() {
        assert_eq!(shown_in("$$", &Carry::default()), "");
        assert_eq!(
            shown_in("\\nabla \\cdot \\vec{B} = 0", &Carry::from(Open::InMath)),
            "∇ · B⃗ = 0"
        );
        assert_eq!(shown_in("$$", &Carry::from(Open::InMath)), "");
    }

    #[test]
    fn heading_marker_hidden() {
        assert_eq!(shown("## Title"), "Title");
        assert_eq!(shown("# A **bold** head"), "A bold head");
    }

    #[test]
    fn quote_becomes_bar() {
        // Default quote_bar glyph is `▎`.
        assert_eq!(shown("> quoted"), "▎ quoted");
    }

    /// A nested quote keeps its depth: one bar per level, not one bar total.
    #[test]
    fn nested_quotes_show_one_bar_per_level() {
        assert_eq!(shown("> > deep"), "▎ ▎ deep");
        assert_eq!(shown(">>> deeper"), "▎ ▎ ▎ deeper");
    }

    /// Fence delimiters conceal to a rule the width of the measure, so a code
    /// block reads as a block without its backticks shouting. SPEC §5.2.
    #[test]
    fn fence_delimiters_become_a_rule() {
        let open = shown("```rust");
        assert_eq!(UnicodeWidthStr::width(open.as_str()), MEASURE as usize);
        assert!(!open.contains('`'));
        // The closing fence needs the carry state that says we are inside one.
        let close = shown_in(
            "```",
            &Carry::from(Open::InFence { fence: "```".into(), lang: None, cont: Default::default() }),
        );
        assert_eq!(UnicodeWidthStr::width(close.as_str()), MEASURE as usize);
    }

    /// The bar and the box wear their own theme colors, not the dim marker
    /// style of the source characters they stand in for.
    #[test]
    fn replacement_glyphs_carry_their_own_color() {
        let theme = Theme::default();
        let styled = |line: &str| {
            let spans = vec![StyledSpan {
                range: 0..line.chars().count(),
                style: Style::fg(Color::None),
            }];
            map(line).render(line, &spans).1
        };
        assert_eq!(styled("> quoted")[0].style.fg, theme.quote_bar);
        // The tick is the line's first span now — it replaces `- [x] ` whole.
        assert_eq!(styled("- [x] done")[0].style.fg, theme.task_done);
    }

    /// Each shown char keeps the style of the span over its source char as
    /// `render` walks the sorted spans in step with the line: past a gap, an
    /// empty span, and a span that ends just before a shown char.
    #[test]
    fn rendered_chars_keep_their_source_style() {
        let line = "a **b** c **d** e";
        let st = |n: u8| Style::fg(Color::Indexed(n));
        let spans = vec![
            StyledSpan { range: 0..2, style: st(1) },
            StyledSpan { range: 3..3, style: st(9) },
            StyledSpan { range: 3..7, style: st(2) },
            StyledSpan { range: 10..17, style: st(3) },
        ];
        let (text, out) = map(line).render(line, &spans);
        assert_eq!(text, "a b c d e");
        // Display col -> source col, for this line's two hidden `**` pairs.
        let source = [0, 1, 4, 7, 8, 9, 12, 15, 16];
        for (disp, &src) in source.iter().enumerate() {
            let want = spans
                .iter()
                .find(|s| s.range.start <= src && src < s.range.end)
                .map_or(Color::None, |s| s.style.fg);
            let got = out.iter().find(|s| s.range.contains(&disp)).map(|s| s.style.fg);
            assert_eq!(got, Some(want), "display col {disp}");
        }
    }

    /// A list should LOOK like a list: `- ` and `* ` become a bullet, one char
    /// for one char, so the text stays in its content column either way.
    #[test]
    fn list_marker_becomes_a_bullet() {
        assert_eq!(shown("- item"), "• item");
        assert_eq!(shown("* item"), "• item");
        assert_eq!(shown("+ item"), "• item");
        // Sub-lists alternate to the hollow bullet (helper tab_width is 2).
        assert_eq!(shown("  - nested"), "│ ◦ nested");
        assert_eq!(shown("    - deeper"), "│ │ • deeper");
        // An ordinal is content, not markup — a numbered list keeps its number.
        assert_eq!(shown("1. item"), "1. item");
        assert_eq!(shown("12) item"), "12) item");
        // A task's checkbox IS its bullet, standing in the same column.
        assert_eq!(shown("- [ ] todo"), "☐ todo");
        // The marker swap is width-preserving. (A task box is not — `[ ]` is
        // three chars for one glyph — which is how it has always rendered.)
        for line in ["- item", "  - nested", "1. item"] {
            assert_eq!(shown(line).chars().count(), line.chars().count(), "{line:?}");
        }
    }

    /// Turning the glyph off falls back to the blank, never to a shifted line.
    #[test]
    fn an_empty_bullet_glyph_still_holds_the_column() {
        let glyphs = GlyphConfig {
            bullet: String::new(),
            ..GlyphConfig::default()
        };
        let theme = Theme::default();
        let map = ConcealMap::build(
            "- item",
            &BlockKind::ListItem { depth: 0, marker: Marker::Dash, checked: None },
            &[],
            &ConcealCtx { glyphs: &glyphs, theme: &theme, measure: MEASURE, tab_width: 2 },
        );
        assert_eq!(map.display_text("- item"), "  item");
    }

    /// SPEC §5.4: each completed indent level trades its first space for the
    /// guide glyph, one char for one char (the helper's tab_width is 2).
    #[test]
    fn indent_guides_replace_a_space_per_level() {
        assert_eq!(shown("    - nested"), "│ │ • nested");
        assert_eq!(shown("  - one level"), "│ ◦ one level");
        assert_eq!(shown("- flush"), "• flush", "no indent, no guides");
        // Width is preserved exactly, which is what keeps SPEC §2 true.
        for line in ["    - nested", "  - one level"] {
            assert_eq!(shown(line).chars().count(), line.chars().count());
        }
    }

    /// A tab is one char but many columns; swapping a 1-cell glyph in for it
    /// would shift the whole line, so tab-indented lines keep their whitespace.
    #[test]
    fn indent_guides_leave_tabs_alone() {
        assert_eq!(shown("\t\t- tabbed"), "\t\t◦ tabbed");
    }

    #[test]
    fn task_checkbox_becomes_a_box_or_tick() {
        // Default glyphs are the portable ballot boxes ☐ / ☑. The box replaces
        // the WHOLE `- [ ] ` prefix, so it stands in the bullet's own column
        // and a task lines up with its plain siblings.
        assert_eq!(shown("- [ ] todo"), "☐ todo");
        assert_eq!(shown("- [x] done"), "☑ done");
        assert_eq!(shown("* [ ] starred"), "☐ starred");
        // An ordered task keeps its number in front of the box.
        assert_eq!(shown("1. [ ] first"), "1. ☐ first");
    }

    /// The reveal shift the active line has to give back: `layout.stable_gutter`
    /// draws that line this many columns left, so the body text never moves as
    /// the cursor arrives. Every leading op counts toward it.
    #[test]
    fn leading_shift_covers_the_whole_marker_run() {
        let shift = |line: &str| map(line).leading_shift(line);
        assert_eq!(shift("## Title"), 3, "`## ` is hidden outright");
        assert_eq!(shift("- item"), 0, "a bullet swaps one char for one");
        assert_eq!(shift("> quoted"), 0, "`> ` becomes a bar of the same width");
        // `- [ ] ` (5 cells) down to one box.
        assert_eq!(shift("- [ ] todo"), 4);
        // Indent guides come first and cost nothing; the task prefix still counts.
        assert_eq!(shift("  - [ ] nested"), 4);
        // The run stops at the body: an inline marker further along the line
        // does not pull the text sideways.
        assert_eq!(shift("**bold** and more"), 2);
    }

    #[test]
    fn source_col_inverts_the_display() {
        // "## Title" conceals "## " -> display "Title".
        let line = "## Title";
        let m = map(line);
        assert_eq!(m.source_col(line, 0), 3); // display 'T' -> source col 3
        assert_eq!(m.source_col(line, 1), 4); // 'i' -> 4
        // "a **bold** b" -> display "a bold b"; display col 2 = 'b' of bold.
        let line = "a **bold** b";
        let m = map(line);
        assert_eq!(m.source_col(line, 0), 0); // 'a'
        assert_eq!(m.source_col(line, 2), 4); // 'b' of bold is source col 4
    }

    /// Wide characters are measured in CELLS, so a click past a CJK glyph lands
    /// on the character it looks like it landed on.
    #[test]
    fn source_col_measures_wide_characters() {
        // "**b** 世界x": display "b 世界x" — 世 and 界 are two cells each.
        let line = "**b** 世界x";
        let m = map(line);
        assert_eq!(m.source_col(line, 0), 2); // 'b'
        assert_eq!(m.source_col(line, 2), 6); // 世 starts at display col 2
        assert_eq!(m.source_col(line, 3), 6); // its second cell is still 世
        assert_eq!(m.source_col(line, 4), 7); // 界
        assert_eq!(m.source_col(line, 6), 8); // 'x'
    }

    #[test]
    fn ops_are_sorted_and_non_overlapping() {
        let m = map("## a **b** `c` [d](e) #f");
        let mut last = 0;
        for op in &m.ops {
            assert!(op.range().start >= last, "overlap in {:?}", m.ops);
            last = op.range().end;
        }
    }
}

// The conceal map for a line is cached alongside the rest of that line's parse
// in `render::cache::RenderCache`, which keeps ONE cache instead of several
// keyed the same way. The active set does not invalidate those entries: a
// concealed and a raw line are two views of one parse, so cursor movement only
// re-picks the view (SPEC.md §9 — a caveat the earlier design missed).
