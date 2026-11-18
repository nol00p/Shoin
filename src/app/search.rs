use super::*;

impl App {
    // ----------------------------------------------------------------- search

    pub(super) fn on_key_search(&mut self, key: KeyEvent, mut query: String, reverse: bool) {
        match field_key(key, &mut query) {
            FieldKey::Cancel => {
                self.mode = Mode::Normal;
                self.flash = None;
            }
            FieldKey::Submit => {
                self.mode = Mode::Normal;
                if !query.is_empty() {
                    self.search = Some(Search { pattern: query, reverse });
                    self.search_move(reverse);
                }
            }
            FieldKey::Stay => self.mode = Mode::Search { query, reverse },
        }
    }

    /// Move the cursor to the next match of the active search, wrapping around
    /// the buffer. `backward` searches toward the start.
    pub(super) fn search_move(&mut self, backward: bool) {
        let pattern = match &self.search {
            Some(s) if !s.pattern.is_empty() => s.pattern.clone(),
            _ => return,
        };
        let rope = &self.buffer.rope;
        let cur = self.buffer.char_index(self.buffer.cursor);
        let found = if backward {
            // Strictly before the cursor, else wrap to the end.
            cur.checked_sub(1)
                .and_then(|before| rope_rfind(rope, &pattern, before))
                .or_else(|| rope_rfind(rope, &pattern, rope.len_chars()))
        } else {
            rope_find(rope, &pattern, cur + 1).or_else(|| rope_find(rope, &pattern, 0))
        };
        match found {
            Some(idx) => {
                self.buffer.cursor = self.idx_to_cursor(idx);
                let sigil = if backward { "?" } else { "/" };
                self.notify(format!("{sigil}{pattern}"), FlashKind::Info);
            }
            None => self.notify(format!("not found: {pattern}"), FlashKind::Error),
        }
    }
}
