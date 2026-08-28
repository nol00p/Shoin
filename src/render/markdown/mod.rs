//! Markdown styling. SPEC.md §5.
//!
//! Two passes:
//!   A. `block` — per-line classification, CACHED, incrementally invalidated.
//!   B. `inline` — per visible line, uncached, allocation-free in steady state.
//!
//! Bespoke scanner rather than `pulldown-cmark`: we style the visible viewport
//! only, must tolerate mid-edit invalid syntax, and must never reorder or drop
//! source text. See SPEC.md §5.1 for the full rationale.

pub mod block;
pub mod code;
pub mod inline;

use crate::config::schema::MarkdownConfig;
use crate::text::buffer::Syntax;

use super::theme::{Attrs, Color, Style, Theme};
use super::StyledSpan;
use block::{BlockKind, Marker};
use inline::InlineSpan;

/// `style_line` for callers that already scanned the line's inline spans.
///
/// The render cache needs those spans twice — once to style the body, once to
/// derive the conceal map (`outer - inner`) — and scanning is the expensive
/// half of building a display line, so it hands the one scan to both.
pub fn style_line_with(
    line: &str,
    block: BlockKind,
    syntax: Syntax,
    theme: &Theme,
    cfg: &MarkdownConfig,
    spans: &[InlineSpan],
) -> Vec<StyledSpan> {
    let len = line.chars().count();
    let base = Style {
        fg: theme.text,
        bg: Color::None,
        attrs: Attrs::default(),
    };

    // `.txt` and unstyled buffers get no Markdown styling at all.
    if syntax != Syntax::Markdown {
        return whole(len, base);
    }

    // Whole-line blocks that carry no inline content.
    match &block {
        // A fence body in a language shoin knows is the one place inside a
        // fence with structure worth showing. SPEC.md §5.3.
        BlockKind::FenceBody { lang: Some(lang), cont } if cfg.code_syntax => {
            return code_spans(line, len, *lang, *cont, theme)
        }
        BlockKind::FenceOpen(_) | BlockKind::FenceBody { .. } | BlockKind::FenceClose => {
            return whole(
                len,
                Style {
                    fg: theme.code,
                    bg: theme.code_bg,
                    attrs: Attrs::default(),
                },
            )
        }
        BlockKind::FrontMatter => return whole(len, marker_style(theme)),
        BlockKind::Rule => return whole(len, Style::fg(theme.rule)),
        // Cell separators recede so the CONTENT of a table reads as columns;
        // a delimiter row is all structure, so all of it recedes.
        BlockKind::Table => {
            let border = Style::fg(theme.table_border);
            if !line.chars().any(|c| c.is_alphanumeric()) {
                return whole(len, border);
            }
            let mut out: Vec<StyledSpan> = Vec::with_capacity(4);
            for (i, c) in line.chars().enumerate() {
                let style = if c == '|' { border } else { base };
                match out.last_mut() {
                    Some(last) if last.style == style => last.range.end = i + 1,
                    _ => out.push(StyledSpan {
                        range: i..i + 1,
                        style,
                    }),
                }
            }
            return out;
        }
        _ => {}
    }

    let (marker_end, body, ordinal) = block_body(line, &block, theme, base);

    let mut out = Vec::new();
    if marker_end > 0 {
        let end = marker_end.min(len);
        // An ORDINAL is content, not markup: `2.` says which item this is, and
        // it is the one marker concealment never hides. Dimming it the way a
        // `-` is dimmed made a numbered list genuinely hard to read — it wore
        // `text_dim` AND the dim attribute, dim on dim. It takes the bullet's
        // color instead, so `1.` reads exactly as loud as the `•` beside it.
        match ordinal.filter(|r| r.1 <= end) {
            Some((a, b)) => {
                if a > 0 {
                    out.push(StyledSpan { range: 0..a, style: marker_style(theme) });
                }
                out.push(StyledSpan { range: a..b, style: Style::fg(theme.list_bullet) });
                if b < end {
                    out.push(StyledSpan { range: b..end, style: marker_style(theme) });
                }
            }
            None => out.push(StyledSpan { range: 0..end, style: marker_style(theme) }),
        }
    }
    if len > marker_end {
        let body_base = StyledSpan {
            range: marker_end..len,
            style: body,
        };
        // Every kind reaching here allows inline styling of its body.
        out.extend(inline::to_styled(line, spans, &body_base, theme));
    }
    out
}

/// The marker-prefix length (char count), the body style, and — for an ordered
/// list — the char range of the ORDINAL itself, which is styled as content
/// rather than as markup. Headings/quotes/lists have a marker; paragraphs do
/// not.
type BlockBody = (usize, Style, Option<(usize, usize)>);

fn block_body(line: &str, block: &BlockKind, theme: &Theme, base: Style) -> BlockBody {
    match block {
        BlockKind::Heading(level) => {
            let chars: Vec<char> = line.chars().collect();
            let hashes = chars.iter().take_while(|c| **c == '#').count();
            let mut m = hashes;
            while m < chars.len() && (chars[m] == ' ' || chars[m] == '\t') {
                m += 1;
            }
            let color = theme.headings[(*level as usize).clamp(1, 6) - 1];
            let body = Style {
                fg: color,
                bg: Color::None,
                attrs: Attrs {
                    bold: theme.heading_bold,
                    italic: theme.heading_italic,
                    ..Attrs::default()
                },
            };
            (m, body, None)
        }
        BlockKind::Quote(_) => {
            let m = line
                .chars()
                .take_while(|c| matches!(*c, ' ' | '\t' | '>'))
                .count();
            (m, Style::fg(theme.quote), None)
        }
        BlockKind::ListItem { marker, checked, .. } => list_body(line, *marker, *checked, theme),
        // Paragraph, Blank — no marker, plain body.
        _ => (0, base, None),
    }
}

/// A highlighted fence body: the lexer's tokens as styled spans, with the code
/// slab's background under all of them and `theme.text` under whatever the
/// lexer left unclassified.
fn code_spans(line: &str, len: usize, lang: code::Lang, cont: code::Cont, theme: &Theme) -> Vec<StyledSpan> {
    let mut tokens = Vec::new();
    code::scan(line, lang, cont, &mut tokens);

    let style = |fg| Style { fg, bg: theme.code_bg, attrs: Attrs::default() };
    let base = style(theme.text);

    let mut out: Vec<StyledSpan> = Vec::with_capacity(tokens.len() * 2 + 1);
    let mut at = 0;
    for token in tokens {
        if token.range.start > at {
            out.push(StyledSpan { range: at..token.range.start, style: base });
        }
        let fg = match token.class {
            code::Class::Comment => theme.syntax_comment,
            code::Class::Str => theme.syntax_string,
            code::Class::Literal => theme.syntax_literal,
            code::Class::Keyword => theme.syntax_keyword,
            code::Class::Type => theme.syntax_type,
            code::Class::Function => theme.syntax_function,
            code::Class::Punct => theme.syntax_punct,
        };
        at = token.range.end;
        out.push(StyledSpan { range: token.range, style: style(fg) });
    }
    if at < len {
        out.push(StyledSpan { range: at..len, style: base });
    }
    out
}

/// One span over the whole line, or nothing for an empty line.
fn whole(len: usize, style: Style) -> Vec<StyledSpan> {
    if len == 0 {
        Vec::new()
    } else {
        vec![StyledSpan { range: 0..len, style }]
    }
}

/// The dimmed style markers wear on the active (raw) line. SPEC.md §5.2.
fn marker_style(theme: &Theme) -> Style {
    Style {
        fg: theme.text_dim,
        bg: Color::None,
        attrs: Attrs {
            dim: theme.dim_markers,
            ..Attrs::default()
        },
    }
}

/// Char length of a list item's marker prefix (indent + bullet + optional task
/// checkbox), and the style its body text takes.
fn list_body(line: &str, marker: Marker, checked: Option<bool>, theme: &Theme) -> BlockBody {
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let indent = chars.iter().take_while(|c| **c == ' ' || **c == '\t').count();
    let mut p = indent;

    // The ordinal's own range, reported so the caller can style it as content.
    let mut ordinal = None;
    match marker {
        Marker::Ordered => {
            while p < len && chars[p].is_ascii_digit() {
                p += 1;
            }
            p += 1; // the `.` or `)`
            ordinal = Some((indent, p.min(len)));
        }
        Marker::Dash | Marker::Star | Marker::Plus => p += 1,
    }
    if p < len && (chars[p] == ' ' || chars[p] == '\t') {
        p += 1;
    }
    if checked.is_some() && p + 2 < len && chars[p] == '[' {
        p += 3;
        if p < len && (chars[p] == ' ' || chars[p] == '\t') {
            p += 1;
        }
    }

    let body = if checked == Some(true) {
        Style {
            fg: theme.task_done,
            bg: Color::None,
            attrs: Attrs {
                strikethrough: true,
                ..Attrs::default()
            },
        }
    } else {
        Style::fg(theme.text)
    };
    (p.min(len), body, ordinal)
}
