//! Painting a frame. Pure: reads state, writes cells, mutates nothing.
//!
//! Build-order steps 1-3 render plain text. Markdown styling arrives in step 4
//! (block classification) and step 5 (inline), concealment in step 6 — all of
//! which slot in where the row `Span` is currently built raw.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, FlashKind};
use crate::finder::{Finder, VISIBLE_ROWS};
use crate::help::Help;
use crate::input::mode::{Mode, Prompt, PromptKind};
use crate::render::cache::RenderCache;
use crate::render::markdown::block::BlockKind;
use crate::render::splash;
use crate::render::focus::FocusRegion;
use crate::render::layout::{display_width, scroll_offset, Layout, VisualRow};
use crate::render::theme::Color as ThemeColor;
use crate::render::theme::Style as ThemeStyle;
use crate::render::StyledSpan;
use crate::text::cursor::Cursor;
use crate::tree::{Entry, FileTree, IconKind};

/// Rows the status line occupies when visible.
pub const STATUS_ROWS: u16 = 1;

pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Recomputed every frame: an overlay that closed must stop withholding the
    // pictures it was covering.
    app.overlay.set(None);
    app.images.borrow_mut().clear();
    let cfg = &app.config;

    // Paint the theme background across the whole surface so a theme is
    // self-contained: a light theme stays light on a dark terminal (and vice
    // versa), instead of drawing its text onto the terminal's own background.
    let bg = app.theme.background.to_ratatui();
    frame.render_widget(Block::default().style(Style::default().bg(bg)), area);

    // The help overlay takes over the whole surface when open.
    if let Some(help) = &app.help {
        render_help(frame, app, help, area);
        return;
    }

    // Split off a left column for the file tree, if open. It is chrome, not a
    // view onto a document, so it sits OUTSIDE the pane tree.
    let tw = tree_width(app, area);
    if let Some(tree) = &app.tree {
        let tree_rect = Rect { x: area.x, y: area.y, width: tw, height: area.height };
        render_tree(frame, app, tree, tree_rect);
    }
    let ea = Rect {
        x: area.x + tw,
        y: area.y,
        width: area.width.saturating_sub(tw),
        height: area.height,
    };

    // --- panes ---
    //
    // One rect per leaf of the layout tree. Each pane computes its own measure
    // and margins from its width, so a split is two narrower measures rather
    // than one measure clipped in half.
    let geo = app.layout.geometry(pane_area(app, area));
    let mut caret = None;
    for (id, rect) in &geo.panes {
        let Some(pane) = app.layout.pane(*id) else { continue };
        if let Some(pos) = render_pane(frame, app, pane, *rect, *id == app.focus_pane) {
            caret = Some(pos);
        }
    }
    for d in &geo.dividers {
        frame.render_widget(
            Block::default().style(
                Style::default()
                    .bg(app.theme.text_dim.to_ratatui())
                    .fg(app.theme.text_dim.to_ratatui()),
            ),
            *d,
        );
    }

    // --- status line ---
    //
    // One for the whole window, not one per pane: it is a transient flash, and
    // repeating it beside itself would be noise.
    if cfg.status.enabled {
        if let Some(status) = status_text(app) {
            let style = match app.flash.as_ref().map(|f| &f.kind) {
                Some(FlashKind::Error) => Style::default().fg(Color::Red),
                _ => Style::default().add_modifier(Modifier::DIM),
            };
            let lay = Layout::compute(&cfg.layout, ea.width, ea.height, STATUS_ROWS);
            let status_area = Rect {
                x: ea.x + lay.margin_left,
                y: ea.y + ea.height.saturating_sub(STATUS_ROWS),
                width: lay.measure,
                height: STATUS_ROWS,
            };
            // Elide rather than let ratatui cut the tail off silently. A
            // clipped message looks like a finished one, and the half that
            // gets lost is the end — which for an error is the half that says
            // what to do about it.
            let status = elide(&status, lay.measure as usize);
            let line = Line::from(Span::styled(status, style)).right_aligned();
            frame.render_widget(Paragraph::new(line), status_area);
        }
    }

    // --- caret ---
    //
    // The finder overlay outranks everything — it takes all input while it is
    // open. Otherwise: the tree owns the caret when it has focus (set in
    // `render_tree`), Command and Search modes put it in the spotlight box, and
    // failing all that it sits where the focused pane put it.
    let tree_focused = app.tree.as_ref().is_some_and(|t| t.focused);
    if let Some(finder) = &app.finder {
        render_finder(frame, app, finder, ea);
    } else if let Mode::Prompt(p) = &app.mode {
        // Above the tree, which is what asked the question.
        let prompt = prompt_label(app, p);
        render_command_box(frame, app, p.kind.title(), &prompt, &p.input, ea);
    } else if tree_focused {
        // caret set by render_tree
    } else if let Mode::Command(buf) = &app.mode {
        render_command_box(frame, app, " command ", ":", buf, ea);
    } else if let Mode::Search { query, reverse } = &app.mode {
        let prompt = if *reverse { "?" } else { "/" };
        render_command_box(frame, app, " search ", prompt, query, ea);
    } else if let Some(pos) = caret {
        frame.set_cursor_position(pos);
    }
}

/// The area the pane tree divides: the frame, less the tree sidebar and the row
/// the status line reserves. `App` asks for this too, so click routing and the
/// screen motions use the same geometry the renderer did.
pub fn pane_area(app: &App, area: Rect) -> Rect {
    let tw = tree_width(app, area);
    let reserved = if app.config.status.enabled { STATUS_ROWS } else { 0 };
    Rect {
        x: area.x + tw,
        y: area.y,
        width: area.width.saturating_sub(tw),
        height: area.height.saturating_sub(reserved),
    }
}

/// Draw one pane, returning where the caret goes if this is the focused one.
///
/// Concealment (build-order step 6). Each source line is turned into its
/// DISPLAY form: the cursor's line (and, in Visual, the whole selection)
/// renders raw 1:1; every other line has its markers hidden. Then:
///   active set (known) → wrap on display width → scroll offset
/// in that order, so a line re-expanding its markers as the cursor lands on it
/// never scrolls itself off screen (SPEC.md §6, "Reveal shift").
///
/// The parse and the wrap come from the document's cache; only the rows
/// actually on screen are turned into styled spans.
fn render_pane(
    frame: &mut Frame,
    app: &App,
    pane: &crate::render::pane::Pane,
    rect: Rect,
    focused: bool,
) -> Option<Position> {
    let cfg = &app.config;
    let bg = app.theme.background.to_ratatui();
    let doc = app.docs.get(pane.doc)?;

    // Nothing opened and nothing typed: draw the start screen instead of an
    // empty measure, and return no caret — there is nothing yet to point at,
    // and a block sitting on the artwork would be the only thing in the room.
    // The first keystroke makes `splash::active` false and the pane renders
    // normally from the very same frame.
    if splash::active(app) {
        splash::render(frame, app, rect);
        return None;
    }

    // The status line already has its row reserved out of `pane_area`.
    let lay = Layout::compute(&cfg.layout, rect.width, rect.height, 0);

    let mut cache = doc.cache.borrow_mut();
    cache.sync_doc(app, doc, lay.measure);

    // The focused pane follows the document's live cursor; every other pane
    // anchors on its OWN, clamped — an edit in another pane can have moved the
    // text out from under a saved position.
    let cursor = if focused {
        doc.buffer.cursor
    } else {
        let line = pane.cursor.line.min(doc.buffer.line_count().saturating_sub(1));
        Cursor::new(line, pane.cursor.col.min(doc.buffer.line_len(line)))
    };
    let (cursor_row, cursor_offset) = cache.locate(cursor.line, cursor.col);
    let top = scroll_offset(
        cursor_row,
        cache.total_rows(),
        &lay,
        &cfg.layout,
        cfg.editor.scroll_off,
        pane.scroll,
    );

    let text_area = Rect {
        x: rect.x + lay.margin_left,
        y: rect.y + lay.top,
        width: lay.measure,
        height: lay.height,
    };

    // Rows of one line are contiguous, so one materialized DisplayLine serves
    // every row it wrapped into. Each row is drawn on its own so the active
    // line can hang its revealed markers into the reserved gutter.
    let mut current: Option<(usize, DisplayLine)> = None;
    for (i, row) in (top..top + lay.height as usize)
        .filter_map(|r| cache.row(r))
        .enumerate()
    {
        // Line spacing (SPEC.md §6): a row with no line behind it. It is
        // painted rather than skipped so it takes the pane's background —
        // skipping it would leave whatever the last frame drew there — and it
        // deliberately gets no fence slab or bar, because `spacer_rows` never
        // puts one inside a fence.
        if row.is_spacer() {
            let r = Rect { x: text_area.x, y: text_area.y + i as u16, width: lay.measure, height: 1 };
            frame.render_widget(Paragraph::new("").style(Style::default().bg(bg)), r);
            current = None;
            continue;
        }

        // A transcluded row has no text in this rope, so it is drawn from the
        // expansion the cache captured rather than from a `DisplayLine`.
        if let Some(index) = row.embedded() {
            let r = Rect {
                x: text_area.x,
                y: text_area.y + i as u16,
                width: lay.measure,
                height: 1,
            };
            render_embed_row(frame, app, &cache, row.line(), index, r, bg);
            current = None;
            continue;
        }
        if current.as_ref().is_none_or(|(l, _)| *l != row.line()) {
            current = Some((row.line(), display_line_of(app, doc, &cache, row.line(), focused)));
        }
        let d = &current.as_ref().unwrap().1;
        let line = Line::from(window_spans(&d.text, &d.spans, row.start_col, row.end_col));
        let shift = gutter_shift(app, &cache, &lay, row.line(), row.start_col);
        // A wrapped list continuation is pushed right to sit under the item's
        // content; the cache already wrapped it at the narrower width (SPEC §6).
        let hang = hang_x(app, &cache, &lay, &row);
        let r = Rect {
            x: text_area.x - shift + hang,
            y: text_area.y + i as u16,
            width: lay.measure + shift - hang,
            height: 1,
        };
        // A fenced row's slab is painted by the ROW, not by its spans: the
        // spans stop where the code does, and SPEC §5.3 asks for a flat
        // background across the measure (SPEC §5.3).
        let fenced = in_fence(&cache, row.line());
        let row_bg = if fenced { app.theme.code_bg.to_ratatui() } else { bg };
        frame.render_widget(Paragraph::new(line).style(Style::default().bg(row_bg)), r);
        if fenced {
            render_fence_bar(frame, app, r);
        }
    }

    render_scroll_hint(frame, app, rect, &text_area, top, cache.total_rows());

    if !focused {
        return None;
    }
    let screen_row = cursor_row.saturating_sub(top);
    if screen_row >= lay.height as usize {
        return None;
    }
    let (row_start, hang) = cache
        .row(cursor_row)
        .map(|r| (r.start_col, hang_x(app, &cache, &lay, &r)))
        .unwrap_or((0, 0));
    let prefix: String = doc
        .buffer
        .line_text(cursor.line)
        .chars()
        .skip(row_start)
        .take(cursor_offset)
        .collect();
    let shift = gutter_shift(app, &cache, &lay, cursor.line, row_start);
    let x = (text_area.x + display_width(&prefix) + hang)
        .saturating_sub(shift)
        .min(rect.right().saturating_sub(1));
    Some(Position::new(x, text_area.y + screen_row as u16))
}

/// Draw one row of an expanded `![[…]]` (SPEC.md §14.3).
///
/// The border and the label wear `theme.text_dim` so the embed reads as a
/// quotation rather than as this document's own text — the reader needs to know
/// at a glance which words they can edit. An unresolved embed wears
/// `theme.error`, per §14.2.
fn render_embed_row(
    frame: &mut Frame,
    app: &App,
    cache: &RenderCache,
    line: usize,
    index: usize,
    rect: Rect,
    bg: ratatui::style::Color,
) {
    use crate::transclude::preview::Row;
    let Some(row) = cache
        .entry(line)
        .and_then(|e| e.embed.as_ref())
        .and_then(|x| x.rows.get(index))
    else {
        return;
    };
    let theme = &app.theme;
    let dim = ThemeStyle::fg(theme.text_dim).to_ratatui();
    let width = rect.width as usize;

    // A picture's box is drawn as an OUTLINE when the terminal cannot show
    // pixels, and as plain spaces when it can — there, the outline would only
    // be something for the image to cover up.
    let pixels = app.image_protocol != crate::image::Protocol::None;
    let spans: Vec<Span> = match row {
        Row::Image(pic) => {
            app.images.borrow_mut().push(crate::render::Placement {
                x: rect.x,
                y: rect.y,
                cols: pic.cols,
                rows: pic.rows,
                line,
                index,
            });
            if pixels {
                vec![Span::raw(" ".repeat(pic.cols as usize))]
            } else {
                let w = (pic.cols as usize).min(width).max(2);
                vec![Span::styled(
                    format!("\u{256d}{}\u{256e}", "\u{2500}".repeat(w - 2)),
                    dim,
                )]
            }
        }
        Row::Reserved => {
            if pixels {
                vec![Span::raw("")]
            } else {
                // The last reserved row closes the box, so the outline needs to
                // know where it ends — the expansion says.
                let (w, last) = cache
                    .entry(line)
                    .and_then(|e| e.embed.as_ref())
                    .map(|x| {
                        let w = match x.rows.first() {
                            Some(Row::Image(p)) => (p.cols as usize).min(width).max(2),
                            _ => width.max(2),
                        };
                        let last = !matches!(x.rows.get(index + 1), Some(Row::Reserved));
                        (w, last)
                    })
                    .unwrap_or((width.max(2), true));
                let (l, r) = if last { ('\u{2570}', '\u{256f}') } else { ('\u{2502}', '\u{2502}') };
                let fill = if last { "\u{2500}".repeat(w - 2) } else { " ".repeat(w - 2) };
                vec![Span::styled(format!("{l}{fill}{r}"), dim)]
            }
        }
        Row::Caption(text) => vec![Span::styled(elide(text, width), dim)],
        Row::Top(label) => {
            // `\u{256d} `, ` `, at least one rule cell, and `\u{256e}` are the fixed cost; a
            // label longer than what is left is elided rather than allowed to
            // push the closing corner off the screen.
            let room = width.saturating_sub(5);
            let label = elide(label, room);
            let head = format!("\u{256d} {label} ");
            let rule = "\u{2500}".repeat(width.saturating_sub(display_width(&head) as usize + 1));
            vec![Span::styled(format!("{head}{rule}\u{256e}"), dim)]
        }
        Row::Bottom => vec![Span::styled(
            format!(
                "\u{2570}{}\u{256f}",
                "\u{2500}".repeat(width.saturating_sub(2))
            ),
            dim,
        )],
        Row::Error(msg) => vec![Span::styled(
            msg.clone(),
            ThemeStyle::fg(theme.error).to_ratatui(),
        )],
        Row::Body(text, block) => {
            // Styled AND CONCEALED with the document's own rules. An embedded
            // line is never the active line — it is not in this rope at all —
            // so by SPEC §2 it always shows its finished form, and an embedded
            // `# Heading` must read as a heading rather than as its source.
            let md = &app.config.markdown;
            let inline = crate::render::markdown::inline::scan(text, md);
            let styled = crate::render::markdown::style_line_with(
                text,
                block.clone(),
                crate::text::buffer::Syntax::Markdown,
                theme,
                md,
                &inline,
            );
            let ctx = crate::render::conceal::ConcealCtx {
                glyphs: &app.config.glyphs,
                theme,
                measure: width as u16,
                tab_width: app.config.editor.tab_width,
            };
            let (shown, spans) = if app.config.layout.conceal {
                crate::render::conceal::ConcealMap::build(text, block, &inline, &ctx)
                    .render(text, &styled)
            } else {
                (text.clone(), styled)
            };
            let bordered = app.config.transclude.border && app.embed_mode.framed();
            let mut out = if bordered {
                vec![Span::styled("\u{2502} ".to_string(), dim)]
            } else {
                Vec::new()
            };
            out.extend(window_spans(&shown, &spans, 0, shown.chars().count()));
            // Pad to the closing bar so the box has a straight right edge. The
            // content was wrapped to fit between the bars, so this only ever
            // adds space — but it is measured in display cells, not chars, so a
            // CJK line does not push the edge out.
            if bordered {
                let used = 2 + display_width(&shown) as usize;
                out.push(Span::styled(
                    " ".repeat(width.saturating_sub(used + 2)) + " \u{2502}",
                    dim,
                ));
            }
            out
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
        rect,
    );
}

/// Shorten `s` to at most `cells` display columns, marking the cut with `…`.
///
/// Counts DISPLAY width, not characters, so a CJK label is elided at the right
/// place rather than one that merely has the right number of chars.
fn elide(s: &str, cells: usize) -> String {
    if display_width(s) as usize <= cells {
        return s.to_string();
    }
    if cells <= 1 {
        return "\u{2026}".to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for c in s.chars() {
        let cw = display_width(&c.to_string()) as usize;
        if w + cw > cells - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('\u{2026}');
    out
}

/// A continuation row's hanging indent, measured from the TEXT COLUMN.
///
/// `VisualRow::hanging` is the item's content column measured from the LINE's
/// own left edge — and a revealed line's left edge is `gutter_shift` columns to
/// the left of the text column. Subtracting that shift is what makes a wrapped
/// task line up under its own text instead of four columns right of it.
fn hang_x(app: &App, cache: &RenderCache, lay: &Layout, row: &VisualRow) -> u16 {
    if row.hanging == 0 {
        return 0;
    }
    let line_shift = gutter_shift(app, cache, lay, row.line(), 0);
    row.hanging.saturating_sub(line_shift).min(lay.measure)
}

/// Whether this line belongs to a fenced code block — its delimiters included,
/// so the slab and the bar run the block's whole height rather than stopping a
/// row short at each end.
fn in_fence(cache: &RenderCache, line: usize) -> bool {
    cache.entry(line).is_some_and(|e| {
        matches!(
            e.block,
            BlockKind::FenceOpen(_) | BlockKind::FenceBody { .. } | BlockKind::FenceClose
        )
    })
}

/// The colored left gutter bar beside a fenced row (SPEC.md §5.3).
///
/// It hangs in the LEFT MARGIN, one column outside the measure, for the same
/// reason `stable_gutter` puts revealed markers there: a bar inside the measure
/// would cost the code a column and move it relative to the prose around it.
/// Nothing is concealed to make room, so it needs no conceal map and the active
/// line keeps its 1:1 source mapping.
fn render_fence_bar(frame: &mut Frame, app: &App, row: Rect) {
    let glyph = &app.config.glyphs.fence_bar;
    if glyph.is_empty() || row.x == 0 {
        return;
    }
    let style = Style::default()
        .fg(app.theme.fence_bar.to_ratatui())
        .bg(app.theme.background.to_ratatui());
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(glyph.clone(), style))),
        Rect { x: row.x - 1, y: row.y, width: 1, height: 1 },
    );
}

/// The 1-column vertical position indicator in the right margin (SPEC.md §6).
///
/// Not a scrollbar: no trough, no arrows, and it is ABSENT whenever the whole
/// document fits on screen — there is nothing to convey then, and a permanent
/// rail is exactly the chrome SPEC §1 rules out. It appears only once the text
/// overflows, which makes it read as information rather than furniture.
///
/// It sits at the pane's own right edge, so in a split each pane answers for
/// itself, and the thumb covers the visible fraction of the document.
fn render_scroll_hint(
    frame: &mut Frame,
    app: &App,
    rect: Rect,
    text_area: &Rect,
    top: usize,
    total: usize,
) {
    let glyph = &app.config.glyphs.scroll_hint;
    let height = text_area.height as usize;
    if !app.config.layout.scroll_hint || glyph.is_empty() || total <= height || height == 0 {
        return;
    }
    // A wide pane has margin to spare; a pane narrowed until the margin is gone
    // would have the hint sitting on the text, so it yields instead.
    if rect.right() <= text_area.right() {
        return;
    }

    // Thumb length is the visible fraction, floored at one row so a very long
    // document still shows something. Its offset spreads the remaining rows
    // over the scrollable range, so the last screenful lands flush at the
    // bottom instead of a row short.
    let len = ((height * height) / total).max(1).min(height);
    let span = total - height; // non-zero: the overflow test above guarantees it
    let offset = (top.min(span) * (height - len) + span / 2) / span;

    let style = Style::default().fg(app.theme.text_dim.to_ratatui());
    let thumb: Vec<Line> = (0..len)
        .map(|_| Line::from(Span::styled(glyph.clone(), style)))
        .collect();
    frame.render_widget(
        Paragraph::new(thumb),
        Rect {
            x: rect.right() - 1,
            y: text_area.y + offset as u16,
            width: 1,
            height: len as u16,
        },
    );
}

/// Columns to hang a revealed line's markers into, left of the text column.
///
/// `layout.stable_gutter`: the active line renders raw, so `## ` reappears and
/// would push its own text three columns right. Drawing that line shifted left
/// by exactly the width the markers took back keeps the BODY text in the same
/// column whether the line is active or not — zero horizontal jitter as the
/// cursor moves through a document (SPEC.md §6, "Reveal shift").
///
/// Only the first row of a line shifts: a wrapped continuation carries no
/// markers, so it already sits where it belongs.
fn gutter_shift(app: &App, cache: &RenderCache, lay: &Layout, line: usize, start_col: usize) -> u16 {
    if !app.config.layout.stable_gutter
        || !cache.conceal_on()
        || cache.is_concealed(line)
        || start_col != 0
    {
        return 0;
    }
    let Some(entry) = cache.entry(line) else {
        return 0;
    };
    entry.cmap.leading_shift(&entry.source).min(lay.margin_left)
}

/// The tree pane width for this frame (0 when closed), capped to half the width.
fn tree_width(app: &App, area: Rect) -> u16 {
    if app.tree.is_some() {
        crate::tree::WIDTH.min(area.width / 2)
    } else {
        0
    }
}

/// Draw the file-tree pane: a scrollable list with a right separator, the
/// selected row highlighted, and (when focused) the caret on that row.
fn render_tree(frame: &mut Frame, app: &App, tree: &FileTree, rect: Rect) {
    let theme = &app.theme;
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(theme.text_dim.to_ratatui()))
        .style(Style::default().bg(theme.background.to_ratatui()));
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let h = inner.height as usize;
    let len = tree.entries.len();
    let start = if len <= h {
        0
    } else {
        tree.selected.saturating_sub(h / 2).min(len - h)
    };
    let width = inner.width as usize;

    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for (i, e) in tree.entries.iter().enumerate().skip(start).take(h) {
        lines.push(tree_row(app, tree, e, i == tree.selected, width));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.background.to_ratatui())),
        inner,
    );

    if tree.focused {
        let row = tree.selected.saturating_sub(start);
        frame.set_cursor_position(Position::new(inner.x, inner.y + row as u16));
    }
}

/// One tree row: `│  ├╴  name`, Neo-tree style — the guide columns its
/// ancestors still occupy, an elbow into this entry, its icon, and the name.
///
/// Everything is padded out to the pane width so the selected row's background
/// covers it end to end. Names too long for the pane are cut, not wrapped: a
/// tree row is one line by definition.
fn tree_row<'a>(app: &App, tree: &FileTree, e: &Entry, selected: bool, width: usize) -> Line<'a> {
    let theme = &app.theme;
    let glyphs = &app.config.glyphs;
    let nerd = glyphs.nerd_fonts;

    let sel_bg = |s: Style| {
        if selected {
            s.bg(theme.selection.to_ratatui())
        } else {
            s
        }
    };

    // --- the guide columns, then the elbow into this row ---
    let mut prefix = String::new();
    for &running in &e.guides {
        prefix.push_str(if running { "│ " } else { "  " });
    }
    if e.depth > 0 {
        prefix.push_str(if e.last { "└╴" } else { "├╴" });
    }

    // --- the icon: open/closed folder, or the file type's own glyph ---
    let (icon, icon_color) = if e.is_dir {
        let open = tree.is_expanded(&e.path);
        let glyph = match (nerd, open) {
            (true, true) => glyphs.folder_open.as_str(),
            (true, false) => glyphs.folder.as_str(),
            (false, true) => "▾",
            (false, false) => "▸",
        };
        (glyph.to_string(), theme.link)
    } else if nerd {
        let (glyph, kind) = crate::tree::file_icon(&e.name);
        (glyph.to_string(), icon_color(theme, kind))
    } else {
        // Without a Nerd Font a file gets no glyph, but still needs the column
        // the sibling directories' arrows take, or the names zigzag.
        (" ".to_string(), theme.text_dim)
    };

    let mut name_style = Style::default().fg(if e.is_dir { theme.link } else { theme.text }.to_ratatui());
    if e.is_dir {
        name_style = name_style.add_modifier(Modifier::BOLD);
    }

    // --- assemble, truncating the NAME (never the guides) to fit ---
    let fixed = prefix.chars().count() + icon.chars().count() + 1;
    let room = width.saturating_sub(fixed);
    let name: String = if e.name.chars().count() > room {
        e.name.chars().take(room).collect()
    } else {
        e.name.clone()
    };
    let pad = width.saturating_sub(fixed + name.chars().count());

    Line::from(vec![
        Span::styled(
            prefix,
            sel_bg(Style::default().fg(theme.text_dim.to_ratatui()).add_modifier(Modifier::DIM)),
        ),
        Span::styled(icon, sel_bg(Style::default().fg(icon_color.to_ratatui()))),
        Span::styled(" ", sel_bg(Style::default())),
        Span::styled(name, sel_bg(name_style)),
        Span::styled(" ".repeat(pad), sel_bg(Style::default())),
    ])
}

/// Draw the `:help` overlay: a near-full-screen bordered panel with the scrolled
/// help text and a hint in the title bar.
fn render_help(frame: &mut Frame, app: &App, help: &Help, area: Rect) {
    let theme = &app.theme;
    let margin = 2u16;
    let rect = Rect {
        x: area.x + margin,
        y: area.y + margin.min(area.height / 4),
        width: area.width.saturating_sub(margin * 2).max(10),
        height: area.height.saturating_sub(margin * 2).max(3),
    };

    let accent = theme.link.to_ratatui();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(theme.background.to_ratatui()))
        .title(Span::styled(
            format!(" help: {} ", help.title),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                " j/k scroll · q close ",
                Style::default().fg(theme.text_dim.to_ratatui()),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect);

    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let view_h = inner.height as usize;
    let max_scroll = help.lines.len().saturating_sub(view_h);
    let scroll = help.scroll.min(max_scroll) as u16;
    let lines: Vec<Line> = help
        .lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(theme.text.to_ratatui()))))
        .collect();
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
}

/// The first and last buffer lines visible IN THE FOCUSED PANE, recomputing the
/// same geometry the renderer uses. Drives the `H`/`M`/`L` screen motions.
pub fn visible_line_range(app: &App, area: Rect) -> Option<(usize, usize)> {
    let cfg = &app.config;
    let rect = focused_rect(app, area)?;
    let lay = Layout::compute(&cfg.layout, rect.width, rect.height, 0);

    let mut cache = app.cache.borrow_mut();
    cache.sync(app, lay.measure);

    let (cursor_row, _) = cache.locate(app.buffer.cursor.line, app.buffer.cursor.col);
    let top = scroll_offset(
        cursor_row,
        cache.total_rows(),
        &lay,
        &cfg.layout,
        cfg.editor.scroll_off,
        app.scroll_hint(),
    );
    let first = cache.row(top)?.line();
    let last_row = (top + lay.height as usize)
        .min(cache.total_rows())
        .saturating_sub(1);
    let last = cache.row(last_row)?.line();
    Some((first, last))
}

/// The rect the focused pane occupies.
fn focused_rect(app: &App, area: Rect) -> Option<Rect> {
    app.layout
        .geometry(pane_area(app, area))
        .panes
        .into_iter()
        .find(|(id, _)| *id == app.focus_pane)
        .map(|(_, r)| r)
}

/// Which pane a screen position falls in, if any.
pub fn pane_at(app: &App, area: Rect, col: u16, row: u16) -> Option<crate::render::pane::PaneId> {
    app.layout
        .geometry(pane_area(app, area))
        .panes
        .into_iter()
        .find(|(_, r)| col >= r.x && col < r.right() && row >= r.y && row < r.bottom())
        .map(|(id, _)| id)
}

/// Map a screen position (a mouse click) back to a buffer `Cursor`, recomputing
/// the same geometry the renderer used. Returns `None` when the click is outside
/// the text area. Inverts the clicked line's conceal map so a click on a
/// concealed line lands on the right source column.
///
/// The caller is expected to have focused the clicked pane first (`pane_at`),
/// so this reads the focused pane's geometry and document.
pub fn locate_click(app: &App, area: Rect, col: u16, row: u16) -> Option<Cursor> {
    let cfg = &app.config;
    let rect = focused_rect(app, area)?;
    // A click outside the pane — in the tree sidebar, say — is not a position.
    if col < rect.x || col >= rect.right() || row < rect.y || row >= rect.bottom() {
        return None;
    }
    let ex = rect.x;
    let lay = Layout::compute(&cfg.layout, rect.width, rect.height, 0);

    let mut cache = app.cache.borrow_mut();
    cache.sync(app, lay.measure);

    let (cursor_row, _) = cache.locate(app.buffer.cursor.line, app.buffer.cursor.col);
    let top = scroll_offset(
        cursor_row,
        cache.total_rows(),
        &lay,
        &cfg.layout,
        cfg.editor.scroll_off,
        app.scroll_hint(),
    );

    if row < rect.y + lay.top {
        return None;
    }
    let screen_row = (row - rect.y - lay.top) as usize;
    if screen_row >= lay.height as usize {
        return None;
    }
    let vr = cache.row(top + screen_row)?;
    let line = vr.line();

    // A click on transcluded content has no column to land on — that text is
    // in another file. It selects the `![[…]]` line that produced it, which is
    // the only thing here the reader can actually edit.
    if !vr.editable {
        return Some(Cursor::new(line, 0));
    }

    // The active line may hang its markers into the gutter, so it starts that
    // many columns to the left of the text column.
    let shift = gutter_shift(app, &cache, &lay, line, vr.start_col);
    let click_x = (col + shift).saturating_sub(ex + lay.margin_left + hang_x(app, &cache, &lay, &vr));
    let display_col = vr.start_col as u16 + click_x;

    let entry = cache.entry(line)?;
    let len = entry.source.chars().count();
    let src_col = if cache.is_concealed(line) {
        entry.cmap.source_col(&entry.source, display_col).min(len)
    } else {
        (display_col as usize).min(len)
    };

    Some(Cursor::new(line, src_col))
}

/// A centered floating input for `:` commands — the spotlight box. Drawn last,
/// over a `Clear`ed rect, owning no source↔screen mapping, so it never touches
/// the concealment machinery. SPEC.md §6 (command entry).
fn render_command_box(
    frame: &mut Frame,
    app: &App,
    title: &str,
    prompt: &str,
    buf: &str,
    area: Rect,
) {
    let theme = &app.theme;

    // Wide enough for what it has to say. A `:` box is 60 columns; a prompt
    // asking "delete notes/ and 12 entries? (y/N)" needs its own question to
    // fit, or the thing being deleted scrolls out of view.
    let wanted = 60.max(display_width(prompt) + display_width(buf) + 4);
    let width = wanted.min(area.width.saturating_sub(4)).max(20);
    let height = 3u16;
    let x = area.x + area.width.saturating_sub(width) / 2;
    // Upper third — spotlight placement, clear of the text the user was editing.
    let y = area.y + (area.height / 3).min(area.height.saturating_sub(height));
    let rect = Rect { x, y, width, height };
    app.overlay.set(Some(rect));

    let accent = spotlight_accent(app);
    let block = spotlight_block(app, accent, title);
    let inner = block.inner(rect);

    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let line = Line::from(vec![
        Span::styled(prompt.to_string(), Style::default().fg(accent.to_ratatui())),
        Span::styled(
            buf.to_string(),
            Style::default()
                .fg(theme.text.to_ratatui())
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), inner);

    // Caret after the typed text, clamped inside the box. Measured from the
    // prompt's own width — it is one cell for `:` and a whole question for a
    // tree prompt.
    let cx = (inner.x + display_width(prompt) + display_width(buf))
        .min(inner.x + inner.width.saturating_sub(1));
    frame.set_cursor_position(Position::new(cx, inner.y));
}

/// What a tree prompt asks, in words. The path is shown RELATIVE to the tree
/// root — an absolute path in a 60-column box is mostly directories the reader
/// already knows they are in.
fn prompt_label(app: &App, p: &Prompt) -> String {
    let shown = |path: &std::path::Path| {
        app.tree
            .as_ref()
            .map(|t| t.relative(path))
            .unwrap_or_else(|| path.display().to_string())
    };
    match &p.kind {
        PromptKind::Create => format!("{}/", shown(&p.target)),
        PromptKind::Rename => "name: ".to_string(),
        PromptKind::Move => "to: ".to_string(),
        PromptKind::Export { format } => format!("{} to: ", format.name()),
        PromptKind::Delete { entries } => {
            let what = shown(&p.target);
            // The count is the point: `d` on a directory row takes everything
            // under it, and the number is the only warning of that.
            if *entries > 1 {
                format!("delete {what} and {} entries inside? (y/N) ", entries - 1)
            } else {
                format!("delete {what}? (y/N) ")
            }
        }
    }
}

/// A spotlight box is the focus while it's open, so it renders bright — an
/// accent border and title against the dimmed buffer behind it — turning to the
/// error color when a flash is up.
fn spotlight_accent(app: &App) -> ThemeColor {
    match app.flash.as_ref().map(|f| &f.kind) {
        Some(FlashKind::Error) => app.theme.error,
        _ => app.theme.link,
    }
}

fn spotlight_block<'a>(app: &App, accent: ThemeColor, title: &'a str) -> Block<'a> {
    let bold_accent = Style::default()
        .fg(accent.to_ratatui())
        .add_modifier(Modifier::BOLD);
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(bold_accent)
        .style(Style::default().bg(app.theme.background.to_ratatui()))
        .title(Span::styled(title, bold_accent))
}

/// The fuzzy finder overlay (`<leader>ff`): the spotlight box with the query on
/// its first row and the matching paths listed under it, matched characters
/// picked out in the accent color. Like the command box it is drawn last over a
/// `Clear`ed rect and owns no source↔screen mapping.
fn render_finder(frame: &mut Frame, app: &App, finder: &Finder, area: Rect) {
    let theme = &app.theme;
    let accent = spotlight_accent(app);
    let width = 60u16.min(area.width.saturating_sub(4)).max(20);
    let inner_w = width.saturating_sub(2) as usize;

    // --- query row: `> typed`, with the match count right-aligned ---
    let typed = format!("> {}", finder.query);
    let count = format!(
        "{}/{}{}",
        finder.matches.len(),
        finder.file_count(),
        if finder.truncated { "+" } else { "" }
    );
    let gap = inner_w.saturating_sub(typed.chars().count() + count.chars().count());
    let mut lines = vec![Line::from(vec![
        Span::styled("> ", Style::default().fg(accent.to_ratatui())),
        Span::styled(
            finder.query.clone(),
            Style::default()
                .fg(theme.text.to_ratatui())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" ".repeat(gap)),
        Span::styled(
            count,
            Style::default()
                .fg(theme.text_dim.to_ratatui())
                .add_modifier(Modifier::DIM),
        ),
    ])];

    // --- result rows, scrolled to keep the selection in view ---
    let len = finder.matches.len();
    let shown = len.min(VISIBLE_ROWS);
    let start = if len <= VISIBLE_ROWS {
        0
    } else {
        finder.selected.saturating_sub(VISIBLE_ROWS / 2).min(len - VISIBLE_ROWS)
    };
    for i in start..start + shown {
        let Some((rel, positions)) = finder.row(i) else {
            continue;
        };
        lines.push(result_line(app, rel, positions, i == finder.selected, inner_w));
    }
    if len == 0 {
        lines.push(Line::from(Span::styled(
            "  no matching files",
            Style::default()
                .fg(theme.text_dim.to_ratatui())
                .add_modifier(Modifier::DIM),
        )));
    }

    let height = (lines.len() as u16 + 2).min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    // Upper third, like the command box — the results grow downward from there.
    let y = area.y + (area.height / 3).min(area.height.saturating_sub(height));
    let rect = Rect { x, y, width, height };
    app.overlay.set(Some(rect));

    let block = spotlight_block(app, accent, " find ");
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(lines), inner);

    let cx = (inner.x + 2 + display_width(&finder.query)).min(inner.x + inner.width.saturating_sub(1));
    frame.set_cursor_position(Position::new(cx, inner.y));
}

/// One result row: the path padded to the box width (so the selection's
/// background covers the whole row), with the query's matched characters in the
/// accent color. A path too long for the box loses its HEAD, not its tail —
/// the file name is the part you are reading.
fn result_line<'a>(
    app: &App,
    rel: &str,
    positions: &[usize],
    selected: bool,
    width: usize,
) -> Line<'a> {
    let theme = &app.theme;
    let accent = spotlight_accent(app);

    let base = if selected {
        Style::default()
            .fg(theme.text.to_ratatui())
            .bg(theme.selection.to_ratatui())
    } else {
        Style::default().fg(theme.text_dim.to_ratatui())
    };
    let hit = base.fg(accent.to_ratatui()).add_modifier(Modifier::BOLD);

    // Marker, then the same file-type icon the tree draws — read from the FILE
    // NAME, not the whole relative path, or a dotted directory would decide it.
    let mut spans = vec![Span::styled(if selected { "▸ " } else { "  " }, base)];
    let mut lead = 2;
    if app.config.glyphs.nerd_fonts {
        let name = rel.rsplit_once('/').map(|(_, n)| n).unwrap_or(rel);
        let (glyph, kind) = crate::tree::file_icon(name);
        spans.push(Span::styled(
            format!("{glyph} "),
            base.fg(icon_color(theme, kind).to_ratatui()),
        ));
        lead += 2;
    }

    // The path fills what is left; too long, and it loses its head, not the
    // file name at the end.
    let avail = width.saturating_sub(lead);
    let chars: Vec<char> = rel.chars().collect();
    let (skipped, elided) = if chars.len() > avail && avail > 1 {
        (chars.len() - (avail - 1), true)
    } else {
        (0, false)
    };
    if elided {
        spans.push(Span::styled("…", base));
    }
    for (i, c) in chars.iter().enumerate().skip(skipped) {
        let style = if positions.contains(&i) { hit } else { base };
        spans.push(Span::styled(c.to_string(), style));
    }
    let drawn = lead + usize::from(elided) + (chars.len() - skipped);
    spans.push(Span::styled(" ".repeat(width.saturating_sub(drawn)), base));
    Line::from(spans)
}

/// The theme color a file-type icon wears. Shared by the tree and the finder so
/// one file reads the same in both.
fn icon_color(theme: &crate::render::theme::Theme, kind: IconKind) -> ThemeColor {
    match kind {
        IconKind::Prose => theme.headings[0],
        IconKind::Code => theme.code,
        IconKind::Data => theme.tag,
        IconKind::Media => theme.wiki_link,
        IconKind::Plain => theme.text_dim,
    }
}


/// One source line's rendered form: the display string plus styled spans over
/// it. For an active or conceal-disabled line the text equals the source; for a
/// concealed line the markers are gone and the spans are re-indexed onto the
/// shorter string.
struct DisplayLine {
    text: String,
    spans: Vec<StyledSpan>,
}

/// Turn one cached line into its display form for THIS frame: the parse comes
/// from the cache, and everything frame-local — focus dim, the command-box dim,
/// search matches, the Visual selection — is overlaid on a copy of it.
///
/// The overlays are applied in SOURCE coordinates, before concealment remaps
/// them, which is what lets a search match inside `**bold**` land on the right
/// display column with no coordinate arithmetic here.
fn display_line_of(
    app: &App,
    doc: &crate::app::BufferState,
    cache: &RenderCache,
    line: usize,
    focused: bool,
) -> DisplayLine {
    // Overlays that follow the CURSOR (focus dim, the Visual selection) apply
    // only in the focused pane; ones that follow the TEXT (search matches)
    // apply wherever that text is shown. `doc` is what tells the two apart when
    // the same document is open in several panes.
    let _ = doc;
    let Some(entry) = cache.entry(line) else {
        return DisplayLine {
            text: String::new(),
            spans: Vec::new(),
        };
    };
    let source = &entry.source;
    let mut styled = entry.styled.clone();

    // Focus mode dims everything outside the current paragraph/sentence. It
    // follows the CURSOR, so it belongs to the focused pane only. Applied in
    // SOURCE coordinates so concealment's remap carries it through, and to both
    // active and concealed lines alike. SPEC.md §6.
    if let Some(region) = app.focus_region.as_ref().filter(|_| focused) {
        apply_focus_dim(&mut styled, source.chars().count(), region, line);
    }

    // While a spotlight box is open — command, search, or the finder — dim the
    // whole buffer behind it so the box reads as the focus of attention.
    if matches!(app.mode, Mode::Command(_) | Mode::Search { .. }) || app.finder.is_some() {
        overlay_dim(&mut styled, 0..source.chars().count());
    }

    // A search match is a property of the TEXT, so it lights up in every pane
    // showing this document.
    // Highlight every search match on this line (source coords, pre-conceal).
    if let Some(search) = &app.search {
        let needle: Vec<char> = search.pattern.chars().collect();
        let hay: Vec<char> = source.chars().collect();
        let mut i = 0;
        while let Some(pos) = find_sub(&hay, &needle, i) {
            overlay_bg(&mut styled, pos..pos + needle.len(), app.theme.search_match);
            i = pos + needle.len();
        }
    }

    // Raw 1:1 when the line is active, when the user turned concealment off, or
    // for non-Markdown buffers — the SPEC §2 invariant plus its escape hatch.
    // The cache settled that when it built this frame's row index.
    if !cache.is_concealed(line) {
        // A Visual selection paints its background over the raw text.
        if let Some((s, e)) = selection_cols(app, line, source.chars().count()).filter(|_| focused) {
            overlay_bg(&mut styled, s..e, app.theme.selection);
        }
        return DisplayLine {
            text: source.clone(),
            spans: styled,
        };
    }

    let (text, spans) = entry.cmap.render(source, &styled);
    DisplayLine { text, spans }
}

/// The selected char columns on `line`, if a Visual selection covers it.
/// VisualLine selects the whole line; Visual is inclusive of the cursor char.
fn selection_cols(app: &App, line: usize, len: usize) -> Option<(usize, usize)> {
    if !matches!(app.mode, Mode::Visual | Mode::VisualLine) {
        return None;
    }
    let anchor = app.anchor?;
    let cursor = app.buffer.cursor;
    let (lo, hi) = if (anchor.line, anchor.col) <= (cursor.line, cursor.col) {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    };
    if line < lo.line || line > hi.line {
        return None;
    }
    if matches!(app.mode, Mode::VisualLine) {
        return Some((0, len));
    }
    let start = if line == lo.line { lo.col } else { 0 };
    let end = if line == hi.line { hi.col + 1 } else { len };
    Some((start.min(len), end.min(len)))
}

/// Repaint the background of `range` (char columns) across the line's spans,
/// splitting spans at the boundaries. Keeps the cover gap-free.
fn overlay_bg(spans: &mut Vec<StyledSpan>, range: std::ops::Range<usize>, bg: ThemeColor) {
    overlay(spans, range, |style| style.bg = bg);
}

/// De-emphasize `range` with the faint attribute, KEEPING its colors — so focus
/// mode dims a paragraph's intensity without flattening its syntax highlighting.
fn overlay_dim(spans: &mut Vec<StyledSpan>, range: std::ops::Range<usize>) {
    overlay(spans, range, |style| style.attrs.dim = true);
}

/// Apply a style tweak to the char columns in `range`, splitting spans at the
/// boundaries so the cover stays gap-free and non-overlapping.
fn overlay(
    spans: &mut Vec<StyledSpan>,
    range: std::ops::Range<usize>,
    tweak: impl Fn(&mut crate::render::theme::Style),
) {
    if range.start >= range.end {
        return;
    }
    let mut out = Vec::with_capacity(spans.len() + 2);
    for sp in spans.drain(..) {
        let (s, e) = (sp.range.start, sp.range.end);
        let a = range.start.max(s);
        let b = range.end.min(e);
        if a >= b {
            out.push(sp);
            continue;
        }
        if s < a {
            out.push(StyledSpan { range: s..a, style: sp.style });
        }
        let mut mid = sp.style;
        tweak(&mut mid);
        out.push(StyledSpan { range: a..b, style: mid });
        if b < e {
            out.push(StyledSpan { range: b..e, style: sp.style });
        }
    }
    *spans = out;
}

/// First index at or after `from` where `needle` occurs in `hay` (empty needle
/// never matches).
fn find_sub(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() || from > hay.len() - needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == needle[..])
}

/// Faintly de-emphasize the source columns of `line` that fall outside the
/// focus region, keeping their colors.
fn apply_focus_dim(spans: &mut Vec<StyledSpan>, len: usize, region: &FocusRegion, line: usize) {
    if line < region.start.line || line > region.end.line {
        overlay_dim(spans, 0..len);
        return;
    }
    let keep_start = if line == region.start.line { region.start.col } else { 0 };
    let keep_end = if line == region.end.line { region.end.col.min(len) } else { len };
    if keep_start > 0 {
        overlay_dim(spans, 0..keep_start.min(len));
    }
    if keep_end < len {
        overlay_dim(spans, keep_end..len);
    }
}

/// Clip a line's styled spans to one visual row's char window and lower them to
/// ratatui spans, pulling display text from the source. Spans arrive ordered,
/// gap-free, and non-overlapping, so the clip preserves those properties.
fn window_spans(source: &str, styled: &[StyledSpan], start: usize, end: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = source.chars().collect();
    let end = end.min(chars.len());
    let mut out = Vec::new();
    for sp in styled {
        let s = sp.range.start.max(start);
        let e = sp.range.end.min(end);
        if s >= e {
            continue;
        }
        let text: String = chars[s..e].iter().collect();
        out.push(Span::styled(text, sp.style.to_ratatui()));
    }
    out
}

/// The status line is hidden by default and appears only transiently — after a
/// save, a mode change, or an error — then fades. Command entry has its own
/// spotlight box (`render_command_box`), so it is not echoed here. SPEC.md §6.
fn status_text(app: &App) -> Option<String> {
    if matches!(app.mode, Mode::Command(_) | Mode::Search { .. }) || app.finder.is_some() {
        return None;
    }

    let flash = app.flash.as_ref()?;
    if let Some(text) = flash.text.as_ref() {
        return Some(text.clone());
    }

    let mut parts: Vec<String> = Vec::new();
    for item in &app.config.status.show {
        match item.as_str() {
            "file" => parts.push(app.buffer.display_name()),
            "modified" if app.buffer.modified => parts.push(app.config.glyphs.modified.clone()),
            "position" => parts.push(format!(
                "{}:{}",
                app.buffer.cursor.line + 1,
                app.buffer.cursor.col + 1
            )),
            "words" => parts.push(format!("{} words", app.buffer.word_count())),
            _ => {}
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("  ·  "))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::config::Config;
    use crate::render::markdown::block::BlockCache;
    use crate::render::theme::Color;
    use crate::text::cursor::Cursor;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn app_with(text: &str) -> App {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.buffer.insert_str(Cursor::new(0, 0), text);
        app.blocks = BlockCache::build(&app.buffer);
        app
    }

    fn render_to(app: &App, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// The rows of a drawn frame, trimmed — what the reader actually sees.
    fn rows(buf: &ratatui::buffer::Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// Highlighting survives the whole draw path — the conceal render, the
    /// window into the row, the slab painted under it — and not just the cache.
    #[test]
    fn a_highlighted_fence_reaches_the_screen() {
        let app = app_with("```rust\nlet s = \"hi\";\n```\n");
        let buf = render_to(&app, 80, 12);

        let row = (0..buf.area.height)
            .find(|y| (0..buf.area.width).any(|x| buf[(x, *y)].symbol() == "l"))
            .expect("the body line is on screen");
        let colors: Vec<ratatui::style::Color> = (0..buf.area.width)
            .map(|x| &buf[(x, row)])
            .filter(|c| !c.symbol().trim().is_empty())
            .map(|c| c.fg)
            .collect();

        assert!(colors.contains(&app.theme.syntax_keyword.to_ratatui()), "`let` is a keyword");
        assert!(colors.contains(&app.theme.syntax_string.to_ratatui()), "`\"hi\"` is a string");
        // The slab runs the full measure regardless.
        let bg = app.theme.code_bg.to_ratatui();
        assert!((0..buf.area.width).filter(|x| buf[(*x, row)].bg == bg).count() >= 40);
    }

    /// Line spacing reaches the SCREEN: a blank row between the lines of a
    /// paragraph, and no second one after the blank line that separates two.
    #[test]
    fn line_spacing_draws_air_between_lines() {
        let mut app = app_with("alpha\nbravo\n\ncharlie\n");
        app.config.layout.line_spacing = 1;
        let drawn = rows(&render_to(&app, 60, 16));
        let row_of = |word: &str| drawn.iter().position(|r| r.trim() == word).expect(word);

        // One row of air inside the paragraph, two at the paragraph break — the
        // blank line's own row plus the spacing of the line above it. That is
        // the whole argument for suppressing spacing after a blank line: the
        // break stays bigger than the line gap without being twice it.
        assert_eq!(row_of("bravo") - row_of("alpha"), 2);
        assert_eq!(row_of("charlie") - row_of("bravo"), 3);
    }

    /// The whole flow paints without panicking and applies heading color.
    #[test]
    fn heading_line_gets_heading_style() {
        let app = app_with("# Title\n\nbody text\n");
        let buf = render_to(&app, 80, 12);

        let heading = app.theme.headings[0].to_ratatui();
        let painted_heading = buf.content().iter().any(|c| {
            c.symbol().chars().any(|ch| ch.is_alphabetic()) && c.fg == heading
        });
        assert!(painted_heading, "expected a heading-colored glyph on screen");
    }

    /// A `#` marker is dimmed, distinct from its heading body.
    #[test]
    fn heading_marker_is_dimmed() {
        let app = app_with("# Title\n");
        let buf = render_to(&app, 80, 6);
        let dim = Color::None; // marker fg is text_dim, never the terminal default
        let hash = buf
            .content()
            .iter()
            .find(|c| c.symbol() == "#")
            .expect("hash marker should be on screen");
        assert_eq!(hash.fg, app.theme.text_dim.to_ratatui());
        assert_ne!(hash.fg, dim.to_ratatui());
    }

    fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area().width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// An ORDINAL is content, not markup — it is the one list marker
    /// concealment never hides, so dimming it the way a `-` is dimmed left a
    /// numbered list hard to read. It wears the bullet's color instead.
    #[test]
    fn an_ordinal_reads_as_loud_as_a_bullet() {
        // The cursor sits on line 0, so every list line below renders concealed.
        let mut app = app_with("intro\n\n- bulleted\n1. numbered\n2. second\n10. ten\n");

        let bullet = app.theme.list_bullet.to_ratatui();
        let dim = app.theme.text_dim.to_ratatui();
        let check = |buf: &ratatui::buffer::Buffer, ch: &str| {
            let pos = (0..buf.area().height)
                .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
                .find(|p| buf[*p].symbol() == ch)
                .unwrap_or_else(|| panic!("{ch:?} should be on screen"));
            let cell = &buf[pos];
            assert_eq!(cell.fg, bullet, "{ch:?} should wear the bullet color");
            assert_ne!(cell.fg, dim, "{ch:?} should not be a dimmed marker");
            assert!(
                !cell.modifier.contains(Modifier::DIM),
                "{ch:?} should not carry the DIM attribute either"
            );
        };

        let buf = render_to(&app, 60, 24);
        for ch in ["1", "2", "0", "."] {
            check(&buf, ch);
        }
        // …and it matches the bullet it sits beside.
        assert_eq!(buf[(0..buf.area().height)
            .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
            .find(|p| buf[*p].symbol() == "\u{2022}")
            .expect("a concealed bullet")]
            .fg, bullet);

        // The ACTIVE line renders raw, and the ordinal must hold up there too —
        // that is the line being edited, and the one the reader is looking at.
        app.buffer.cursor = Cursor::new(3, 0);
        app.active = crate::render::conceal::ActiveSet { start: 3, end: 3 };
        let buf = render_to(&app, 60, 24);
        for ch in ["1", "."] {
            check(&buf, ch);
        }
    }

    /// A scratch vault, and an app editing its composition file.
    fn composition(body: &str, frag: &str) -> (std::path::PathBuf, App) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shoin-fx-{t}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("frag.md"), frag).unwrap();
        let note = d.join("note.md");
        std::fs::write(&note, body).unwrap();

        let mut app = App::new(Config::default(), Some(note), None).unwrap();
        app.embed_mode = crate::transclude::Mode::Short;
        app.refresh_blocks();
        (d, app)
    }

    /// SPEC §14.3: an `![[…]]` line draws its target in place…
    #[test]
    fn an_embed_expands_where_it_sits() {
        let (d, app) = composition("top\n\n![[frag]]\n\ntail\n", "# Frag\n\nfragment body\n");
        let rows: Vec<String> = (0..24).map(|y| row_text(&render_to(&app, 80, 24), y)).collect();
        let all = rows.join("\n");

        assert!(all.contains("fragment body"), "the target is drawn");
        assert!(all.contains("frag"), "…under its label");
        assert!(!all.contains("![["), "and its source is not");
        // The composition's own text still surrounds it, in order.
        let top = rows.iter().position(|r| r.contains("top")).unwrap();
        let body = rows.iter().position(|r| r.contains("fragment body")).unwrap();
        let tail = rows.iter().position(|r| r.contains("tail")).unwrap();
        assert!(top < body && body < tail);
        std::fs::remove_dir_all(&d).ok();
    }

    /// …and shows its RAW source when the cursor is on it, exactly as
    /// `**bold**` does. This is the §2 invariant applied to embeds, and it is
    /// what keeps the cursor from ever being inside transcluded text.
    #[test]
    fn the_active_embed_line_shows_its_source() {
        let (d, mut app) = composition("top\n\n![[frag]]\n", "fragment body\n");
        app.buffer.cursor = Cursor::new(2, 0);
        app.active = crate::render::conceal::ActiveSet { start: 2, end: 2 };

        let buf = render_to(&app, 80, 24);
        let all: String = (0..24).map(|y| row_text(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(all.contains("![[frag]]"), "the cursor's line is raw");
        assert!(!all.contains("fragment body"), "so it is not expanded");
        std::fs::remove_dir_all(&d).ok();
    }

    /// Embedded markdown is never the active line, so it always shows its
    /// finished form — an embedded `# Heading` reads as a heading.
    #[test]
    fn embedded_markdown_is_concealed_like_any_other_line() {
        // The embed must not be on line 0 — that is where the cursor starts,
        // and the active line renders raw by §2.
        let (d, app) = composition("top\n![[frag]]\n", "# Frag title\n\nwith **bold** in it\n");
        let buf = render_to(&app, 80, 24);
        let all: String = (0..24).map(|y| row_text(&buf, y)).collect::<Vec<_>>().join("\n");

        assert!(all.contains("Frag title"));
        assert!(!all.contains("# Frag title"), "the heading marker is concealed");
        assert!(all.contains("bold") && !all.contains("**bold**"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// Turning preview off puts the source back, and the row count with it.
    #[test]
    fn embed_preview_is_a_toggle() {
        let (d, mut app) = composition("top\n![[frag]]\n", "alpha\nbeta\ngamma\n");
        let rows_on = {
            let mut c = app.cache.borrow_mut();
            c.sync(&app, 40);
            c.total_rows()
        };
        app.embed_mode = crate::transclude::Mode::Off;
        let rows_off = {
            let mut c = app.cache.borrow_mut();
            c.sync(&app, 40);
            c.total_rows()
        };
        assert!(rows_on > rows_off, "expanding adds rows: {rows_on} vs {rows_off}");

        let all: String = (0..24)
            .map(|y| row_text(&render_to(&app, 80, 24), y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!all.contains("alpha"), "the target is not drawn with preview off");
        // The line is inactive, so it still CONCEALS its `[[ ]]` markers like
        // any other wiki link — off means "not expanded", not "not styled".
        assert!(all.contains("frag"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// A status message too long for the measure is ELIDED, not silently cut.
    /// The end is the half that gets lost, and for an error that is the half
    /// saying what to do about it — so the reader has to be able to see that
    /// something was dropped.
    #[test]
    fn an_overlong_status_says_it_was_cut() {
        let mut app = app_with("hi\n");
        // Set the flash directly: `notify` is private, and what is under test
        // is the RENDERING of an over-long one, not how it got there.
        app.flash = Some(crate::app::Flash {
            text: Some("x".repeat(400)),
            kind: crate::app::FlashKind::Error,
            expires_at: std::time::Instant::now() + std::time::Duration::from_secs(60),
        });

        let buf = render_to(&app, 60, 10);
        let rows: Vec<String> = (0..10).map(|y| row_text(&buf, y)).collect();
        let status = rows
            .iter()
            .find(|r| r.contains('x'))
            .expect("the message is on screen");
        assert!(status.contains('\u{2026}'), "elided: {status:?}");
        let lay = Layout::compute(&app.config.layout, 60, 10 - STATUS_ROWS, 0);
        assert!(
            display_width(status.trim()) as usize <= lay.measure as usize,
            "and inside the measure: {status:?}"
        );
    }

    /// `transclude.border` promises a "labelled box", and a box has four sides.
    /// It used to have three: the rules ran to the measure and the body rows
    /// stopped wherever their text did, which read as a broken frame.
    #[test]
    fn the_embed_box_closes_on_every_row() {
        let (d, app) = composition(
            "top\n![[frag]]\n",
            "# Frag\n\nshort\n\na much longer line of prose that has to wrap at least once\n",
        );
        let buf = render_to(&app, 80, 24);
        let lay = Layout::compute(&app.config.layout, 80, 24 - STATUS_ROWS, 0);
        let right = lay.margin_left + lay.measure - 1;
        let left = lay.margin_left;

        let mut corners = (0, 0);
        let mut bars = 0;
        for y in 0..24 {
            let l = buf[(left, y)].symbol().to_string();
            let r = buf[(right, y)].symbol().to_string();
            match l.as_str() {
                "\u{256d}" => {
                    corners.0 += 1;
                    assert_eq!(r, "\u{256e}", "the top rule must close");
                }
                "\u{2570}" => {
                    corners.1 += 1;
                    assert_eq!(r, "\u{256f}", "the bottom rule must close");
                }
                "\u{2502}" => {
                    bars += 1;
                    assert_eq!(r, "\u{2502}", "every body row must reach the right bar");
                }
                _ => {}
            }
        }
        assert_eq!(corners, (1, 1), "one box");
        assert!(bars >= 4, "and its body rows, got {bars}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A label longer than the box elides rather than pushing the corner off.
    #[test]
    fn a_long_label_is_elided_not_overflowed() {
        // `composition` writes `frag.md`; give it the long name too, so the
        // link resolves before the first parse rather than after it.
        let (d, app) = composition(
            "top\n![[a-name-far-too-long-to-fit-inside-a-narrow-measure-at-all]]\n",
            "body\n",
        );
        std::fs::copy(
            d.join("frag.md"),
            d.join("a-name-far-too-long-to-fit-inside-a-narrow-measure-at-all.md"),
        )
        .unwrap();

        let buf = render_to(&app, 50, 20);
        let lay = Layout::compute(&app.config.layout, 50, 20 - STATUS_ROWS, 0);
        let right = lay.margin_left + lay.measure - 1;
        let top = (0..20)
            .find(|y| buf[(lay.margin_left, *y)].symbol() == "\u{256d}")
            .expect("a box");
        assert_eq!(buf[(right, top)].symbol(), "\u{256e}", "the corner survives");
        let text: String = (0..50).map(|x| buf[(x, top)].symbol()).collect();
        assert!(text.contains('\u{2026}'), "the label is elided: {text:?}");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A click on transcluded text selects the line that produced it — there is
    /// no column in this rope to land on.
    #[test]
    fn clicking_an_embed_selects_its_source_line() {
        let (d, app) = composition("top\n![[frag]]\n", "aaa\nbbb\n");
        let area = Rect { x: 0, y: 0, width: 80, height: 24 };
        let _ = render_to(&app, 80, 24);

        let rows: Vec<String> = (0..24).map(|y| row_text(&render_to(&app, 80, 24), y)).collect();
        let y = rows.iter().position(|r| r.contains("bbb")).unwrap() as u16;
        let got = locate_click(&app, area, 40, y).expect("a position");
        assert_eq!(got.line, 1, "the `![[frag]]` line");
        assert_eq!(got.col, 0);
        std::fs::remove_dir_all(&d).ok();
    }

    /// SPEC §6: a wrapped list item's continuation lines up under the item's
    /// content, not back at the left margin.
    #[test]
    fn a_wrapped_list_item_hangs_under_its_content() {
        let long = "- ".to_string() + &"word ".repeat(30);
        let mut app = app_with(&(long + "\n"));
        app.config.layout.measure = 30;
        let buf = render_to(&app, 60, 24);

        let rows: Vec<String> = (0..24).map(|y| row_text(&buf, y)).collect();
        let first = rows.iter().position(|r| r.contains("word")).unwrap();
        let indent = |s: &str| s.len() - s.trim_start().len();
        // The bullet's content column, and where the next row starts.
        let content = rows[first].find("word").unwrap();
        assert_eq!(indent(&rows[first + 1]), content, "continuation hangs");
        assert!(
            rows[first + 1].trim_start().starts_with("word"),
            "…and it is the item's own text"
        );
    }

    /// Prose must NOT hang — there is no content column to line up under.
    #[test]
    fn a_wrapped_paragraph_does_not_hang() {
        let mut app = app_with(&("word ".repeat(30) + "\n"));
        app.config.layout.measure = 30;
        let buf = render_to(&app, 60, 24);
        let rows: Vec<String> = (0..24).map(|y| row_text(&buf, y)).collect();
        let first = rows.iter().position(|r| r.contains("word")).unwrap();
        let indent = |s: &str| s.len() - s.trim_start().len();
        assert_eq!(indent(&rows[first]), indent(&rows[first + 1]));
    }

    /// `hanging_indent = false` goes back to a flush continuation.
    #[test]
    fn hanging_indent_honors_its_setting() {
        let long = "- ".to_string() + &"word ".repeat(30);
        let mut app = app_with(&(long + "\n"));
        app.config.layout.measure = 30;
        app.config.layout.hanging_indent = false;
        let buf = render_to(&app, 60, 24);
        let rows: Vec<String> = (0..24).map(|y| row_text(&buf, y)).collect();
        let first = rows.iter().position(|r| r.contains("word")).unwrap();
        let indent = |s: &str| s.len() - s.trim_start().len();
        assert_eq!(indent(&rows[first]), indent(&rows[first + 1]), "flush again");
    }

    /// SPEC §5.3: a fenced body sits on a FLAT code background — across the
    /// whole measure, not just as far as the code happens to reach — with a
    /// colored bar in the margin one column to its left.
    #[test]
    fn fenced_code_gets_a_slab_and_a_gutter_bar() {
        let app = app_with("intro\n\n```rust\nfn x() {}\n```\n\nafter\n");
        let buf = render_to(&app, 80, 24);
        let lay = Layout::compute(&app.config.layout, 80, 24 - STATUS_ROWS, 0);
        let bar_x = lay.margin_left - 1;

        let bar = app.theme.fence_bar.to_ratatui();
        let rows: Vec<u16> = (0..buf.area().height)
            .filter(|y| buf[(bar_x, *y)].symbol() == "\u{258a}" && buf[(bar_x, *y)].fg == bar)
            .collect();
        assert_eq!(rows.len(), 3, "a bar on the fence open, body and close rows");

        // The body row's background reaches the far edge of the measure, well
        // past the end of `fn x() {}`.
        let body = rows[1];
        let code_bg = app.theme.code_bg.to_ratatui();
        let last = lay.margin_left + lay.measure - 1;
        assert_eq!(buf[(last, body)].bg, code_bg, "slab spans the whole measure");
        assert_ne!(
            buf[(bar_x, body)].bg,
            code_bg,
            "the bar hangs in the margin, outside the slab"
        );
        // Prose either side is untouched.
        assert_ne!(buf[(last, rows[0] - 1)].bg, code_bg);
        assert_ne!(buf[(bar_x, rows[2] + 1)].symbol(), "\u{258a}");
    }

    /// An empty `fence_bar` turns the bar off without disturbing the slab —
    /// the same "empty glyph means off" rule the indent guides follow.
    #[test]
    fn an_empty_fence_bar_glyph_draws_nothing() {
        let mut app = app_with("```\ncode\n```\n");
        app.config.glyphs.fence_bar = String::new();
        let buf = render_to(&app, 80, 24);
        let lay = Layout::compute(&app.config.layout, 80, 24 - STATUS_ROWS, 0);
        let bar_x = lay.margin_left - 1;
        let painted = (0..buf.area().height)
            .any(|y| buf[(bar_x, y)].fg == app.theme.fence_bar.to_ratatui());
        assert!(!painted, "no bar should be drawn");
    }

    /// Rows in the last column carrying the scroll-hint glyph.
    fn hint_rows(buf: &ratatui::buffer::Buffer) -> Vec<u16> {
        let x = buf.area().width - 1;
        (0..buf.area().height)
            .filter(|y| buf[(x, *y)].symbol() == "\u{2590}")
            .collect()
    }

    /// A document that fits shows no indicator at all — it is information, not
    /// furniture (SPEC §6: "no scrollbar").
    #[test]
    fn scroll_hint_is_absent_when_the_document_fits() {
        let app = app_with("one\ntwo\nthree\n");
        let buf = render_to(&app, 80, 24);
        assert!(hint_rows(&buf).is_empty(), "short document should show no hint");
    }

    /// Overflow makes it appear, and it tracks the scroll position: at the top
    /// of the document the thumb is at the top, at the end it reaches the
    /// bottom row of the text area.
    #[test]
    fn scroll_hint_tracks_the_scroll_position() {
        let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let mut app = app_with(&text);

        let buf = render_to(&app, 80, 24);
        let top_rows = hint_rows(&buf);
        assert!(!top_rows.is_empty(), "long document should show a hint");

        let lay = Layout::compute(&app.config.layout, 80, 24 - STATUS_ROWS, 0);
        assert_eq!(top_rows[0], lay.top, "thumb starts at the first text row");

        app.buffer.cursor = Cursor::new(199, 0);
        let buf = render_to(&app, 80, 24);
        let bottom_rows = hint_rows(&buf);
        assert!(!bottom_rows.is_empty());
        assert_eq!(
            *bottom_rows.last().unwrap(),
            lay.top + lay.height - 1,
            "thumb ends flush with the last text row"
        );
        assert!(bottom_rows[0] > top_rows[0], "thumb moved down");
        assert_eq!(bottom_rows.len(), top_rows.len(), "thumb keeps its length");
    }

    /// Turning it off leaves the margin clean.
    #[test]
    fn scroll_hint_honors_its_setting() {
        let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
        let mut app = app_with(&text);
        app.config.layout.scroll_hint = false;
        let buf = render_to(&app, 80, 24);
        assert!(hint_rows(&buf).is_empty(), "disabled hint should not paint");
    }

    /// The cursor's line renders raw; the line below has its `#` concealed.
    /// This is the SPEC §2 invariant, exercised through the full frame.
    #[test]
    fn concealment_reveals_only_the_active_line() {
        let mut app = app_with("# One\n## Two\n");
        // Cursor starts on line 0.
        let buf = render_to(&app, 60, 8);
        let joined: String = (0..buf.area().height).map(|y| row_text(&buf, y)).collect();
        assert!(joined.contains("# One"), "active line 0 should be raw");
        assert!(joined.contains("Two"), "line 1 should be present");
        assert!(!joined.contains("## Two"), "inactive line 1 should hide its ##");

        // Move the cursor to line 1; now IT is raw and line 0 conceals.
        app.buffer.cursor = Cursor::new(1, 0);
        app.active = crate::render::conceal::ActiveSet::compute(&app.mode, &app.buffer.cursor, None);
        let buf = render_to(&app, 60, 8);
        let joined: String = (0..buf.area().height).map(|y| row_text(&buf, y)).collect();
        assert!(joined.contains("## Two"), "active line 1 should be raw");
        assert!(!joined.contains("# One"), "inactive line 0 should hide its #");
        assert!(joined.contains("One"), "line 0 text should remain");
    }

    /// Indent guides (SPEC §5.4) are a conceal op like any other, so they show
    /// on inactive lines and give way to the raw source under the cursor —
    /// the same bargain every marker makes.
    #[test]
    fn indent_guides_show_on_inactive_lines_only() {
        let mut app = app_with("- top\n    - nested\n");
        let buf = render_to(&app, 60, 12);
        let joined: String = (0..buf.area().height).map(|y| row_text(&buf, y)).collect();
        // One level at the default tab_width of 4: the guide takes the first
        // space, the marker becomes a hollow sub-bullet, "nested" never moves.
        assert!(joined.contains("│   ◦ nested"), "guide on the inactive line: {joined:?}");

        // The guide's color comes from the styled spans `indent::apply` colored
        // by depth, not from the conceal op — a plain Replace inherits.
        let (x, y) = (0..buf.area().height)
            .find_map(|y| {
                (0..buf.area().width).find(|&x| buf[(x, y)].symbol() == "│").map(|x| (x, y))
            })
            .expect("a guide glyph on screen");
        assert_eq!(buf[(x, y)].fg, app.theme.indent_colors[0].to_ratatui());

        // Cursor onto the nested line: it renders raw, guides included.
        app.buffer.cursor = Cursor::new(1, 0);
        app.active = crate::render::conceal::ActiveSet::compute(&app.mode, &app.buffer.cursor, None);
        let buf = render_to(&app, 60, 12);
        let joined: String = (0..buf.area().height).map(|y| row_text(&buf, y)).collect();
        assert!(joined.contains("    - nested"), "active line is its source");
        assert!(!joined.contains("│"), "…so no guide glyph anywhere: {joined:?}");
    }

    /// Paragraph focus dims text outside the cursor's paragraph.
    #[test]
    fn focus_mode_dims_other_paragraphs() {
        use crate::render::focus::{FocusMode, FocusRegion};
        let mut app = app_with("# Title\n\nbody words\n");
        app.focus = FocusMode::Paragraph; // cursor on line 0 -> region = line 0
        app.focus_region = FocusRegion::compute(&app.buffer, app.focus);
        let buf = render_to(&app, 60, 8);

        // A glyph from the out-of-focus paragraph is faint but keeps its color.
        let b = buf
            .content()
            .iter()
            .find(|c| c.symbol() == "b")
            .expect("'body words' should be on screen");
        assert!(
            b.modifier.contains(ratatui::style::Modifier::DIM),
            "out-of-focus text should be faint"
        );
        assert_eq!(b.fg, app.theme.text.to_ratatui(), "…but keep its syntax color");
    }

    /// Opening the command box dims the buffer behind it.
    #[test]
    fn command_mode_dims_the_buffer() {
        let mut app = app_with("hello there\n\nmore text\n");
        app.mode = Mode::Command(String::new());
        let buf = render_to(&app, 60, 20); // tall, so the box clears rows below the text
        let h = buf
            .content()
            .iter()
            .find(|c| c.symbol() == "h")
            .expect("body text on screen");
        assert!(
            h.modifier.contains(ratatui::style::Modifier::DIM),
            "buffer text should be dimmed behind the command box"
        );
    }

    /// The tree draws Neo-tree guide lines: a `├╴` elbow on each row, `└╴` on
    /// the last sibling, and a `│` column running down past a subtree whose
    /// parent still has siblings below it.
    #[test]
    fn tree_rows_draw_guides_and_icons() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("shoin-frame-tree-{t}"));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("deep.md"), "").unwrap();
        std::fs::write(dir.join("zeta.md"), "").unwrap();

        let mut app = app_with("hi\n");
        let mut tree = crate::tree::FileTree::open(dir.clone());
        tree.selected = tree.entries.iter().position(|e| e.name == "sub").unwrap();
        tree.activate(); // expand it
        app.tree = Some(tree);

        let buf = render_to(&app, 80, 10);
        let rows: Vec<String> = (0..4).map(|y| row_text(&buf, y)).collect();
        assert!(rows[1].starts_with("├╴"), "sub is not the last sibling: {:?}", rows[1]);
        assert!(rows[2].starts_with("│ └╴"), "deep.md hangs under sub: {:?}", rows[2]);
        assert!(rows[3].starts_with("└╴"), "zeta.md closes the tree: {:?}", rows[3]);

        // The markdown file's icon is colored as prose, not left plain.
        let icon_x = rows[3].chars().position(|c| c == '\u{f48a}').expect("markdown icon");
        assert_eq!(
            buf[(icon_x as u16, 3)].fg,
            app.theme.headings[0].to_ratatui(),
            "a .md file gets the prose-colored icon"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `nerd_fonts = false` must not paint Private Use Area glyphs at anyone
    /// whose font has no icons — directories fall back to plain arrows.
    #[test]
    fn tree_without_nerd_fonts_uses_arrows() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("shoin-frame-ascii-{t}"));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("zeta.md"), "").unwrap();

        let mut app = app_with("hi\n");
        app.config.glyphs.nerd_fonts = false;
        app.tree = Some(crate::tree::FileTree::open(dir.clone()));

        let buf = render_to(&app, 80, 10);
        let joined: String = (0..4).map(|y| row_text(&buf, y)).collect();
        assert!(joined.contains("▸ sub"), "a collapsed dir gets an arrow: {joined:?}");
        assert!(joined.contains("zeta.md"));
        assert!(
            !joined.chars().any(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)),
            "no Private Use Area glyphs: {joined:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The finder overlay draws its query, its match count, and the matching
    /// paths — with the matched characters picked out in the accent color.
    #[test]
    fn finder_overlay_lists_matches_and_highlights_them() {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("shoin-frame-finder-{t}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("alpha.md"), "").unwrap();
        std::fs::write(dir.join("beta.md"), "").unwrap();

        let mut app = app_with("hello there\n");
        let mut finder = crate::finder::Finder::open(dir.clone());
        finder.push_char('b');
        app.finder = Some(finder);

        let buf = render_to(&app, 60, 20);
        let joined: String = (0..buf.area().height).map(|y| row_text(&buf, y)).collect();
        assert!(joined.contains("find"), "the box is titled");
        assert!(joined.contains("> b"), "the query is echoed");
        assert!(joined.contains("1/2"), "matched / total files");
        assert!(joined.contains("beta.md"), "the match is listed");
        assert!(!joined.contains("alpha.md"), "the non-match is filtered out");

        // Result rows carry the same file-type icon the tree draws.
        let row = (0..buf.area().height)
            .find(|&y| row_text(&buf, y).contains("beta.md"))
            .expect("result row");
        let icon_x = row_text(&buf, row)
            .chars()
            .position(|c| c == '\u{f48a}')
            .expect("markdown icon on the result row");
        assert_eq!(
            buf[(icon_x as u16, row)].fg,
            app.theme.headings[0].to_ratatui(),
            "colored by type, like the tree"
        );

        // The matched 'b' of "beta.md" renders in the accent color, its
        // neighbours do not.
        let accent = app.theme.link.to_ratatui();
        // Char position, not byte: the row starts with a multi-byte "▸".
        let x = row_text(&buf, row).chars().position(|c| c == 'b').unwrap() as u16;
        assert_eq!(buf[(x, row)].fg, accent, "matched char is accented");
        assert_ne!(buf[(x + 1, row)].fg, accent, "the rest of the path is not");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A split draws both panes, each measuring its own width, with a divider
    /// between them — and only the focused one shows its raw (active) line.
    #[test]
    fn a_split_renders_two_panes_side_by_side() {
        let mut app = app_with("# Title\n\nbody text here\n");
        app.split_pane_for_test(true);

        let buf = render_to(&app, 100, 24);
        let rows: Vec<String> = (0..24).map(|y| row_text(&buf, y)).collect();
        let with_body: Vec<&String> = rows.iter().filter(|r| r.contains("body text here")).collect();
        assert_eq!(with_body.len(), 1, "one row, holding the text twice");
        let line = with_body[0];
        assert_eq!(line.matches("body text here").count(), 2, "once per pane: {line:?}");

        // The divider column is painted with the dim color, and nothing else is
        // drawn over it.
        let geo = app.layout.geometry(pane_area(&app, Rect { x: 0, y: 0, width: 100, height: 24 }));
        assert_eq!(geo.dividers.len(), 1);
        let x = geo.dividers[0].x;
        assert_eq!(buf[(x, 5)].bg, app.theme.text_dim.to_ratatui());

        // Panes have their own cursors, but CONCEALMENT is still per document:
        // the row index lives in its render cache, so the focused pane's active
        // line renders raw in both. Panes showing DIFFERENT documents do
        // diverge — the background one has no cursor, so it conceals
        // throughout.
        let heading_row = rows.iter().find(|r| r.contains("Title")).expect("the heading");
        assert_eq!(heading_row.matches("# Title").count(), 2, "{heading_row:?}");
    }

    /// A left-click maps back to the buffer line it landed on.
    #[test]
    fn click_maps_to_the_clicked_line() {
        let app = app_with("# One\n\nsecond paragraph here\n");
        let area = Rect { x: 0, y: 0, width: 60, height: 12 };
        // Row lay.top is line 0; two rows down is the (blank) line 1; three is 2.
        assert_eq!(locate_click(&app, area, 8, 2).map(|c| c.line), Some(0));
        assert_eq!(locate_click(&app, area, 8, 4).map(|c| c.line), Some(2));
        // Above the text area → no position.
        assert!(locate_click(&app, area, 8, 0).is_none());
    }

    /// The theme background fills the whole surface, so a theme renders on its
    /// own background rather than the terminal's.
    #[test]
    fn theme_background_is_painted() {
        let app = app_with("hello world\n");
        let buf = render_to(&app, 40, 6);
        let bg = app.theme.background.to_ratatui();
        assert!(
            buf.content().iter().all(|c| c.bg == bg),
            "every cell should carry the theme background"
        );
    }

    /// `layout.stable_gutter`: a heading's TEXT stays in the same column
    /// whether the cursor is on it (markers revealed) or not (concealed).
    #[test]
    fn a_stable_gutter_keeps_revealed_text_in_place() {
        let mut app = app_with("# Title\n\nbody\n");
        // Where the heading's "T" sits, anywhere on screen (padding moves rows).
        let column_of = |buf: &ratatui::buffer::Buffer| -> Option<u16> {
            (0..buf.area().height)
                .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
                .find(|(x, y)| buf[(*x, *y)].symbol() == "T")
                .map(|(x, _)| x)
        };
        let has_hash = |buf: &ratatui::buffer::Buffer| -> bool {
            (0..buf.area().height)
                .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
                .any(|(x, y)| buf[(x, y)].symbol() == "#")
        };

        // Cursor elsewhere: the heading is concealed, "Title" at the margin.
        app.buffer.cursor = Cursor::new(2, 0);
        app.active = crate::render::conceal::ActiveSet { start: 2, end: 2 };
        let concealed = render_to(&app, 60, 24);

        // Cursor on the heading: "## " comes back, into the gutter.
        app.buffer.cursor = Cursor::new(0, 0);
        app.active = crate::render::conceal::ActiveSet { start: 0, end: 0 };
        let revealed = render_to(&app, 60, 24);

        assert_eq!(column_of(&concealed), column_of(&revealed));
        assert!(column_of(&concealed).is_some(), "the heading should be drawn");
        assert!(
            has_hash(&revealed),
            "the markers should be on screen, just left of the text"
        );

        // Off, the text moves with its markers — the behavior being avoided.
        app.config.layout.stable_gutter = false;
        let unstable = render_to(&app, 60, 24);
        assert_ne!(column_of(&concealed), column_of(&unstable));
    }

    /// A warm cache must paint exactly what a cold one paints. This is the
    /// backstop for the whole incremental design: edit, scroll and reveal, then
    /// compare the frame against the same state rendered from an empty cache.
    #[test]
    fn a_warm_cache_paints_what_a_cold_one_does() {
        let mut app = app_with("# Title\n\nsome **bold** prose here\n\n- one\n- two\n\n> quoted\n");
        let _ = render_to(&app, 60, 12);

        // Edit, move, and re-render — the cache follows all of it.
        app.buffer.insert_str(Cursor::new(2, 4), " more");
        app.buffer.insert_str(Cursor::new(4, 0), "- inserted\n");
        app.refresh_blocks();
        app.buffer.cursor = Cursor::new(5, 2);
        app.active = crate::render::conceal::ActiveSet { start: 5, end: 5 };
        let warm = render_to(&app, 60, 12);

        *app.cache.borrow_mut() = RenderCache::default();
        let cold = render_to(&app, 60, 12);
        assert_eq!(warm, cold);
    }

    /// `layout.conceal = false` renders every line raw (the escape hatch).
    #[test]
    fn conceal_disabled_keeps_all_markers() {
        let mut app = app_with("# One\n## Two\n");
        app.config.layout.conceal = false;
        let buf = render_to(&app, 60, 8);
        let joined: String = (0..buf.area().height).map(|y| row_text(&buf, y)).collect();
        assert!(joined.contains("# One"));
        assert!(joined.contains("## Two"), "conceal off: line 1 keeps its ##");
    }
}

