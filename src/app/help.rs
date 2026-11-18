use super::*;

impl App {
    // ------------------------------------------------------------------ help

    pub(super) fn open_help(&mut self, topic: &str) {
        self.help = Some(Help::open(topic));
    }

    /// Scroll, search or close the help overlay.
    pub(super) fn on_key_help(&mut self, key: KeyEvent) {
        let Some(help) = &mut self.help else { return };
        help.status = None;
        // A `/` search being typed owns the keys until Enter or Esc.
        if let Some(typed) = help.typing.as_mut() {
            match key.code {
                KeyCode::Esc => help.typing = None,
                KeyCode::Backspace if typed.is_empty() => help.typing = None,
                KeyCode::Backspace => {
                    typed.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => typed.clear(),
                KeyCode::Char(c) => typed.push(c),
                KeyCode::Enter => {
                    let q = help.typing.take().unwrap_or_default();
                    if !q.is_empty() {
                        if !help.search(&q, false) {
                            help.status = Some(format!("not found: {q}"));
                        }
                        help.query = Some(q);
                    }
                }
                _ => {}
            }
            return;
        }
        let max = help.rows().len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.help = None,
            KeyCode::Char('j') | KeyCode::Down => help.scroll = (help.scroll + 1).min(max),
            KeyCode::Char('k') | KeyCode::Up => help.scroll = help.scroll.saturating_sub(1),
            KeyCode::Char('d') | KeyCode::PageDown => help.scroll = (help.scroll + 10).min(max),
            KeyCode::Char('u') | KeyCode::PageUp => help.scroll = help.scroll.saturating_sub(10),
            KeyCode::Char('g') | KeyCode::Home => help.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => help.scroll = max,
            KeyCode::Char('/') => help.typing = Some(String::new()),
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => help.switch(true),
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => help.switch(false),
            KeyCode::Char(c @ '1'..='9') => help.show(c as usize - '1' as usize),
            KeyCode::Char(c @ ('n' | 'N')) => match help.query.clone() {
                Some(q) if !help.search(&q, c == 'N') => help.status = Some(format!("not found: {q}")),
                Some(_) => {}
                None => help.status = Some("no search yet — / to search".into()),
            },
            _ => {}
        }
    }
}
