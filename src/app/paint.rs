use super::*;

impl App {
    /// Erase the pixels of iTerm2 pictures that moved or went away.
    ///
    /// An iTerm2 picture is the content of the cells it covers, and goes only
    /// when something is written over them. ratatui writes only the cells that
    /// CHANGED, and a picture's box is blank before and after, so the cells of
    /// an old box that are still blank are never written and keep the old
    /// pixels. This writes exactly those cells, as `buf` (the frame just
    /// drawn) has them, so the screen and ratatui's idea of it stay the same.
    /// A cell that now holds text was already rewritten by ratatui. Kitty
    /// pictures are separate objects, revoked by `image::clear` instead.
    pub(super) fn unpaint_moved_pictures(&self, buf: &ratatui::buffer::Buffer) -> String {
        let painted = self.painted.borrow();
        if self.image_protocol != crate::image::Protocol::Iterm2 || painted.0.is_empty() {
            return String::new();
        }
        let now = self.images.borrow();
        let mut out = String::new();
        for p in painted.0.iter().filter(|p| !now.contains(p)) {
            for y in p.y..(p.y + p.rows).min(buf.area.bottom()) {
                for x in p.x..(p.x + p.cols).min(buf.area.right()) {
                    let cell = &buf[(x, y)];
                    if cell.symbol() == " " {
                        out.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
                        out.push_str(&crate::render::print::sgr(cell.fg, cell.bg, cell.modifier));
                        out.push(' ');
                    }
                }
            }
        }
        if !out.is_empty() {
            // DECSC/DECRC: ratatui has put the caret where the document wants it.
            out = format!("\x1b7{out}\x1b[0m\x1b8");
        }
        out
    }

    /// The pictures the frame just drawn reserved boxes for, less the ones that
    /// must not be painted.
    pub(super) fn visible_pictures(&self) -> Vec<crate::render::Placement> {
        let mut places = std::mem::take(&mut *self.images.borrow_mut());
        // A picture cut off at the side is not drawn at all; see
        // `Placement::fits_in` for why that is the safe answer rather than a
        // lazy one. One cut off at the top or bottom stays: kitty draws the
        // part that shows (`paint_images`), and only kitty.
        places.retain(|p| p.fits_in(self.term_size));
        if self.image_protocol != crate::image::Protocol::Kitty {
            places.retain(|p| p.whole());
        }
        // A spotlight box was summoned to be read. Anything it covers stands
        // down until it closes — an overlay competing with a photograph for
        // the same cells is a fight the overlay has to win.
        if let Some(box_) = self.overlay.get() {
            places.retain(|p| !p.overlaps(box_));
        }
        // In screen order, not the order the panes were drawn in: the focused
        // pane is drawn last, so moving focus between two panes that show the
        // same pictures would reorder an unchanged frame, and `picture_bytes`
        // would repaint it. Panes do not overlap, so the position is unique.
        places.sort_by_key(|p| (p.y, p.x));
        places
    }

    /// Draw the frame's pictures over the boxes the renderer reserved for them.
    ///
    /// Outside ratatui entirely, because there is no cell representation of a
    /// picture — the escape sequences go straight to the terminal, positioned
    /// by hand. Everything ratatui knows about those cells is that they are
    /// blank, which is exactly right: on the next frame it repaints them blank
    /// and this runs again.
    ///
    /// A failed write is dropped on purpose. A terminal that will not take an
    /// image still has a document to show, and an editor that quit over a
    /// picture would be a worse editor.
    pub(super) fn paint_images(&self, places: Vec<crate::render::Placement>) {
        use std::io::Write;
        let Some(out) = self.picture_bytes(&places) else {
            return;
        };
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
        *self.painted.borrow_mut() = (places, self.term_size);
    }

    /// What `paint_images` writes for `places`, or `None` when it writes
    /// nothing: no protocol, or nothing moved since the last paint.
    pub(super) fn picture_bytes(&self, places: &[crate::render::Placement]) -> Option<String> {
        if self.image_protocol == crate::image::Protocol::None {
            return None;
        }

        // Nothing moved and nothing resized, so what is on screen is still
        // right. This is the whole economy of the painter: a keystroke that
        // does not move a picture sends no bytes at all, where re-sending
        // meant a third of a megabyte per keypress, per picture.
        {
            let painted = self.painted.borrow();
            if painted.0 == places && painted.1 == self.term_size {
                return None;
            }
        }

        let mut out = String::new();
        // DECSC/DECRC around the whole batch. Positioning each image moves the
        // caret, and ratatui has already put it where the document wants it —
        // leaving it under the last picture is what makes the screen look
        // scrambled after a scroll.
        out.push_str("\x1b7");
        // Kitty images outlive the cells they were drawn over, so last frame's
        // have to be taken down before this frame's go up. Only reached when
        // something actually changed, so this is not a per-frame flicker.
        out.push_str(&crate::image::unplace(self.image_protocol));
        // Kitty keeps each picture's pixels under its id, so one still on
        // screen is placed again rather than sent again: scrolling a picture
        // one row costs a put, not the file. One that left the screen is
        // forgotten, so the terminal's memory holds only what is shown.
        let kitty = self.image_protocol == crate::image::Protocol::Kitty;
        let mut sent: Vec<u32> = Vec::new();
        if kitty {
            let painted = self.painted.borrow();
            for p in &painted.0 {
                if sent.contains(&p.id) {
                    continue;
                }
                if places.iter().any(|q| q.id == p.id) {
                    sent.push(p.id);
                } else {
                    out.push_str(&crate::image::kitty_forget(p.id));
                }
            }
        }
        let payloads = self.image_payloads.borrow();
        for p in places {
            let Some(payload) = payloads.get(&p.id) else { continue };
            if p.shown == 0 {
                continue;
            }
            let seq = if kitty {
                if !sent.contains(&p.id) {
                    let bytes = crate::image::kitty_send(p.id, payload);
                    if bytes.is_empty() {
                        continue; // not a PNG: its box and caption stay
                    }
                    out.push_str(&bytes);
                    sent.push(p.id);
                }
                // Cut off at the top or bottom: the rows that show, from the
                // picture's own pixels, in a box as tall as those rows.
                let crop = (!p.whole())
                    .then(|| crate::image::probe(payload))
                    .flatten()
                    .map(|info| crate::image::crop(info.width, info.height, p.rows, p.skip, p.shown));
                crate::image::kitty_put(p.id, p.cols, p.shown, crop)
            } else {
                crate::image::draw(self.image_protocol, payload, p.cols, p.rows)
            };
            // `\x1b[{row};{col}H` is 1-based, and `Placement` is not.
            out.push_str(&format!("\x1b[{};{}H", p.y + 1, p.x + 1));
            out.push_str(&seq);
        }
        out.push_str("\x1b8");
        Some(out)
    }

    /// Push the mode's caret shape and color to the terminal, but only on an
    /// actual change. Command mode gets a bar caret painted blue — a clear "type
    /// a command here" cue; every other mode uses the configured shape and the
    /// terminal's default color.
    pub(super) fn sync_cursor_shape(&mut self) {
        let c = &self.config.cursor;
        // The finder is a typing box like the command line, so it takes the
        // same bar caret and accent color even though the mode is still Normal.
        let command = matches!(
            self.mode,
            Mode::Command(_) | Mode::Search { .. } | Mode::Prompt(_)
        ) || self.finder.is_some();

        let shape = if command {
            CursorShape::Bar
        } else {
            match self.mode {
                Mode::Insert => CursorShape::parse(&c.insert).unwrap_or(CursorShape::Block),
                Mode::Visual | Mode::VisualLine => CursorShape::parse(&c.visual).unwrap_or(CursorShape::Block),
                _ => CursorShape::parse(&c.normal).unwrap_or(CursorShape::Block),
            }
        };
        if self.cursor_shape != Some(shape) {
            self.cursor_shape = Some(shape);
            let _ = execute!(std::io::stdout(), shape.to_terminal(c.blink));
        }

        // The accent caret marks "typing goes somewhere other than the text
        // under it" — the `:` line, the finder, and now a selection that will
        // hand you back to Insert. It is the whole cue for the last of those,
        // so nothing is drawn and zen mode keeps working.
        let tinted = command || self.visual_resumes_insert;
        if tinted != self.cursor_colored {
            self.cursor_colored = tinted;
            set_cursor_color(tinted.then(|| cursor_blue(&self.theme)));
        }
    }
}
