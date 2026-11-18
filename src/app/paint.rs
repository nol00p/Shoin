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
        // A picture that does not fit on screen is not drawn at all; see
        // `Placement::fits_in` for why that is the safe answer rather than a
        // lazy one.
        places.retain(|p| p.fits_in(self.term_size));
        // A spotlight box was summoned to be read. Anything it covers stands
        // down until it closes — an overlay competing with a photograph for
        // the same cells is a fight the overlay has to win.
        if let Some(box_) = self.overlay.get() {
            places.retain(|p| !p.overlaps(box_));
        }
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
        if self.image_protocol == crate::image::Protocol::None {
            return;
        }

        // Nothing moved and nothing resized, so what is on screen is still
        // right. This is the whole economy of the painter: a keystroke that
        // does not move a picture sends no bytes at all, where re-sending
        // meant a third of a megabyte per keypress, per picture.
        {
            let painted = self.painted.borrow();
            if painted.0 == places && painted.1 == self.term_size {
                return;
            }
        }

        let mut out = String::new();
        // DECSC/DECRC around the whole batch. Positioning each image moves the
        // caret, and ratatui has already put it where the document wants it —
        // leaving it under the last picture is what makes the screen look
        // scrambled after a scroll.
        out.push_str("\x1b7");
        // Kitty images outlive the cells they were drawn over, so last frame's
        // have to be revoked before this frame's go down. Only reached when
        // something actually changed, so this is not a per-frame flicker.
        out.push_str(&crate::image::clear(self.image_protocol));
        // The payloads stay in the cache and are only borrowed here — a frame
        // with three photographs on it must not clone three photographs.
        let cache = self.cache.borrow();
        for p in &places {
            let pic = match cache
                .entry(p.line)
                .and_then(|e| e.embed.as_ref())
                .and_then(|x| x.rows.get(p.index))
            {
                Some(crate::transclude::preview::Row::Image(pic)) => pic,
                _ => continue,
            };
            if pic.payload.is_empty() {
                continue;
            }
            // `\x1b[{row};{col}H` is 1-based, and `Placement` is not.
            out.push_str(&format!("\x1b[{};{}H", p.y + 1, p.x + 1));
            out.push_str(&crate::image::draw(
                self.image_protocol,
                &pic.payload,
                p.cols,
                p.rows,
            ));
        }
        out.push_str("\x1b8");
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
        *self.painted.borrow_mut() = (places, self.term_size);
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
