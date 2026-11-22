use super::*;
use crate::fs::save::SavePolicy;
use crate::render::markdown::block::BlockCache;
use crate::test_support::{app_with, app_with_config};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn text(app: &App) -> String {
    app.buffer.rope.to_string()
}

/// Feed plain character keys, one per char.
fn feed(app: &mut App, keys: &str) {
    for c in keys.chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.sync_after_input();
    }
}

fn esc(app: &mut App) {
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.sync_after_input();
}

/// An iTerm2 picture that went away has the cells it covered rewritten —
/// the blank ones only, as the frame now has them — and a picture that is
/// still in place costs nothing.
#[test]
fn a_moved_iterm2_picture_has_its_blank_cells_rewritten() {
    use ratatui::buffer::Buffer as Frame;
    use ratatui::layout::Rect;
    let mut app = app_with("text\n");
    app.image_protocol = crate::image::Protocol::Iterm2;
    let old = crate::render::Placement { x: 2, y: 1, cols: 3, rows: 2, skip: 0, shown: 2, line: 0, index: 0, id: 1 };
    *app.painted.borrow_mut() = (vec![old], (20, 5));

    let mut buf = Frame::empty(Rect::new(0, 0, 20, 5));
    buf[(3, 1)].set_symbol("x"); // text now drawn over one cell of the old box
    let out = app.unpaint_moved_pictures(&buf);
    let moves = out.matches('H').count();
    assert_eq!(moves, 5, "six cells, one holds text now: {out:?}");
    assert!(!out.contains("[2;4H"), "the text cell is ratatui's, left alone");
    assert!(out.starts_with("\x1b7") && out.ends_with("\x1b8"));

    // Still placed where it was: nothing to undo.
    app.images.borrow_mut().push(old);
    assert!(app.unpaint_moved_pictures(&buf).is_empty());

    // Kitty pictures are objects, revoked another way.
    app.images.borrow_mut().clear();
    app.image_protocol = crate::image::Protocol::Kitty;
    assert!(app.unpaint_moved_pictures(&buf).is_empty());
}

/// Kitty gets a picture's bytes once, then only puts: cut at the top, the
/// source rectangle is the rows that show; scrolled a row, the same id is put
/// again with the new rectangle; gone from the screen, its pixels are freed.
#[test]
fn a_cut_off_kitty_picture_is_sent_once_and_put_by_the_rows_that_show() {
    use crate::render::conceal::ActiveSet;
    let d = std::env::temp_dir().join(format!("shoin-paint-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let mut png = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89,
    ];
    png[16..20].copy_from_slice(&400u32.to_be_bytes());
    png[20..24].copy_from_slice(&600u32.to_be_bytes());
    std::fs::write(d.join("photo.png"), &png).unwrap();
    let body: String = std::iter::once("![[photo.png]]\n".to_string())
        .chain((0..10).map(|i| format!("after {i}\n")))
        .collect();
    std::fs::write(d.join("note.md"), body).unwrap();
    let mut app = App::new(Config::default(), Some(d.join("note.md")), None).unwrap();
    app.embed_mode = crate::transclude::Mode::Short;
    app.image_protocol = crate::image::Protocol::Kitty;
    app.term_size = (80, 24);
    let id = crate::image::kitty_id(&png);

    // Draw with the caret on `line`, and return the placement and the bytes.
    let paint = |app: &mut App, line: usize, height: u16| {
        app.buffer.cursor = Cursor::new(line, 0);
        app.active = ActiveSet::none();
        app.term_size = (80, height);
        app.refresh_blocks();
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height)).unwrap();
        t.draw(|f| crate::render::frame::render(f, app)).unwrap();
        let places = app.visible_pictures();
        let out = app.picture_bytes(&places).unwrap_or_default();
        *app.painted.borrow_mut() = (places.clone(), app.term_size);
        (places, out)
    };

    let (places, out) = paint(&mut app, 10, 24);
    assert_eq!(places.len(), 1, "{places:?}");
    let p = places[0];
    assert!(p.skip > 0 && p.shown == p.rows - p.skip, "cut off at the top: {p:?}");
    assert_eq!(out.matches("a=t,").count(), 1, "sent once");
    assert!(out.contains(&format!("\x1b_Ga=t,f=100,i={id},q=2,m=0;")));
    let c = crate::image::crop(400, 600, p.rows, p.skip, p.shown);
    assert_eq!((c.y + c.h, c.w), (600, 400), "down to the last pixel row, full width");
    let put = format!(
        "\x1b[{};{}H\x1b_Ga=p,i={id},q=2,C=1,c={},r={},x=0,y={},w=400,h={}\x1b\\",
        p.y + 1,
        p.x + 1,
        p.cols,
        p.shown,
        c.y,
        c.h
    );
    assert!(out.contains(&put), "{put:?} in {out:?}");
    assert!(out.starts_with("\x1b7\x1b_Ga=d,d=a,q=2\x1b\\") && out.ends_with("\x1b8"));

    // Scrolled up a row: one more row of it shows, and nothing is re-sent.
    let top = app.focused_pane().unwrap().scroll.get();
    app.focused_pane().unwrap().scroll.set(top - 1);
    let (places, out) = paint(&mut app, 5, 24);
    let q = places[0];
    assert_eq!(q.skip + 1, p.skip, "{q:?}");
    assert!(!out.contains("a=t,"), "only the crop changed: {out:?}");
    assert!(out.contains(&format!("a=p,i={id},")) && !out.contains("d=I"));

    // Unchanged: no bytes at all.
    assert_eq!(app.picture_bytes(&places), None);

    // A frame with no picture on it (the caret on the embed line, a scroll
    // past it): taken down, and its pixels freed.
    let out = app.picture_bytes(&[]).unwrap();
    assert!(out.contains(&crate::image::kitty_forget(id)), "{out:?}");
    assert!(!out.contains("a=p"));

    // Under iTerm2 a cut picture is not painted; the frame shows its caption.
    app.image_protocol = crate::image::Protocol::Iterm2;
    *app.painted.borrow_mut() = (Vec::new(), (0, 0));
    let (places, _) = paint(&mut app, 10, 24);
    assert!(places.is_empty(), "{places:?}");
    std::fs::remove_dir_all(&d).ok();
}

/// `![[photo.png|alt text]]` captions the picture with its alt text in place
/// of the file's name, elided to the row; `|200` is a size and keeps the
/// name. Under iTerm2 a box cut off by the top shows the same caption.
#[test]
fn a_picture_with_alt_text_is_captioned_with_it() {
    use crate::render::conceal::ActiveSet;
    let d = std::env::temp_dir().join(format!("shoin-alt-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let mut png = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89,
    ];
    png[16..20].copy_from_slice(&400u32.to_be_bytes());
    png[20..24].copy_from_slice(&200u32.to_be_bytes());
    std::fs::write(d.join("photo.png"), &png).unwrap();
    let long = "A red barn ".repeat(12);
    let body: String = format!("![[photo.png|A red barn]]\n![[photo.png|200]]\n![[photo.png|{long}]]\n")
        .chars()
        .chain((0..30).flat_map(|i| format!("after {i}\n").chars().collect::<Vec<_>>()))
        .collect();
    std::fs::write(d.join("note.md"), body).unwrap();
    let mut app = App::new(Config::default(), Some(d.join("note.md")), None).unwrap();
    app.embed_mode = crate::transclude::Mode::Short;
    app.image_protocol = crate::image::Protocol::None;
    let screen = |app: &mut App, line: usize, height: u16| -> Vec<String> {
        app.buffer.cursor = Cursor::new(line, 0);
        app.active = ActiveSet::none();
        app.term_size = (80, height);
        app.refresh_blocks();
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height)).unwrap();
        t.draw(|f| crate::render::frame::render(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..height).map(|y| (0..80).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect()
    };
    let rows = screen(&mut app, 4, 80);
    let captions: Vec<&String> = rows.iter().filter(|r| r.contains("400\u{d7}200") || r.contains('\u{2026}')).collect();
    assert_eq!(captions.len(), 3, "{rows:#?}");
    assert!(captions[0].contains("A red barn \u{b7} 400\u{d7}200") && !captions[0].contains("photo"), "{captions:?}");
    assert!(captions[1].contains("photo.png \u{b7} 400\u{d7}200"), "a size is not alt text: {captions:?}");
    // Too long for the row: cut with an ellipsis, the size with it.
    let elided = captions[2].split('\u{2026}').next().unwrap();
    assert!(elided.trim_start().starts_with("A red barn A red barn"), "{captions:?}");
    assert!(captions[2].contains('\u{2026}') && !captions[2].contains("400"), "{captions:?}");

    // iTerm2, the first picture cut off by the top: its box carries the alt text.
    app.image_protocol = crate::image::Protocol::Iterm2;
    app.focused_pane().unwrap().scroll.set(2);
    let rows = screen(&mut app, 4, 80);
    assert!(rows.iter().any(|r| r.contains("\u{2502} A red barn \u{b7} 400\u{d7}200")), "{rows:#?}");
    std::fs::remove_dir_all(&d).ok();
}

/// A whole kitty picture is put whole: no source rectangle.
#[test]
fn a_whole_kitty_picture_is_put_without_a_crop() {
    let d = std::env::temp_dir().join(format!("shoin-paint-whole-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let mut png = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89,
    ];
    png[16..20].copy_from_slice(&800u32.to_be_bytes());
    png[20..24].copy_from_slice(&200u32.to_be_bytes());
    std::fs::write(d.join("photo.png"), &png).unwrap();
    std::fs::write(d.join("note.md"), "top\n![[photo.png]]\nafter\n").unwrap();
    let mut app = App::new(Config::default(), Some(d.join("note.md")), None).unwrap();
    app.embed_mode = crate::transclude::Mode::Short;
    app.image_protocol = crate::image::Protocol::Kitty;
    app.term_size = (80, 24);
    app.refresh_blocks();
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    t.draw(|f| crate::render::frame::render(f, &app)).unwrap();
    let places = app.visible_pictures();
    let p = places[0];
    assert!(p.whole(), "{p:?}");
    let out = app.picture_bytes(&places).unwrap();
    let id = crate::image::kitty_id(&png);
    assert!(out.contains(&format!("\x1b_Ga=p,i={id},q=2,C=1,c={},r={}\x1b\\", p.cols, p.rows)), "{out:?}");
    std::fs::remove_dir_all(&d).ok();
}

/// Two panes showing the same picture: moving focus between them moves no
/// picture, so it sends nothing. The focused pane is drawn last, which used
/// to swap the two placements and re-place (Kitty) or re-send (iTerm2) both
/// on every `<C-w>w`.
#[test]
fn moving_focus_between_two_panes_repaints_no_picture() {
    let d = std::env::temp_dir().join(format!("shoin-paint-focus-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let mut png = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89,
    ];
    png[16..20].copy_from_slice(&200u32.to_be_bytes());
    png[20..24].copy_from_slice(&100u32.to_be_bytes());
    std::fs::write(d.join("photo.png"), &png).unwrap();
    std::fs::write(d.join("note.md"), "top\n![[photo.png]]\nafter\n").unwrap();
    for protocol in [crate::image::Protocol::Kitty, crate::image::Protocol::Iterm2] {
        let mut app = App::new(Config::default(), Some(d.join("note.md")), None).unwrap();
        app.embed_mode = crate::transclude::Mode::Short;
        app.image_protocol = protocol;
        app.term_size = (120, 30);
        app.split_pane(true);
        let paint = |app: &mut App| {
            app.refresh_blocks();
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
            t.draw(|f| crate::render::frame::render(f, app)).unwrap();
            let places = app.visible_pictures();
            let out = app.picture_bytes(&places);
            *app.painted.borrow_mut() = (places.clone(), app.term_size);
            (places, out)
        };
        let (places, out) = paint(&mut app);
        assert_eq!(places.len(), 2, "one picture per pane: {places:?}");
        assert!(out.is_some(), "the first paint draws them");
        for _ in 0..2 {
            app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
            feed(&mut app, "w");
            let (again, out) = paint(&mut app);
            assert_eq!(again.len(), 2, "{protocol:?}: {again:?}");
            assert_eq!(out, None, "{protocol:?}: focus moved, no picture did");
        }
    }
    std::fs::remove_dir_all(&d).ok();
}

/// Read-only mode refuses every way of changing the text, and says so,
/// while moving, yanking and searching still work.
#[test]
fn read_only_refuses_edits_and_says_so() {
    let mut app = app_with("# Title\n\n- one\n- two\n");
    app.set_view(true);
    let before = text(&app);

    feed(&mut app, "jj"); // a move: allowed, and it ends the offer to edit
    assert_eq!(app.buffer.cursor.line, 2);
    for keys in ["dd", "x", "p", ">al", "u", "o", "gb"] {
        feed(&mut app, keys);
        esc(&mut app);
        assert_eq!(text(&app), before, "{keys} changed the text");
    }
    assert!(flash_text(&app).contains("read-only"), "{}", flash_text(&app));
    app.on_paste("pasted".into());
    assert_eq!(text(&app), before, "a paste is an edit too");

    feed(&mut app, "yy");
    assert_eq!(text(&app), before);
    assert!(app.view, "nothing above left read-only mode");
    // Underneath the refusals, the buffer itself will not change.
    assert!(!app.buffer.insert_str(Cursor::new(0, 0), "x"));
}

/// `i` right after a refusal (or right after entering) is the way out.
#[test]
fn i_after_the_refusal_starts_editing() {
    let mut app = app_with("hello\n");
    app.set_view(true);
    feed(&mut app, "i");
    assert!(!app.view && matches!(app.mode, Mode::Insert), "first i edits");
    feed(&mut app, "X");
    assert_eq!(text(&app), "Xhello\n");
    esc(&mut app);

    app.set_view(true);
    feed(&mut app, "l"); // any other command ends the offer
    feed(&mut app, "i");
    assert!(app.view, "refused once");
    assert!(flash_text(&app).contains("press i"), "{}", flash_text(&app));
    feed(&mut app, "i");
    assert!(!app.view && matches!(app.mode, Mode::Insert));
}

/// Every line conceals, the cursor's included, and `q` quits.
#[test]
fn read_only_conceals_everything_and_q_quits() {
    let mut app = app_with("**bold**\n");
    // As if opened from disk: nothing unsaved for `q` to stop at.
    app.buffer.saved_state = app.buffer.history.state();
    app.buffer.sync_modified();
    app.set_view(true);
    app.sync_after_input();
    assert!(!app.active.contains(0), "no line is revealed");
    feed(&mut app, "q");
    assert!(app.quit);
}

/// `Space v` (`toggle_view`) is `:view` on a key, and works from inside
/// read-only mode too — it is not an edit, so it is not refused.
#[test]
fn leader_v_toggles_read_only() {
    let mut app = app_with_keys("hello\n", &[("<leader>v", "toggle_view")]);
    feed(&mut app, " v");
    assert!(app.view, "on");
    assert!(app.buffer.read_only);
    feed(&mut app, " v");
    assert!(!app.view, "off again, from inside read-only");
    assert!(!app.buffer.read_only);
    feed(&mut app, "x");
    assert_eq!(text(&app), "ello\n", "editing works again");
}

/// `:help <word>` opens at the line, and inside help `/` searches the
/// page, `n` goes on, and a miss says so instead of moving.
#[test]
fn help_opens_at_a_word_and_searches() {
    let mut app = app_with("x\n");
    app.run_command("help gb");
    let h = app.help.as_ref().expect("help open");
    assert!(h.rows()[h.mark.unwrap()].plain().contains("gb"));
    assert_eq!(h.cats[h.cat].name, "Writing");

    // Tab and Shift-Tab walk the categories; a digit jumps to one.
    feed(&mut app, "1");
    assert_eq!(app.help.as_ref().unwrap().cat, 0);
    app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.help.as_ref().unwrap().cat, 1);
    app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    let h = app.help.as_ref().unwrap();
    assert_eq!(h.cat, h.cats.len() - 1, "round to the last");

    feed(&mut app, "/");
    assert_eq!(app.help.as_ref().unwrap().typing.as_deref(), Some(""));
    feed(&mut app, "heading");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let h = app.help.as_ref().unwrap();
    assert!(h.typing.is_none());
    let first = (h.cat, h.mark.unwrap());
    assert!(h.rows()[first.1].plain().to_lowercase().contains("heading"));
    feed(&mut app, "n");
    let h = app.help.as_ref().unwrap();
    assert_ne!((h.cat, h.mark.unwrap()), first, "n moves on");

    feed(&mut app, "/zzqx");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let h = app.help.as_ref().unwrap();
    assert_eq!(h.status.as_deref(), Some("not found: zzqx"));
    feed(&mut app, "j");
    assert!(app.help.as_ref().unwrap().status.is_none(), "cleared by the next key");

    // Esc while typing drops the search, not the page.
    feed(&mut app, "/abc");
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.help.as_ref().is_some_and(|h| h.typing.is_none()));
    feed(&mut app, "q");
    assert!(app.help.is_none());
}

/// `:view` turns it on and off, and a file opened meanwhile is read-only
/// too.
#[test]
fn view_toggles_from_the_command_line() {
    let mut app = app_with("text\n");
    app.run_command("view");
    assert!(app.view && app.buffer.read_only);
    app.run_command("view");
    assert!(!app.view && !app.buffer.read_only);
    assert!(app.active.contains(0), "the cursor line is raw again");
}

/// A queue that is already full is drained in a handful of batches, not one
/// batch per event — the property that keeps a paste from freezing the
/// editor.
///
/// Every frame is O(lines), so what a paste costs is not any single frame
/// but how many of them it asks for: one per character, drawn between two
/// events that were both already in the queue. 10 KB pasted into a
/// 5 000-line document took a minute of terminal that answered nothing.
///
/// The bound is loose on purpose. One batch is the ordinary answer, but
/// `BATCH_BUDGET` is wall-clock, so a loaded machine is allowed several —
/// what must never come back is the batch-per-event that was the bug.
#[test]
fn a_full_queue_is_drained_in_batches_not_one_event_at_a_time() {
    let mut app = app_with("hello\n");
    feed(&mut app, "A"); // append, so the events below are typed text

    const N: usize = 500;
    let mut queue: std::collections::VecDeque<CtEvent> = (0..N)
        .map(|_| CtEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)))
        .collect();

    let mut batches = 0usize;
    let mut absorbed = 0usize;
    while !queue.is_empty() {
        absorbed += app.absorb_batch(&mut || Ok(queue.pop_front())).unwrap();
        batches += 1;
        assert!(batches <= N, "the loop must make progress");
    }

    assert_eq!(absorbed, N, "every event has to be applied");
    assert!(
        batches <= 50,
        "{N} queued events took {batches} batches — a frame per keystroke is the freeze"
    );
    assert!(
        text(&app).starts_with(&format!("hello{}", "x".repeat(N))),
        "coalescing changes WHEN the screen is drawn, never what the buffer says"
    );
}

/// A batch stops at `:q`. Whatever was typed after it was typed at a buffer
/// that is on its way out, and must not be applied to it.
#[test]
fn a_batch_stops_at_quit() {
    let mut app = app_with("hello\n");
    // `:q!`, because `app_with` leaves the buffer modified and a plain `:q`
    // would rightly refuse it — this test is about the batch, not the guard.
    let mut queue: std::collections::VecDeque<CtEvent> = ":q!\rxxxxx"
        .chars()
        .map(|c| {
            let code = if c == '\r' { KeyCode::Enter } else { KeyCode::Char(c) };
            CtEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
        })
        .collect();

    app.absorb_batch(&mut || Ok(queue.pop_front())).unwrap();
    assert!(app.quit, "`:q` still quits from inside a batch");
    assert_eq!(queue.len(), 5, "the keys after it are left for the shell");
}

// ------------------------------------------------------------ gf / gx

/// A scratch vault: `note.md` holding `body`, plus whatever else the test
/// names. Returns the directory and an app editing the note.
fn vault(body: &str, files: &[(&str, &str)]) -> (PathBuf, App) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("shoin-gf-{t}-{n}"));
    std::fs::create_dir_all(&d).unwrap();
    for (rel, text) in files {
        let p = d.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
    }
    let note = d.join("note.md");
    std::fs::write(&note, body).unwrap();
    let mut app = App::new(Config::default(), Some(note), None).unwrap();
    app.refresh_blocks();
    (d, app)
}

/// The file name of the buffer the app is showing.
fn shown(app: &App) -> String {
    app.buffer
        .path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What the status line is currently saying.
fn flash_text(app: &App) -> String {
    app.flash
        .as_ref()
        .and_then(|f| f.text.clone())
        .unwrap_or_default()
}

/// Put the cursor on the first occurrence of `needle` in line `line`.
fn cursor_on(app: &mut App, line: usize, needle: &str) {
    let text = app.buffer.line_text(line);
    let col = text.find(needle).map(|b| text[..b].chars().count()).unwrap();
    app.buffer.cursor = Cursor::new(line, col);
    app.sync_after_input();
}

/// `gf` on a `[[link]]` opens the note it names.
#[test]
fn gf_opens_the_note_a_wikilink_names() {
    let (_d, mut app) = vault("see [[target]] here\n", &[("target.md", "# Target\n")]);
    cursor_on(&mut app, 0, "target");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "target.md", "the link's note is the open buffer");
}

/// The `!` of an unexpanded `![[…]]` is not part of the scanner's span —
/// the scan leaves it a character and reads the wiki link after it. A
/// reader whose cursor is on the bang still means the embed.
#[test]
fn gf_follows_an_embed_from_its_bang() {
    let (_d, mut app) = vault("![[frag]]\n", &[("frag.md", "# Frag\n")]);
    cursor_on(&mut app, 0, "!");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "frag.md", "the bang belongs to the embed after it");
}

/// A link is written before its note is. Following a dead one WRITES the
/// note, so the link that made it resolves from then on.
#[test]
fn gf_creates_a_note_that_does_not_exist_yet() {
    let (d, mut app) = vault("plan: [[tomorrow]]\n", &[]);
    cursor_on(&mut app, 0, "tomorrow");
    feed(&mut app, "gf");

    let made = d.join("tomorrow.md");
    assert!(made.is_file(), "the note is on disk, not just in a buffer");
    assert_eq!(std::fs::read_to_string(&made).unwrap(), "", "and it is empty");
    assert_eq!(shown(&app), "tomorrow.md", "and open");

    // The point of writing it to disk: the same link now resolves.
    let l = link::Link::parse("tomorrow").unwrap();
    assert!(
        link::resolve(&l, &app.link_from(), &app.link_root()).is_ok(),
        "the link that created the note now finds it"
    );
}

/// A note that is a link to a missing file is not created at the link's
/// far end: `gf` says the link is broken and makes nothing.
#[cfg(unix)]
#[test]
fn gf_on_a_broken_link_says_so_and_creates_nothing() {
    let (d, mut app) = vault("plan: [[tomorrow]]\n", &[]);
    std::os::unix::fs::symlink(d.join("gone.md"), d.join("tomorrow.md")).unwrap();
    cursor_on(&mut app, 0, "tomorrow");
    feed(&mut app, "gf");

    let flash = flash_text(&app);
    assert!(flash.contains("tomorrow.md is a broken link"), "got {flash:?}");
    assert!(std::fs::symlink_metadata(d.join("gone.md")).is_err(), "nothing at the far end");
    assert!(std::fs::symlink_metadata(d.join("tomorrow.md")).unwrap().file_type().is_symlink());
}

/// `#Heading` lands ON the heading, not at the top of the file.
#[test]
fn gf_lands_on_the_section_a_link_asks_for() {
    let (_d, mut app) = vault(
        "[[frag#Second]]\n",
        &[("frag.md", "# First\n\nbody\n\n## Second\n\nmore\n")],
    );
    cursor_on(&mut app, 0, "frag");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "frag.md");
    assert_eq!(
        app.buffer.line_text(app.buffer.cursor.line).trim(),
        "## Second",
        "the cursor is on the heading the link named"
    );
}

/// A heading that was renamed away still opens the note — landing at its
/// top beats refusing to move.
#[test]
fn a_missing_section_still_opens_the_note() {
    let (_d, mut app) = vault("[[frag#Gone]]\n", &[("frag.md", "# First\n")]);
    cursor_on(&mut app, 0, "frag");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "frag.md");
    assert_eq!(app.buffer.cursor.line, 0);
}

/// `resolve` REFUSES a bare name several files answer to. The refusal is
/// turned into a choice rather than an error.
#[test]
fn an_ambiguous_name_opens_the_finder() {
    let (_d, mut app) = vault(
        "[[dup]]\n",
        &[("a/dup.md", "# A\n"), ("b/dup.md", "# B\n")],
    );
    cursor_on(&mut app, 0, "dup");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "note.md", "nothing is opened until one is picked");
    let f = app.finder.as_ref().expect("the finder is offering the candidates");
    assert_eq!(f.file_count(), 2, "both notes are in it");
}

/// A markdown link is a PATH, resolved relative to the edited file.
#[test]
fn gf_opens_the_path_in_a_markdown_link() {
    let (_d, mut app) = vault(
        "see [the notes](sub/other.md)\n",
        &[("sub/other.md", "# Other\n")],
    );
    cursor_on(&mut app, 0, "sub/other.md");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "other.md");
}

/// A link inside emphasis is the one followed: the scan draws the
/// emphasis as one span, and `gf` reads it again for the link.
#[test]
fn gf_follows_a_link_inside_emphasis() {
    for line in ["**see [the notes](sub/other.md)**\n", "_a ==b [the notes](sub/other.md)== c_\n"] {
        let (_d, mut app) = vault(line, &[("sub/other.md", "# Other\n")]);
        cursor_on(&mut app, 0, "notes");
        feed(&mut app, "gf");
        assert_eq!(shown(&app), "other.md", "{line:?}");
    }
}

/// `gf` never launches anything. A URL is reported, and `gx` is named —
/// which is how the other key gets learned.
#[test]
fn gf_refuses_a_url_and_names_gx() {
    let (_d, mut app) = vault("read https://example.com/x today\n", &[]);
    cursor_on(&mut app, 0, "https");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "note.md", "the buffer did not change");
    let flash = flash_text(&app);
    assert!(flash.contains("gx"), "the message points at gx — got {flash:?}");
}

/// A dot in a note's NAME is not an extension: a dead dated link creates
/// the note the link resolves to next time, `.md` appended to the whole
/// name rather than replacing its last part.
#[test]
fn gf_creates_a_dated_note() {
    for (name, file) in [
        ("2024.01.16 meeting", "2024.01.16 meeting.md"),
        ("2024.01.16", "2024.01.16.md"),
        ("Mr. Smith", "Mr. Smith.md"),
        ("Dr.Who", "Dr.Who.md"),
    ] {
        let (d, mut app) = vault(&format!("see [[{name}]]\n"), &[]);
        cursor_on(&mut app, 0, name);
        feed(&mut app, "gf");
        assert!(d.join(file).is_file(), "{name}: created {file}, got {}", flash_text(&app));
        assert_eq!(shown(&app), file, "{name}: and open");
        let l = link::Link::parse(name).unwrap();
        assert_eq!(
            link::resolve(&l, &app.link_from(), &app.link_root()).ok(),
            Some(d.join(file)),
            "{name}: the link finds what gf made"
        );
    }
}

/// A dead link to something that is not text does NOT get an empty file
/// written in its place.
#[test]
fn a_missing_non_note_is_never_created() {
    let (d, mut app) = vault("[the paper](out/paper.pdf)\n", &[]);
    cursor_on(&mut app, 0, "out/paper.pdf");
    feed(&mut app, "gf");
    assert!(!d.join("out/paper.pdf").exists(), "no empty pdf was invented");
    assert_eq!(shown(&app), "note.md");
}

/// The same for a wiki link: `[[report.pdf]]` names a PDF, so a missing
/// one is reported, not turned into a note called `report.pdf.md`.
#[test]
fn a_missing_non_note_wiki_link_is_never_created() {
    for name in ["report.pdf", "Data.CSV", "song.mp3"] {
        let (d, mut app) = vault(&format!("see [[{name}]]\n"), &[]);
        cursor_on(&mut app, 0, name);
        feed(&mut app, "gf");
        assert!(!d.join(name).exists(), "{name}: no empty file was invented");
        assert!(!d.join(format!("{name}.md")).exists(), "{name}: no note either");
        assert_eq!(shown(&app), "note.md", "{name}: the buffer did not change");
        let flash = flash_text(&app);
        assert!(flash.contains(name), "{name}: the miss is reported, got {flash:?}");
    }
}

/// Off a link, `gf` says so rather than guessing at the word under the
/// cursor.
#[test]
fn gf_off_a_link_does_nothing() {
    let (_d, mut app) = vault("just some prose here\n", &[]);
    cursor_on(&mut app, 0, "prose");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "note.md");
    assert!(flash_text(&app).contains("no link"), "got {:?}", flash_text(&app));
}

/// `<C-^>` is the way back out of a followed link, and pressing it twice
/// lands where it started — which is what makes it usable for reading two
/// notes against each other.
#[test]
fn ctrl_caret_goes_back_and_forth() {
    let (_d, mut app) = vault("[[target]]\n", &[("target.md", "# Target\n")]);
    cursor_on(&mut app, 0, "target");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "target.md");

    ctrl(&mut app, '^');
    assert_eq!(shown(&app), "note.md", "back to the note that linked here");
    ctrl(&mut app, '^');
    assert_eq!(shown(&app), "target.md", "and back again");
}

/// What line `line` of the current document draws as an embed, row by row,
/// after syncing its render cache the way a frame does.
fn embed_rows(app: &mut App, line: usize) -> Vec<String> {
    app.refresh_blocks();
    embed_rows_of(app, app.current(), line)
}

/// The same for document `doc`, which need not be the current one.
fn embed_rows_of(app: &App, doc: usize, line: usize) -> Vec<String> {
    use crate::transclude::preview::Row;
    let mut cache = app.docs[doc].cache.borrow_mut();
    cache.sync_doc(app, &app.docs[doc], app.focus_pane, 60);
    let Some(x) = cache.entry(line).and_then(|e| e.embed.as_ref()) else {
        return Vec::new();
    };
    x.rows
        .iter()
        .filter_map(|r| match r {
            Row::Body(s, _, _) => Some(s.clone()),
            Row::Error(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

/// Follow the embed on line 1 with `gf`, replace the note's text, and `:w`.
fn write_through_an_embed(app: &mut App, text: &str) {
    cursor_on(app, 1, "fragments");
    embed_rows(app, 1); // a frame on the link line, as the reader saw it
    feed(app, "gf");
    assert_eq!(shown(app), "next.md");
    // A frame of the embedding note after the switch, as a split showing
    // it would draw: from here only the write can tell it to re-read.
    embed_rows_of(app, 0, 1);
    feed(app, "ggdGi");
    feed(app, text);
    esc(app);
    feed(app, ":w");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    assert!(flash_text(app).contains("written"), "got {:?}", flash_text(app));
}

/// The bug as found: an embed of a note that did not exist yet kept saying
/// "no note called …" after `gf` had created the note and it was written.
/// The expansion is read from disk but cached with the LINE, and the line
/// had not changed.
#[test]
fn an_embed_of_a_missing_note_shows_it_once_it_is_created_and_written() {
    let (_d, mut app) = vault("intro\n![[fragments/next]]\n", &[]);
    app.embed_mode = crate::transclude::Mode::Full;
    let before = embed_rows(&mut app, 1);
    assert!(before[0].contains("no note called"), "got {before:?}");

    write_through_an_embed(&mut app, "Next: protect two hours.");
    ctrl(&mut app, '^');
    feed(&mut app, "gg"); // off the link line, so it expands
    assert_eq!(embed_rows(&mut app, 1), ["Next: protect two hours."]);
}

/// The same for a note that already existed: the embed shows what the note
/// was last written as.
#[test]
fn an_embed_shows_what_its_note_was_last_written_as() {
    let (_d, mut app) = vault("intro\n![[fragments/next]]\n", &[("fragments/next.md", "old\n")]);
    app.embed_mode = crate::transclude::Mode::Full;
    assert_eq!(embed_rows(&mut app, 1), ["old"]);

    write_through_an_embed(&mut app, "new");
    ctrl(&mut app, '^');
    feed(&mut app, "gg");
    assert_eq!(embed_rows(&mut app, 1), ["new"]);
}

/// A document that stays on screen in another pane is never switched back
/// to, so the write itself has to reach its embeds.
#[test]
fn a_write_refreshes_embeds_in_a_document_left_on_screen() {
    let (_d, mut app) = vault("intro\n![[fragments/next]]\n", &[("fragments/next.md", "old\n")]);
    app.embed_mode = crate::transclude::Mode::Full;
    assert_eq!(embed_rows_of(&app, 0, 1), ["old"]);

    write_through_an_embed(&mut app, "new");
    assert_eq!(shown(&app), "next.md", "still in the embedded note");
    assert_eq!(embed_rows_of(&app, 0, 1), ["new"], "the embedding note re-read it");
}

/// Terminals disagree about which of the two chords they send.
#[test]
fn ctrl_6_is_the_same_binding() {
    let (_d, mut app) = vault("[[target]]\n", &[("target.md", "# Target\n")]);
    cursor_on(&mut app, 0, "target");
    feed(&mut app, "gf");
    ctrl(&mut app, '6');
    assert_eq!(shown(&app), "note.md");
}

/// A `6` with CONTROL is a binding, not the start of a count — `as_char`
/// refuses a modified key, so `6G` still means line 6.
#[test]
fn ctrl_6_is_not_swallowed_as_a_count() {
    let mut app = app_with("a\nb\nc\nd\ne\nf\ng\n");
    feed(&mut app, "6G");
    assert_eq!(app.buffer.cursor.line, 5, "6G is still line 6");
}

/// With nowhere to go back to, it says so rather than moving.
#[test]
fn ctrl_caret_with_no_alternate_says_so() {
    let (_d, mut app) = vault("nothing here\n", &[]);
    ctrl(&mut app, '^');
    assert_eq!(shown(&app), "note.md");
    assert!(flash_text(&app).contains("no alternate"), "got {:?}", flash_text(&app));
}

/// Closing a document must not leave the alternate pointing at it, nor at
/// whatever slid into its index.
#[test]
fn closing_a_buffer_clears_it_as_an_alternate() {
    let (_d, mut app) = vault("[[target]]\n", &[("target.md", "# Target\n")]);
    cursor_on(&mut app, 0, "target");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "target.md");

    // Close target.md: the note that linked to it is all that is left, so
    // there is nowhere to go back to.
    app.close_buffer(true);
    assert_eq!(shown(&app), "note.md");
    ctrl(&mut app, '^');
    assert_eq!(shown(&app), "note.md", "the closed document is not an alternate");
    assert!(flash_text(&app).contains("no alternate"), "got {:?}", flash_text(&app));
}

/// Nothing written in a note can reach a flag on the opener's argv.
#[test]
fn gx_refuses_a_target_that_looks_like_a_flag() {
    let (_d, mut app) = vault("x\n", &[]);
    app.spawn_opener("-a/Applications/Anything.app");
    let flash = flash_text(&app);
    assert!(flash.contains("refusing"), "got {flash:?}");
}

/// A file opened by bare name (`shoin notes.md`) has an EMPTY parent path, not
/// a cwd — so the finder root must fall back to the cwd rather than "", which
/// reads as no directory and opens an empty list.
#[test]
fn file_dir_of_a_bare_filename_is_a_real_directory() {
    let mut app = app_with("x\n");
    app.buffer.path = Some(PathBuf::from("notes.md"));
    let dir = app.file_dir();
    assert!(!dir.as_os_str().is_empty(), "root must not be the empty path");
    assert!(dir.is_dir(), "{} should be a directory", dir.display());
}

/// `[tree] show_hidden = true` means the tree opens with dotfiles already
/// listed — no `H` needed.
#[test]
fn the_config_key_seeds_the_hidden_state() {
    let mut app = app_with("x\n");
    app.config.tree.show_hidden = true;
    app.toggle_tree(app.cwd());
    assert!(app.tree.as_ref().unwrap().show_hidden);
}

/// `H` is the one piece of tree state that outlives the panel: close the
/// tree and reopen it and the dotfiles are still there.
#[test]
fn the_hidden_toggle_survives_closing_the_tree() {
    let mut app = app_with("x\n");
    let root = app.cwd();

    app.toggle_tree(root.clone());
    assert!(!app.tree.as_ref().unwrap().show_hidden, "off to start with");
    app.on_key_tree(KeyEvent::new(KeyCode::Char('H'), KeyModifiers::NONE));
    assert!(app.tree.as_ref().unwrap().show_hidden);

    // Focused → close, then open again: the state came back with it.
    app.toggle_tree(root.clone());
    assert!(app.tree.is_none(), "second press closes it");
    app.toggle_tree(root);
    assert!(
        app.tree.as_ref().unwrap().show_hidden,
        "H is remembered across open/close"
    );
}

/// The two tree keys must land in DIFFERENT places — that is their whole
/// point. `fe` roots at the file's own folder, `fE` at `$HOME`.
#[test]
fn the_tree_keys_root_in_different_places() {
    let mut app = app_with("x\n");
    let home = app.home();
    app.buffer.path = Some(home.join("notes").join("today.md"));

    assert_eq!(app.root_dir(Root::File), home.join("notes"));
    assert_eq!(app.root_dir(Root::Home), home);
    assert_ne!(
        app.root_dir(Root::File),
        app.root_dir(Root::Home),
        "fe and fE must not open the same listing"
    );
}

/// `tree.root = "cwd"` moves the plain tree and finder to the launch
/// folder, so a vault opens whole; the `_cwd` actions go there whatever
/// the setting, and `_home` is untouched by it.
#[test]
fn tree_root_cwd_opens_the_launch_folder() {
    let mut app = app_with("x\n");
    let home = app.home();
    app.buffer.path = Some(home.join("vault").join("sub").join("note.md"));
    let near = home.join("vault").join("sub");

    assert_eq!(app.root_dir(Root::File), near, "the default is the file's folder");
    assert_eq!(app.root_dir(Root::Cwd), app.cwd());

    app.config.tree.root = "cwd".into();
    assert_eq!(app.root_dir(Root::File), app.cwd());
    assert_eq!(app.root_dir(Root::Home), home);

    app.config.tree.root = "nonsense".into();
    assert_eq!(app.root_dir(Root::File), near, "an unknown word falls back");
}

/// `refresh_blocks` rescans from the edited line instead of rebuilding.
/// The incremental result must be indistinguishable from a full build after
/// edits that change a line's kind, open a fence, and delete lines.
#[test]
fn incremental_block_refresh_matches_full_build() {
    let mut app = app_with("# Title\n\npara one\n\n```\ncode\n```\n\ntail\n");

    // Turn a paragraph into a heading (kind change, no carry change).
    feed(&mut app, "jjI## ");
    esc(&mut app);
    app.refresh_blocks();
    assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width).kinds);

    // Delete the fence opener — every following line's carry shifts.
    feed(&mut app, "jjdd");
    app.refresh_blocks();
    assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width).kinds);
    assert_eq!(app.blocks.revision, app.buffer.revision);

    // Undo puts it back; undo bypasses `touch`, so it must mark its own
    // dirty line or the cache silently keeps the post-delete carries.
    app.buffer.undo();
    app.refresh_blocks();
    assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width).kinds);
}

/// An edit that keeps the line count can change lines well below the
/// first one it touched. Moving `9. i` past `10. j` below other text left
/// the cache calling `9. i` a paragraph, so the renumber skipped it and
/// wrote `9. j` / `9. i`.
#[test]
fn a_block_swap_below_the_top_refreshes_every_line_it_moved() {
    let mut app = app_with("intro\n\n9. i\n    more\n10. j\n");
    feed(&mut app, "jj");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "intro\n\n9. j\n10. i\n    more\n");
    app.refresh_blocks();
    assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width).kinds);

    // The same through undo and redo, which edit the rope directly.
    feed(&mut app, "u");
    app.refresh_blocks();
    assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width).kinds);
    app.buffer.redo();
    app.refresh_blocks();
    assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width).kinds);

    // And an indent of several lines, which keeps the count too. The
    // setup's own edit is forgotten first, or the rescan would start at
    // line 0 and rebuild everything.
    let mut app = app_with("intro\n\n- a\n- b\n- c\n- d\n");
    app.refresh_blocks();
    feed(&mut app, "3jVj>");
    app.refresh_blocks();
    assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width).kinds);
}

/// `--zen` and `:zen` live on `App`, so a config reload cannot forget them,
/// and leaving zen restores the user's own settings rather than defaults.
#[test]
fn zen_survives_a_config_reload() {
    let mut app = app_with("x\n");
    app.config.status.enabled = true;
    app.config.layout.scroll_hint = true;

    app.set_zen(true);
    assert!(!app.config.status.enabled);
    assert!(!app.config.layout.scroll_hint);

    // A reload swaps in a freshly parsed config, chrome and all.
    app.config = Config::default();
    app.reapply_zen();
    assert!(!app.config.status.enabled, "zen must survive the reload");

    app.set_zen(false);
    assert!(
        app.config.status.enabled,
        "leaving zen restores what the config asked for"
    );
}

/// One `u` takes back a word, not the whole insert session: the step is
/// split at each word boundary.
#[test]
fn undo_splits_an_insert_session_at_word_boundaries() {
    let mut app = app_with("");
    feed(&mut app, "ihello world");
    esc(&mut app);
    assert_eq!(text(&app), "hello world");
    feed(&mut app, "u");
    assert_eq!(text(&app), "hello");
    feed(&mut app, "u");
    assert_eq!(text(&app), "");
}

/// Undoing back to what is on disk marks the buffer unmodified again — a
/// monotonic revision counter cannot express that, so history positions do.
#[test]
fn undoing_back_to_the_saved_state_clears_modified() {
    let mut app = app_with("one\n");
    let path = std::env::temp_dir().join(format!(
        "shoin-saved-{}.md",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    app.buffer.save_as(path.clone(), SavePolicy::default(), false).unwrap();
    assert!(!app.buffer.modified, "a fresh save is clean");

    feed(&mut app, "x");
    assert!(app.buffer.modified, "an edit dirties it");

    feed(&mut app, "u");
    assert!(!app.buffer.modified, "undoing back to disk cleans it");

    app.buffer.redo();
    assert!(app.buffer.modified, "redoing dirties it again");
    let _ = std::fs::remove_file(path);
}

/// A write refuses to clobber a file something else changed
/// underneath us, and `:w!` is the way past it. Without this the recorded
/// `disk_mtime` was bookkeeping nobody read.
#[test]
fn a_write_refuses_to_clobber_an_external_change() {
    let mut app = app_with("mine\n");
    let path = std::env::temp_dir().join(format!(
        "shoin-clobber-{}.md",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    app.buffer.save_as(path.clone(), SavePolicy::default(), false).unwrap();

    // Someone else writes the file. `set_modified` rather than a sleep, so
    // the mtime differs by construction and the test cannot flake on a
    // coarse clock.
    std::fs::write(&path, "theirs\n").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    f.set_modified(later).unwrap();
    drop(f);

    let err = app.buffer.save(SavePolicy::default(), false).unwrap_err().to_string();
    assert!(err.contains("changed on disk"), "got: {err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n", "not clobbered");

    app.buffer.save(SavePolicy::default(), true).expect(":w! overrides the guard");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");

    // The write recorded a fresh mtime, so the next plain :w is fine again.
    app.buffer.save(SavePolicy::default(), false).expect("guard rearms against the new mtime");
    let _ = std::fs::remove_file(path);
}

// ------------------------------------------------- read-only and the disk

/// Read-only mode takes a change made on disk, as one undo step that `u`
/// gives back once editing again.
#[test]
fn read_only_takes_changes_from_disk() {
    let (mut app, path) = app_on_disk("ro-check", "mine\n");
    app.set_view(true);
    write_settled(&path, "theirs\n");
    assert!(app.check_disk(), "the change was noticed");
    assert_eq!(text(&app), "theirs\n", "and taken");
    assert!(!app.buffer.modified, "the buffer matches the file");
    assert!(app.buffer.read_only, "still read-only");
    app.set_view(false);
    feed(&mut app, "u");
    assert_eq!(text(&app), "mine\n", "one undo step");
    let _ = std::fs::remove_file(path);
}

/// `:revert!` in read-only mode re-reads the file too.
#[test]
fn read_only_reverts_from_disk() {
    let (mut app, path) = app_on_disk("ro-revert", "mine\n");
    app.set_view(true);
    write_settled(&path, "theirs\n");
    ex(&mut app, "revert!");
    assert_eq!(text(&app), "theirs\n");
    assert!(flash_text(&app).contains("reverted"), "got: {}", flash_text(&app));
    assert!(app.buffer.read_only, "still read-only");
    let _ = std::fs::remove_file(path);
}

/// `w` in the diff view writes nothing in read-only mode. The merge
/// cannot go into a buffer that refuses edits, and writing the buffer
/// as it stands would put the unmerged text over the file's.
///
/// The view stays open with every choice the reader made: closing it
/// would throw those away for a refusal they can do nothing about inside
/// it. The refusal is drawn in the view (see `render_diff`).
#[test]
fn read_only_diff_commit_writes_nothing() {
    let (mut app, path) = app_on_disk("ro-diff", "mine\n");
    feed(&mut app, "A edited");
    esc(&mut app);
    write_settled(&path, "theirs\n");
    app.set_view(true);
    app.open_diff();
    assert!(app.diff.is_some(), "a read-only buffer can still be compared");
    // `:view` flashed a message; the view would show it as its own.
    assert!(app.flash.is_none(), "an older message does not follow it in");
    diff_key(&mut app, 'f');
    diff_key(&mut app, 'w');
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n", "the file is untouched");
    assert_eq!(text(&app), "mine edited\n", "so is the buffer");
    assert!(app.buffer.modified, "and its changes are still unsaved");
    let view = app.diff.as_ref().expect("the view stays open");
    assert!(
        view.sides.iter().all(|s| *s == crate::diff::Side::File),
        "with the choices made in it: {:?}",
        view.sides
    );
    assert!(flash_text(&app).contains("read-only"), "got: {}", flash_text(&app));
    assert!(flash_text(&app).contains("W "), "and names the way on: {}", flash_text(&app));
    // `q` still leaves it, and nothing was written on the way out.
    diff_key(&mut app, 'q');
    assert!(app.diff.is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n");
    assert!(app.buffer.read_only);
    let _ = std::fs::remove_file(path);
}

/// `W` after that refusal is the way on: it leaves read-only mode and
/// writes the merge the view's choices add up to.
#[test]
fn read_only_diff_shift_w_leaves_read_only_and_writes() {
    let (mut app, path) = app_on_disk("ro-diff-w", "mine\n");
    feed(&mut app, "A edited");
    esc(&mut app);
    write_settled(&path, "theirs\n");
    app.set_view(true);
    app.open_diff();
    diff_key(&mut app, 'b');
    diff_key(&mut app, 'w');
    assert!(app.diff.is_some(), "refused, still open");
    diff_key(&mut app, 'W');
    assert!(app.diff.is_none(), "written, so closed");
    assert!(!app.buffer.read_only, "read-only mode is off");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine edited\ntheirs\n");
    assert_eq!(text(&app), "mine edited\ntheirs\n");
    assert!(!app.buffer.modified);
    let _ = std::fs::remove_file(path);
}

// ------------------------------------------------------------ diff view

fn diff_key(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

/// `:diff` on a file that matches says so and opens nothing. A view of no
/// differences is a window a reader has to close for no reason.
#[test]
fn diff_on_a_matching_file_opens_nothing() {
    let (mut app, path) = app_on_disk("same", "one\ntwo\n");
    app.open_diff();
    assert!(app.diff.is_none());
    assert!(flash_text(&app).contains("matches the file"), "got: {}", flash_text(&app));
    let _ = std::fs::remove_file(path);
}

/// `:w` with `trim_on_save` trims the buffer as it trims the file, so
/// `:diff` straight after finds nothing; and `.` still repeats the edit
/// made before the save, not the `:w`.
#[test]
fn a_trimming_save_leaves_the_buffer_matching_and_dot_alone() {
    let (mut app, path) = app_on_disk("trim", "one two\nx \nthree four\n");
    app.config.editor.trim_on_save = true;
    feed(&mut app, "dw:w");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\nx\nthree four\n");
    assert_eq!(text(&app), "two\nx\nthree four\n");
    app.open_diff();
    assert!(flash_text(&app).contains("matches the file"), "got: {}", flash_text(&app));

    feed(&mut app, "jj.");
    assert_eq!(text(&app), "two\nx\nfour\n", "`.` is still `dw`");
    let _ = std::fs::remove_file(path);
}

/// An autosave that trims mid-command is the save's edit, not the
/// command's: a `:noh` or a Visual yank around it must not become `.`.
#[test]
fn an_autosave_trim_mid_command_is_not_what_dot_repeats() {
    for (around, then) in [(":", "noh\r"), ("v", "ly")] {
        let (mut app, path) = app_on_disk("trim-dot", "one two three\nx \n");
        app.config.editor.trim_on_save = true;
        feed(&mut app, "dw");
        feed(&mut app, around);
        autosave_due(&mut app);
        assert!(app.autosave_tick());
        assert_eq!(text(&app), "two three\nx\n", "trimmed by the autosave");
        for c in then.chars() {
            let code = if c == '\r' { KeyCode::Enter } else { KeyCode::Char(c) };
            app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
            app.sync_after_input();
        }
        esc(&mut app);
        app.buffer.cursor = Cursor::new(0, 0);
        feed(&mut app, ".");
        assert_eq!(text(&app), "three\nx\n", "`.` is still `dw` after {around:?}");
        let _ = std::fs::remove_file(path);
    }
}

/// The view aligns the two versions: an edited line is one row carrying
/// both, and an insertion is a row with a filler on the buffer's side.
#[test]
fn diff_opens_aligned_on_the_first_difference() {
    let (mut app, path) = app_on_disk("open", "keep\nmine\ntail\n");
    write_settled(&path, "keep\ntheirs\nextra\ntail\n");

    app.open_diff();
    let view = app.diff.as_ref().expect("the view opened");
    assert_eq!(view.mine, vec!["keep", "mine", "tail"]);
    assert_eq!(view.theirs, vec!["keep", "theirs", "extra", "tail"]);
    assert_eq!(
        view.align.rows,
        vec![
            crate::diff::Row::Same { left: 0, right: 0 },
            crate::diff::Row::Changed { left: 1, right: 1 },
            crate::diff::Row::Added { right: 2 },
            crate::diff::Row::Same { left: 2, right: 3 },
        ]
    );
    assert_eq!(view.align.hunks, vec![1..3], "one difference, two rows");
    let _ = std::fs::remove_file(path);
}

/// `q` and `<Esc>` close it and change nothing at all — the escape a reader
/// needs before they will risk pressing anything else.
#[test]
fn diff_closes_without_touching_either_side() {
    let (mut app, path) = app_on_disk("close", "mine\n");
    feed(&mut app, "A edited");
    esc(&mut app);
    write_settled(&path, "theirs\n");
    let before = text(&app);

    app.open_diff();
    assert!(app.diff.is_some());
    diff_key(&mut app, 'q');
    assert!(app.diff.is_none(), "q closes it");
    assert_eq!(text(&app), before, "the buffer is untouched");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n", "so is the file");

    app.open_diff();
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.diff.is_none(), "and so does Esc");
    let _ = std::fs::remove_file(path);
}

/// `l` / `f` / `b` SELECT for every difference; they do not commit. A key
/// that both selected and wrote would silently discard whatever the reader
/// had already toggled.
#[test]
fn the_bulk_keys_select_without_committing() {
    use crate::diff::Side;
    let (mut app, path) = app_on_disk("bulk", "shared\n");
    feed(&mut app, "A mine");
    esc(&mut app);
    write_settled(&path, "theirs\n");
    app.open_diff();

    for (key, want) in [('f', Side::File), ('b', Side::Both), ('l', Side::Live)] {
        diff_key(&mut app, key);
        let view = app.diff.as_ref().expect("still open — nothing was committed");
        assert!(view.sides.iter().all(|s| *s == want), "{key} set every difference");
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theirs\n",
        "and the file is untouched until w"
    );
    let _ = std::fs::remove_file(path);
}

/// `<Tab>` cycles the CURRENT difference only, through live → file → both.
/// Not `<Space>`: space is the leader key everywhere else in the editor.
#[test]
fn tab_cycles_only_the_current_difference() {
    use crate::diff::Side;
    let (mut app, path) = app_on_disk("tab", "a\nb\nc\nd\ne\n");
    write_settled(&path, "a\nB\nc\nD\ne\n");
    app.term_size = (80, 24);
    app.open_diff();
    assert_eq!(app.diff.as_ref().unwrap().sides, vec![Side::Live, Side::Live]);

    let tab = |app: &mut App| app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    tab(&mut app);
    assert_eq!(app.diff.as_ref().unwrap().sides, vec![Side::File, Side::Live]);
    tab(&mut app);
    assert_eq!(app.diff.as_ref().unwrap().sides, vec![Side::Both, Side::Live]);
    tab(&mut app);
    assert_eq!(app.diff.as_ref().unwrap().sides, vec![Side::Live, Side::Live], "wraps");

    // Move on, and the next Tab leaves the first difference alone.
    diff_key(&mut app, 'n');
    tab(&mut app);
    assert_eq!(app.diff.as_ref().unwrap().sides, vec![Side::Live, Side::File]);
    let _ = std::fs::remove_file(path);
}

/// `w` commits the WHOLE merged document — as one undo step, written once,
/// with the conflict settled.
#[test]
fn w_commits_the_merge_as_one_undo_step() {
    use crate::diff::Side;
    let (mut app, path) = app_on_disk("commit", "a\nmine\nc\nx\ne\n");
    feed(&mut app, "A ");
    esc(&mut app);
    app.buffer.replace_all("a\nmine\nc\nx\ne\n");
    write_settled(&path, "a\ntheirs\nc\ny\ne\n");
    assert!(app.check_disk());
    assert!(app.buffer.conflict);
    let before = text(&app);

    app.open_diff();
    let view = app.diff.as_mut().unwrap();
    assert_eq!(view.align.hunks.len(), 2);
    view.sides[0] = Side::File;
    view.sides[1] = Side::Both;

    app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
    assert!(app.diff.is_none(), "the view closes behind the commit");
    assert_eq!(text(&app), "a\ntheirs\nc\nx\ny\ne\n", "one hunk each way");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "a\ntheirs\nc\nx\ny\ne\n",
        "and it reached the file"
    );
    assert!(!app.buffer.modified, "written, so clean");
    assert!(!app.buffer.conflict, "and settled");

    feed(&mut app, "u");
    assert_eq!(text(&app), before, "one u puts the whole merge back");
    let _ = std::fs::remove_file(path);
}

/// A THIRD write during the decision must not be clobbered. `check_disk`
/// freezes the document, but nothing stops another process touching the
/// file while the reader deliberates, and `w` forces.
#[test]
fn w_refuses_when_the_file_moved_under_the_view() {
    let (mut app, path) = app_on_disk("third", "mine\n");
    write_settled(&path, "theirs\n");
    app.open_diff();

    write_settled(&path, "somebody else\n");
    app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
    assert!(app.diff.is_none(), "the stale view is closed");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "somebody else\n",
        "the third write survives"
    );
    assert!(flash_text(&app).contains("changed again"), "got: {}", flash_text(&app));
    let _ = std::fs::remove_file(path);
}

/// `q` after toggling is still a true abort: selections were never applied.
#[test]
fn q_abandons_selections_without_writing() {
    use crate::diff::Side;
    let (mut app, path) = app_on_disk("abort", "mine\n");
    write_settled(&path, "theirs\n");
    app.open_diff();
    app.diff.as_mut().unwrap().set_all(Side::Both);

    diff_key(&mut app, 'q');
    assert!(app.diff.is_none());
    assert_eq!(text(&app), "mine\n", "the buffer never changed");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n", "nor the file");
    let _ = std::fs::remove_file(path);
}

/// `n` and `p` step between differences and wrap. Wrapping matters: past
/// the last difference a reader means "show me again", not "do nothing".
#[test]
fn diff_steps_between_differences_and_wraps() {
    let (mut app, path) = app_on_disk("steps", "a\nb\nc\nd\ne\n");
    write_settled(&path, "a\nB\nc\nD\ne\n");
    app.term_size = (80, 24);
    app.open_diff();
    assert_eq!(app.diff.as_ref().unwrap().align.hunks.len(), 2);
    assert_eq!(app.diff.as_ref().unwrap().hunk, 0, "opens on the first");

    diff_key(&mut app, 'n');
    assert_eq!(app.diff.as_ref().unwrap().hunk, 1);
    diff_key(&mut app, 'n');
    assert_eq!(app.diff.as_ref().unwrap().hunk, 0, "wraps forward");
    diff_key(&mut app, 'p');
    assert_eq!(app.diff.as_ref().unwrap().hunk, 1, "and backward");

    // The view is still open: neither key resolves anything on its own.
    assert!(app.diff.is_some());
    let _ = std::fs::remove_file(path);
}

/// A document under review is FROZEN: the comparison must not move because
/// a background poll noticed a third write mid-decision.
#[test]
fn a_document_under_review_is_left_alone_by_the_poll() {
    let (mut app, path) = app_on_disk("frozen", "mine\n");
    write_settled(&path, "theirs\n");
    app.open_diff();
    let snapshot = app.diff.as_ref().unwrap().theirs.clone();

    // Something writes it a third time while the reader is deciding.
    write_settled(&path, "third\n");
    assert!(!app.check_disk(), "the poll steps over it");
    assert_eq!(text(&app), "mine\n", "the buffer did not move");
    assert_eq!(
        app.diff.as_ref().unwrap().theirs,
        snapshot,
        "and neither did the comparison"
    );
    let _ = std::fs::remove_file(path);
}

/// A buffer with no file has nothing to compare against, and one whose file
/// is not written yet says which of the two problems it has.
#[test]
fn diff_needs_a_file_to_compare_against() {
    let mut app = app_with("scratch\n");
    app.open_diff();
    assert!(app.diff.is_none());
    assert!(flash_text(&app).contains("no file"), "got: {}", flash_text(&app));

    app.buffer.path = Some(std::env::temp_dir().join("shoin-diff-absent-xyz.md"));
    app.open_diff();
    assert!(app.diff.is_none());
    assert!(flash_text(&app).contains("not on disk"), "got: {}", flash_text(&app));
}

// ----------------------------------------------------------- autoreload

/// Write `text` to `path` and backdate its mtime past `DISK_SETTLE`.
///
/// Backdating is not a convenience: a file written THIS instant is
/// deliberately deferred by `check_disk`, so a test that skipped this would
/// be testing the settle window rather than the reload.
fn write_settled(path: &std::path::Path, text: &str) {
    std::fs::write(path, text).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() - Duration::from_secs(5))
        .unwrap();
}

/// An app holding `text`, saved to a scratch path, clean and mtime-stamped.
fn app_on_disk(tag: &str, text: &str) -> (App, std::path::PathBuf) {
    let mut app = app_with(text);
    let path = saved_file(&mut app, tag);
    (app, path)
}

/// Case 1: the file changed, the buffer did not — the file wins.
#[test]
fn a_clean_buffer_takes_the_file_changes() {
    let (mut app, path) = app_on_disk("clean", "mine\n");
    app.buffer.cursor = Cursor::new(0, 2);
    write_settled(&path, "theirs\n");

    assert!(app.check_disk(), "the change was noticed");
    assert_eq!(text(&app), "theirs\n", "the file won");
    assert!(!app.buffer.modified, "and the buffer matches disk, so it is clean");
    assert!(!app.buffer.conflict, "nothing to resolve");
    assert!(flash_text(&app).contains("reloaded"), "got: {}", flash_text(&app));
    let _ = std::fs::remove_file(path);
}

/// A file that changes into something that cannot be loaded is
/// reported once, not every poll. The text stays, the poll keeps trying,
/// and a readable version reloads as usual. `:w` still refuses to write
/// over the file; `:w!` does.
#[test]
fn an_unloadable_file_is_reported_once() {
    let backdate = |path: &std::path::Path, secs: u64| {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - Duration::from_secs(secs)).unwrap();
    };
    let (mut app, path) = app_on_disk("unloadable", "mine\n");
    std::fs::write(&path, [0xff, 0xfe, b'x', b'\n']).unwrap();
    backdate(&path, 5);

    assert!(app.check_disk(), "the first poll reports it");
    assert!(flash_text(&app).contains("reload"), "got: {}", flash_text(&app));
    assert_eq!(text(&app), "mine\n", "the text is untouched");
    assert!(!app.buffer.conflict, "a clean buffer is not a conflict");

    app.flash = None;
    assert!(!app.check_disk(), "the next poll stays quiet");
    assert!(app.flash.is_none());
    assert!(app.buffer.save(SavePolicy::default(), false).is_err(), ":w refuses");
    assert_eq!(std::fs::read(&path).unwrap(), [0xff, 0xfe, b'x', b'\n']);

    // Broken again, a different version: announced again, once.
    std::fs::write(&path, [0xff, b'y', b'\n']).unwrap();
    backdate(&path, 4);
    assert!(app.check_disk(), "a new broken version is news");
    app.flash = None;
    assert!(!app.check_disk());

    // Readable again: reloaded.
    write_settled(&path, "theirs\n");
    backdate(&path, 3);
    assert!(app.check_disk());
    assert_eq!(text(&app), "theirs\n");
    assert!(app.buffer.reload_failed.is_none());
    let _ = std::fs::remove_file(path);
}

/// The reload is ONE undo step, which is what makes an automatic reload
/// reversible rather than merely fast. Undoing it also makes the buffer
/// modified again — you are once more holding what the file does not.
#[test]
fn an_automatic_reload_is_one_undo_step() {
    let (mut app, path) = app_on_disk("undo", "one\ntwo\n");
    write_settled(&path, "wholly different\n");

    assert!(app.check_disk());
    assert_eq!(text(&app), "wholly different\n");

    feed(&mut app, "u");
    assert_eq!(text(&app), "one\ntwo\n", "one u, not one per line");
    assert!(app.buffer.modified, "holding text the file does not have");

    app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    app.sync_after_input();
    assert_eq!(text(&app), "wholly different\n", "and redo takes it back");
    let _ = std::fs::remove_file(path);
}

/// A reload that finds the same text with new line endings adopts them,
/// so the next save keeps what the file now uses.
#[test]
fn a_reload_adopts_a_changed_line_ending() {
    let (mut app, path) = app_on_disk("crlf", "one\ntwo\n");
    write_settled(&path, "one\r\ntwo\r\n");
    app.check_disk();
    assert_eq!(text(&app), "one\ntwo\n");
    assert_eq!(app.buffer.line_ending, crate::text::buffer::LineEnding::Crlf);
    let _ = std::fs::remove_file(path);
}

/// Case 2's precondition: both changed, so NOTHING is touched. This is the
/// assertion the whole feature rests on — an editor may never silently
/// choose between two versions of your work.
#[test]
fn a_modified_buffer_is_never_reloaded_only_flagged() {
    let (mut app, path) = app_on_disk("conflict", "shared\n");
    feed(&mut app, "A mine");
    esc(&mut app);
    let before = text(&app);
    write_settled(&path, "theirs\n");

    assert!(app.check_disk(), "the divergence was noticed");
    assert_eq!(text(&app), before, "the buffer is untouched");
    assert!(app.buffer.modified, "and still unsaved");
    assert!(app.buffer.conflict, "flagged for the reader to resolve");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theirs\n",
        "and the file is untouched too"
    );
    let msg = flash_text(&app);
    assert!(msg.contains("changed on disk"), "got: {msg}");

    // Announced ONCE: a second check finds nothing new to say, or the
    // status line would be buried every second.
    app.flash = None;
    assert!(!app.check_disk(), "the conflict is not re-announced");
    assert_eq!(flash_text(&app), "");
    let _ = std::fs::remove_file(path);
}

/// `:w!` is how a reader says "keep mine", so it settles the conflict.
#[test]
fn writing_over_the_change_clears_the_conflict() {
    let (mut app, path) = app_on_disk("resolve", "shared\n");
    feed(&mut app, "A mine");
    esc(&mut app);
    write_settled(&path, "theirs\n");
    assert!(app.check_disk());
    assert!(app.buffer.conflict);

    app.write(None, true); // :w!
    assert!(!app.buffer.conflict, "the write settled it");
    assert!(!app.buffer.modified);
    assert!(std::fs::read_to_string(&path).unwrap().contains("mine"));
    let _ = std::fs::remove_file(path);
}

/// A `touch` moves the mtime without changing a byte. Absorbing it in
/// silence is the point: replacing text with itself would throw the cursor
/// away and flash about nothing.
#[test]
fn an_identical_rewrite_is_absorbed_silently() {
    let (mut app, path) = app_on_disk("touch", "same\n");
    app.buffer.cursor = Cursor::new(0, 3);
    write_settled(&path, "same\n");

    assert!(!app.check_disk(), "nothing to redraw");
    assert_eq!(flash_text(&app), "", "and nothing to say");
    assert_eq!(app.buffer.cursor.col, 3, "the cursor stayed put");
    // The mtime was taken, so it is not re-noticed every second.
    assert!(
        !crate::fs::save::changed_externally(&path, app.buffer.disk_mtime),
        "the new mtime was absorbed"
    );
    let _ = std::fs::remove_file(path);
}

/// A DELETED file is not a change to take. Reloading emptiness over a
/// document because something removed the file is the worst available
/// behaviour; the buffer keeps its text and `:w` recreates the file.
#[test]
fn a_deleted_file_leaves_the_buffer_alone() {
    let (mut app, path) = app_on_disk("gone", "still here\n");
    std::fs::remove_file(&path).unwrap();

    assert!(!app.check_disk());
    assert_eq!(text(&app), "still here\n");
    assert!(!app.buffer.conflict);

    // And it can be written back out.
    app.write(None, false);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "still here\n");
    let _ = std::fs::remove_file(path);
}

/// A file still being written is left for the next poll. Without this, a
/// truncated read that happens to be valid UTF-8 loads as real content.
#[test]
fn a_file_written_this_instant_waits_for_the_next_poll() {
    let (mut app, path) = app_on_disk("settle", "mine\n");
    // No backdating: mtime is now, so the writer may still be going.
    std::fs::write(&path, "half-writ").unwrap();

    assert!(!app.check_disk(), "not read while it is still warm");
    assert_eq!(text(&app), "mine\n", "buffer untouched");

    // Once it has settled, the same check takes it.
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_modified(std::time::SystemTime::now() - Duration::from_secs(5)).unwrap();
    drop(f);
    assert!(app.check_disk());
    assert_eq!(text(&app), "half-writ");
    let _ = std::fs::remove_file(path);
}

/// Off means off — the `:w` guard is still there to catch it at write time.
#[test]
fn autoreload_off_does_nothing_at_all() {
    let (mut app, path) = app_on_disk("off", "mine\n");
    app.set_option("autoreload off");
    assert!(!app.config.editor.autoreload);
    write_settled(&path, "theirs\n");

    assert!(!app.check_disk());
    assert_eq!(text(&app), "mine\n");
    assert!(!app.buffer.conflict);

    app.set_option("autoreload");
    assert!(app.config.editor.autoreload, "bare :set toggles it back on");
    assert!(app.check_disk(), "and now it takes the change");
    assert_eq!(text(&app), "theirs\n");
    let _ = std::fs::remove_file(path);
}

/// Every open document is checked, not just the visible one — and each is
/// judged on its OWN dirtiness.
#[test]
fn each_document_is_judged_on_its_own() {
    let dir = two_files();
    let mut app = app_with("scratch\n");
    app.open_file(dir.join("one.md"));
    app.open_file(dir.join("two.md"));
    // Dirty the second one only.
    feed(&mut app, "A mine");
    esc(&mut app);

    write_settled(&dir.join("one.md"), "one changed\n");
    write_settled(&dir.join("two.md"), "two changed\n");

    assert!(app.check_disk());
    let by_name = |app: &App, name: &str| -> String {
        app.docs
            .iter()
            .find(|d| d.buffer.display_name() == name)
            .map(|d| d.buffer.rope.to_string())
            .unwrap()
    };
    assert_eq!(by_name(&app, "one.md"), "one changed\n", "clean one reloaded");
    assert!(
        by_name(&app, "two.md").contains("mine"),
        "the dirty one kept its unsaved text"
    );
    assert!(app.buffer.conflict, "and is flagged");
    std::fs::remove_dir_all(&dir).ok();
}

/// `:revert` needs the `!` on a modified buffer, exactly as `:q` does —
/// it throws unsaved work away, and that has to be deliberate.
#[test]
fn revert_takes_the_file_but_only_with_a_bang() {
    let (mut app, path) = app_on_disk("revert", "mine\n");
    feed(&mut app, "A edited");
    esc(&mut app);
    write_settled(&path, "theirs\n");

    app.revert(false);
    assert!(text(&app).contains("edited"), "a plain :revert refuses");
    assert!(flash_text(&app).contains(":revert!"), "and names the way past");

    app.revert(true);
    assert_eq!(text(&app), "theirs\n", ":revert! takes the file");
    assert!(!app.buffer.modified);
    assert!(!app.buffer.conflict);
    assert!(flash_text(&app).contains("reverted"), "got: {}", flash_text(&app));
    let _ = std::fs::remove_file(path);
}

/// A conflicted buffer is skipped by autosave. The guard would refuse it
/// every interval, and the status marker already carries the message.
#[test]
fn autosave_leaves_a_conflicted_buffer_to_the_reader() {
    let (mut app, path) = app_on_disk("both", "shared\n");
    feed(&mut app, "A mine");
    esc(&mut app);
    write_settled(&path, "theirs\n");
    assert!(app.check_disk());
    assert!(app.buffer.conflict);

    app.flash = None;
    autosave_due(&mut app);
    assert!(!app.autosave_tick(), "no repeated complaint on a timer");
    assert_eq!(flash_text(&app), "");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theirs\n",
        "and certainly not written over"
    );
    let _ = std::fs::remove_file(path);
}

// ------------------------------------------------------------- autosave

/// `:w <other>` on a buffer with its own file writes a copy and
/// keeps editing the original; the next `:w` goes to the original.
#[test]
fn w_to_another_name_writes_a_copy() {
    let mut app = app_with("one\n");
    let orig = saved_file(&mut app, "copy-orig");
    let copy = orig.with_extension("copy.md");
    app.run_command(&format!("w {}", copy.display()));
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "one\n");
    assert_eq!(app.buffer.path.as_deref(), Some(orig.as_path()), "still the original");
    assert!(flash_text(&app).contains("still editing"), "got: {}", flash_text(&app));

    feed(&mut app, "ccTWO");
    esc(&mut app);
    assert!(app.buffer.modified, "a copy is not a save of this file");
    app.run_command("w");
    assert_eq!(std::fs::read_to_string(&orig).unwrap(), "TWO\n", "the edit went home");
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "one\n", "the copy kept its text");

    // An existing file is refused without `!`.
    app.run_command(&format!("w {}", copy.display()));
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "one\n");
    app.run_command(&format!("w! {}", copy.display()));
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "TWO\n");

    // Its own name, however spelled, is an ordinary save.
    feed(&mut app, "ccthree");
    esc(&mut app);
    app.run_command(&format!("w {}", orig.display()));
    assert!(!app.buffer.modified);
    let _ = std::fs::remove_file(orig);
    let _ = std::fs::remove_file(copy);
}

/// `:saveas` switches the buffer to the new file and restyles it by
/// the new extension; an unnamed buffer's `:w <name>` does the same.
#[test]
fn saveas_switches_the_buffer_and_its_syntax() {
    let mut app = app_with("text\n");
    let orig = saved_file(&mut app, "saveas-orig");
    let txt = orig.with_extension("txt");
    assert_eq!(app.buffer.syntax, crate::text::buffer::Syntax::Markdown);
    app.run_command(&format!("saveas {}", txt.display()));
    assert_eq!(app.buffer.path.as_deref(), Some(txt.as_path()));
    assert_eq!(app.buffer.syntax, crate::text::buffer::Syntax::PlainProse);
    assert!(!app.buffer.modified);

    let mut app = app_with("text\n");
    let named = txt.with_extension("other.md");
    app.run_command(&format!("w {}", named.display()));
    assert_eq!(app.buffer.path.as_deref(), Some(named.as_path()), "unnamed adopts it");
    assert_eq!(app.buffer.syntax, crate::text::buffer::Syntax::Markdown);
    for p in [orig, txt, named] {
        let _ = std::fs::remove_file(p);
    }
}

/// `:saveas` will not take over a file another buffer has open, and
/// `:wq <path>` saves the buffer as that file (not a copy) before quitting.
#[test]
fn saveas_refuses_an_open_file_and_wq_path_saves_as() {
    let mut app = app_with("a\n");
    let a = saved_file(&mut app, "saveas-a");
    let b = a.with_extension("b.md");
    std::fs::write(&b, "b\n").unwrap();
    assert!(app.open_file(b.clone()));
    app.run_command(&format!("b {}", a.file_name().unwrap().to_string_lossy()));
    assert_eq!(app.buffer.path.as_deref(), Some(a.as_path()));
    app.run_command(&format!("saveas! {}", b.display()));
    assert_eq!(app.buffer.path.as_deref(), Some(a.as_path()), "refused");
    assert!(flash_text(&app).contains("another buffer"), "got: {}", flash_text(&app));
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "b\n");

    let mut app = app_with("x\n");
    let orig = saved_file(&mut app, "wq-orig");
    let out = orig.with_extension("out.md");
    feed(&mut app, "ccedited");
    esc(&mut app);
    app.run_command(&format!("wq {}", out.display()));
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "edited\n");
    assert!(!app.buffer.modified, "saved, so the quit is not refused");
    for p in [a, b, orig, out] {
        let _ = std::fs::remove_file(p);
    }
}

/// `~` and `~/…` at the start of a typed path are `$HOME`.
#[test]
fn a_typed_tilde_is_home() {
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else { return };
    let home = PathBuf::from(home);
    assert_eq!(expand_home("~"), home);
    assert_eq!(expand_home("~/notes/x.md"), home.join("notes/x.md"));
    assert_eq!(expand_home("~user/x.md"), PathBuf::from("~user/x.md"));
    assert_eq!(expand_home("a/~/x.md"), PathBuf::from("a/~/x.md"));

    // `:e` on a file that is not there opens an empty buffer for it and
    // writes nothing, so this touches no real file.
    let name = "shoin-tilde-test-not-a-real-file-7f3a.md";
    let mut app = app_with("");
    app.run_command(&format!("e ~/{name}"));
    assert_eq!(app.buffer.path.as_deref(), Some(home.join(name).as_path()));
    assert!(!home.join(name).exists());
}

/// A named scratch file with `text` in it, already saved so the buffer is
/// clean and `disk_mtime` is recorded — the state autosave has to start in.
fn saved_file(app: &mut App, tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = N.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("shoin-autosave-{tag}-{t}-{n}.md"));
    app.buffer
        .save_as(path.clone(), SavePolicy::default(), false)
        .unwrap();
    path
}

/// Turn autosave on and put the deadline in the PAST, so one `autosave_tick`
/// stands in for however many minutes. Nothing in the feature reads a clock
/// except `autosave_tick` itself, which is what makes that legitimate.
fn autosave_due(app: &mut App) {
    app.config.editor.autosave = true;
    app.autosave_at = Some(Instant::now() - Duration::from_secs(1));
}

/// Off is off: a dirty named buffer and a fired clock still write nothing,
/// because there is no deadline to fire. This is the default, and the one
/// assertion that has to hold whatever else changes.
#[test]
fn autosave_does_nothing_until_it_is_turned_on() {
    let mut app = app_with("draft\n");
    let path = saved_file(&mut app, "off");
    feed(&mut app, "x");
    assert!(app.buffer.modified);

    app.arm_autosave();
    assert!(app.autosave_at.is_none(), "no deadline while autosave is off");
    assert!(!app.autosave_tick(), "and so nothing to fire");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "draft\n",
        "the file on disk is untouched"
    );
    assert!(app.buffer.modified, "and the buffer is still dirty");
    let _ = std::fs::remove_file(path);
}

/// On, and due: the edit reaches disk, the buffer goes clean, and the
/// deadline moves into the future by the configured interval.
#[test]
fn autosave_writes_a_modified_buffer_and_rearms() {
    let mut app = app_with("draft\n");
    let path = saved_file(&mut app, "write");
    feed(&mut app, "A more");
    esc(&mut app);

    autosave_due(&mut app);
    assert!(app.autosave_tick(), "the timer fired and wrote");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "draft more\n");
    assert!(!app.buffer.modified, "a save is a save — the buffer is clean");
    assert!(flash_text(&app).contains("autosaved"), "got: {}", flash_text(&app));

    // Re-armed for the next interval, not left in the past.
    let due = app.autosave_at.expect("still armed");
    assert!(due > Instant::now(), "the deadline moved forward");
    assert!(
        due <= Instant::now() + Duration::from_secs(180),
        "and by no more than the three-minute default"
    );

    // Nothing modified now, so the next firing is silent rather than a
    // pointless write of the same bytes.
    app.autosave_at = Some(Instant::now() - Duration::from_secs(1));
    assert!(!app.autosave_tick(), "nothing dirty, nothing to say");
    let _ = std::fs::remove_file(path);
}

/// THE one that matters: autosave never forces. A file something else has
/// written since we read it is left exactly as that writer left it, and the
/// refusal is reported instead of being swallowed by a background timer.
#[test]
fn autosave_never_clobbers_a_file_someone_else_wrote() {
    let mut app = app_with("mine\n");
    let path = saved_file(&mut app, "guard");

    // Someone else writes it. `set_modified` rather than a sleep, so the
    // mtime differs by construction and the test cannot flake.
    std::fs::write(&path, "theirs\n").unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    let later = std::time::SystemTime::now() + Duration::from_secs(10);
    f.set_modified(later).unwrap();
    drop(f);

    feed(&mut app, "x");
    autosave_due(&mut app);
    assert!(app.autosave_tick(), "it reported, which needs a redraw");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "theirs\n",
        "an unattended write must never take the file"
    );
    assert!(app.buffer.modified, "and the text is still unsaved");
    let msg = flash_text(&app);
    assert!(msg.contains("autosave"), "got: {msg}");
    assert!(msg.contains("changed on disk"), "got: {msg}");
    let _ = std::fs::remove_file(path);
}

/// A buffer with no filename is skipped in silence. `save` would fail with
/// "no filename", and a timer that flashes that every few minutes at a
/// scratch buffer is worse than one that says nothing.
#[test]
fn autosave_skips_a_buffer_with_no_filename() {
    let mut app = app_with("scratch\n");
    assert!(app.buffer.path.is_none());
    assert!(app.buffer.modified);

    autosave_due(&mut app);
    assert!(!app.autosave_tick(), "nothing written and nothing said");
    assert_eq!(flash_text(&app), "", "no error at a buffer that has no file");
}

/// Every modified document, not just the visible one — a dirty buffer
/// behind a split is exactly what an autosave is for, and `:w` only ever
/// means the one in front of you.
#[test]
fn autosave_writes_every_modified_document() {
    let dir = two_files();
    let mut app = app_with("scratch\n");
    app.open_file(dir.join("one.md"));
    feed(&mut app, "A 1");
    esc(&mut app);
    app.open_file(dir.join("two.md"));
    feed(&mut app, "A 2");
    esc(&mut app);

    // The scratch buffer stays dirty and unnamed: it is skipped, and its
    // presence must not stop the two named ones from being written.
    assert_eq!(
        app.docs.iter().filter(|d| d.buffer.modified).count(),
        3,
        "two files plus the scratch buffer"
    );

    autosave_due(&mut app);
    assert!(app.autosave_tick());
    assert_eq!(std::fs::read_to_string(dir.join("one.md")).unwrap(), "file one 1\n");
    assert_eq!(std::fs::read_to_string(dir.join("two.md")).unwrap(), "file two 2\n");
    assert_eq!(
        app.docs.iter().filter(|d| d.buffer.modified).count(),
        1,
        "only the unnamed scratch buffer is left dirty"
    );
    assert_eq!(flash_text(&app), "autosaved 2 files");
    std::fs::remove_dir_all(&dir).ok();
}

/// A manual `:w` restarts the clock. Otherwise a timer armed at startup
/// fires seconds after a save and writes bytes nobody changed.
#[test]
fn a_manual_write_restarts_the_clock() {
    let mut app = app_with("draft\n");
    let path = saved_file(&mut app, "rearm");
    app.config.editor.autosave = true;
    app.autosave_at = Some(Instant::now() + Duration::from_secs(1));

    feed(&mut app, "x");
    app.write(None, false);
    let due = app.autosave_at.expect("armed");
    assert!(
        due > Instant::now() + Duration::from_secs(120),
        "the deadline restarted from the write, not from startup"
    );
    let _ = std::fs::remove_file(path);
}

/// `:set autosave` arms and disarms it live, and `:set autosave_interval`
/// refuses a number outside 1–5 rather than rounding it into range.
#[test]
fn set_autosave_arms_it_and_bounds_the_interval() {
    let mut app = app_with("x\n");

    app.set_option("autosave");
    assert!(app.config.editor.autosave, "bare :set toggles it on");
    assert!(app.autosave_at.is_some(), "and arms the timer");
    assert!(flash_text(&app).contains("every 3 min"), "got: {}", flash_text(&app));

    app.set_option("autosave_interval=5");
    assert_eq!(app.config.editor.autosave_interval, 5);

    app.set_option("autosave_interval=9");
    assert_eq!(app.config.editor.autosave_interval, 5, "9 is refused, not clamped");
    assert!(flash_text(&app).contains("1 to 5"), "got: {}", flash_text(&app));

    app.set_option("autosave off");
    assert!(!app.config.editor.autosave);
    assert!(app.autosave_at.is_none(), "off disarms the timer");
    assert_eq!(
        app.config.editor.autosave_interval, 5,
        "the interval survives being switched off"
    );
}

/// `:w <other-path>` must not be blocked by the mtime of the file we READ —
/// that mtime describes a different file entirely.
#[test]
fn writing_to_a_different_path_is_not_guarded() {
    let mut app = app_with("body\n");
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let first = std::env::temp_dir().join(format!("shoin-sa-{tag}-a.md"));
    let second = std::env::temp_dir().join(format!("shoin-sa-{tag}-b.md"));
    app.buffer.save_as(first.clone(), SavePolicy::default(), false).unwrap();
    // Age the recorded mtime, so a guard that wrongly consulted it for
    // another path would fire.
    app.buffer.disk_mtime = Some(std::time::UNIX_EPOCH);

    app.buffer
        .save_as(second.clone(), SavePolicy::default(), false)
        .expect("a different target is not what disk_mtime describes");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "body\n");
    let _ = std::fs::remove_file(first);
    let _ = std::fs::remove_file(second);
}

/// `:w other.md` over a file that is not this buffer's refuses, as vim's
/// E13 does; `:w!` writes it.
#[test]
fn writing_over_another_existing_file_needs_a_bang() {
    let mut app = app_with("body\n");
    let tag = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let other = std::env::temp_dir().join(format!("shoin-e13-{tag}.md"));
    std::fs::write(&other, "theirs\n").unwrap();
    let refused = app.buffer.save_as(other.clone(), SavePolicy::default(), false);
    assert!(refused.is_err());
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "theirs\n");
    assert_eq!(app.buffer.path, None, "a refused :w names nothing");
    app.buffer
        .save_as(other.clone(), SavePolicy::default(), true)
        .expect(":w! overwrites");
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "body\n");
    // Now it IS this buffer's file: a plain :w to it needs no bang.
    app.buffer.insert_str(Cursor::new(0, 0), "x");
    app.buffer.save(SavePolicy::default(), false).expect("our own file");
    let _ = std::fs::remove_file(other);
}

/// `:wq!` whose write fails keeps the editor open: quitting would throw
/// the unsaved text away.
#[test]
fn wq_bang_with_a_failed_write_does_not_quit() {
    let mut app = app_with("body\n");
    app.buffer.insert_str(Cursor::new(0, 0), "x");
    app.buffer.modified = true;
    app.run_command("wq!");
    assert!(!app.quit, "no file name: the write failed");
}

/// `jk` in Insert removes both characters and leaves the mode.
/// The `j` must appear first — the editor may not stall a keystroke waiting
/// to see what follows.
#[test]
fn the_escape_alias_leaves_insert_mode() {
    let mut app = app_with("");
    feed(&mut app, "i");
    feed(&mut app, "j");
    assert_eq!(text(&app), "j", "the first character lands immediately");
    assert!(matches!(app.mode, Mode::Insert));

    feed(&mut app, "k");
    assert_eq!(text(&app), "", "completing the alias takes both back");
    assert!(matches!(app.mode, Mode::Normal));
}

/// The alias must not eat ordinary prose. `jk` only fires as a run; a `j`
/// that is part of a word, or one an edit has separated from its `k`,
/// stays a letter.
#[test]
fn the_escape_alias_leaves_ordinary_typing_alone() {
    let mut app = app_with("");
    feed(&mut app, "ijazz");
    assert_eq!(text(&app), "jazz");
    assert!(matches!(app.mode, Mode::Insert), "no k followed the j");

    // A word ending in `j` then a Backspace then `k` is not the sequence.
    let mut app = app_with("");
    feed(&mut app, "ij");
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    app.sync_after_input();
    feed(&mut app, "k");
    assert_eq!(text(&app), "k");
    assert!(matches!(app.mode, Mode::Insert), "backspace broke the run");
}

/// A pause longer than `sequence_timeout_ms` means two letters, not an
/// escape — this is what the setting is for.
#[test]
fn the_escape_alias_times_out_between_characters() {
    let mut app = app_with("");
    app.config.input.sequence_timeout_ms = 0;
    feed(&mut app, "ijk");
    assert_eq!(text(&app), "jk", "a lapsed run is just typing");
    assert!(matches!(app.mode, Mode::Insert));
}

/// `escape_alias = ""` disables it, as the config comment promises.
#[test]
fn an_empty_escape_alias_is_off() {
    let mut app = app_with("");
    app.config.input.escape_alias = String::new();
    feed(&mut app, "ijk");
    assert_eq!(text(&app), "jk");
    assert!(matches!(app.mode, Mode::Insert));
}

/// `[keys.insert]` was built from config and never consulted — every
/// binding in it silently did nothing. It is the same override layer the
/// other modes have.
#[test]
fn the_insert_keymap_is_consulted() {
    let mut cfg = Config::default();
    cfg.keys
        .insert
        .insert("<C-g>".to_string(), "normal_mode".to_string());
    let mut app = App::new(cfg, None, None).unwrap();
    feed(&mut app, "iab");
    assert!(matches!(app.mode, Mode::Insert));
    app.on_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
    app.sync_after_input();
    assert!(matches!(app.mode, Mode::Normal), "the bound chord ran");
    assert_eq!(text(&app), "ab", "and typed nothing");
}

/// An UNBOUND control chord types nothing. Crossterm delivers `<C-a>` as
/// `Char('a')` with CONTROL, so the plain-character arm used to insert an
/// `a` for it.
#[test]
fn an_unbound_control_chord_types_nothing() {
    let mut app = app_with("");
    feed(&mut app, "i");
    app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    app.sync_after_input();
    assert_eq!(text(&app), "");
}

/// `editor.auto_pair` closes brackets, steps over a closer already there,
/// and takes both halves back on Backspace.
#[test]
fn auto_pair_closes_and_steps_over() {
    let mut app = app_with("");
    app.config.editor.auto_pair = true;
    feed(&mut app, "i(");
    assert_eq!(text(&app), "()");
    assert_eq!(app.buffer.cursor.col, 1, "cursor sits between them");

    feed(&mut app, "x");
    assert_eq!(text(&app), "(x)");
    feed(&mut app, ")");
    assert_eq!(text(&app), "(x)", "typing the closer steps over it");
    assert_eq!(app.buffer.cursor.col, 3);

    // Backspace between an empty pair takes both.
    let mut app = app_with("");
    app.config.editor.auto_pair = true;
    feed(&mut app, "i[");
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    app.sync_after_input();
    assert_eq!(text(&app), "");
}

/// The reason it is off by default: prose is full of unmatched quotes. An
/// apostrophe inside a word must stay one character.
#[test]
fn auto_pair_leaves_prose_punctuation_alone() {
    let mut app = app_with("");
    app.config.editor.auto_pair = true;
    feed(&mut app, "idon't");
    assert_eq!(text(&app), "don't", "an apostrophe after a word is punctuation");

    let mut app = app_with("");
    app.config.editor.auto_pair = true;
    feed(&mut app, "i\"hi");
    assert_eq!(text(&app), "\"hi\"", "…but an opening quote still pairs");
}

/// Nothing pairs directly in front of a word — that would split it.
#[test]
fn auto_pair_does_not_split_a_word() {
    let mut app = app_with("word\n");
    app.config.editor.auto_pair = true;
    feed(&mut app, "i(");
    assert_eq!(text(&app), "(word\n");
}

/// Off by default, so the setting is real in both directions.
#[test]
fn auto_pair_is_off_unless_asked_for() {
    let mut app = app_with("");
    assert!(!app.config.editor.auto_pair);
    feed(&mut app, "i(");
    assert_eq!(text(&app), "(");
}

/// `editor.auto_indent` is described as continuing list markers and quote
/// prefixes, not just whitespace — it used to do only the whitespace half.
#[test]
fn enter_continues_a_list_marker() {
    let mut app = app_with("- one\n");
    feed(&mut app, "A");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    feed(&mut app, "two");
    assert_eq!(text(&app), "- one\n- two\n");

    // An ordinal advances; repeating `1.` would renumber on sight.
    let mut app = app_with("3. three\n");
    feed(&mut app, "A");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    feed(&mut app, "four");
    assert_eq!(text(&app), "3. three\n4. four\n");

    // A task continues EMPTY — carrying the tick would tick off work
    // nobody did.
    let mut app = app_with("- [x] done\n");
    feed(&mut app, "A");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    feed(&mut app, "next");
    assert_eq!(text(&app), "- [x] done\n- [ ] next\n");

    // And a quote keeps its bar.
    let mut app = app_with("> quoted\n");
    feed(&mut app, "A");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    feed(&mut app, "more");
    assert_eq!(text(&app), "> quoted\n> more\n");
}

/// Enter on an empty item ENDS the list — otherwise there is no way out of
/// one but Esc.
#[test]
fn enter_on_an_empty_item_ends_the_list() {
    let mut app = app_with("- one\n");
    feed(&mut app, "A");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    feed(&mut app, "prose");
    assert_eq!(text(&app), "- one\n\nprose\n");
}

/// With the setting off, Enter is a plain newline again.
#[test]
fn auto_indent_off_gives_a_bare_newline() {
    let mut app = app_with("  - one\n");
    app.config.editor.auto_indent = false;
    feed(&mut app, "A");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    feed(&mut app, "x");
    assert_eq!(text(&app), "  - one\nx\n");
}

/// `r{c}` is in the help and was reaching no handler at all — the pending
/// machine emitted the action and `apply_action`'s catch-all swallowed it.
#[test]
fn replace_char_replaces_under_the_cursor() {
    let mut app = app_with("abcd\n");
    feed(&mut app, "rZ");
    assert_eq!(text(&app), "Zbcd\n");
    assert_eq!(app.buffer.cursor.col, 0);

    // A count replaces that many, and stops at the end of the line.
    feed(&mut app, "3rx");
    assert_eq!(text(&app), "xxxd\n");
    assert_eq!(app.buffer.cursor.col, 2, "cursor lands on the last one");

    let mut app = app_with("ab\n");
    feed(&mut app, "9rz");
    assert_eq!(text(&app), "zz\n", "never runs past the line");
}

/// `<C-w>` + an ARROW moves focus like `<C-w>hjkl`. It used to do nothing:
/// `after_prefix` took a `char`, and an arrow is not one.
#[test]
fn window_arrows_move_focus_like_the_letters() {
    let ctrl_w = |app: &mut App| {
        app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        app.sync_after_input();
    };
    let arrow = |app: &mut App, code: KeyCode| {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        app.sync_after_input();
    };

    let mut app = app_with("hi\n");
    app.term_size = (80, 24);
    app.split_pane(true);
    let right = app.focus_pane;

    ctrl_w(&mut app);
    arrow(&mut app, KeyCode::Left);
    let left = app.focus_pane;
    assert_ne!(left, right, "<C-w><Left> moved to the other pane");

    ctrl_w(&mut app);
    arrow(&mut app, KeyCode::Right);
    assert_eq!(app.focus_pane, right, "<C-w><Right> came back");
}

/// …and from inside the file tree, which runs its own `<C-w>` prefix.
#[test]
fn window_arrows_work_from_the_tree() {
    let mut app = app_with("hi\n");
    app.term_size = (80, 24);
    app.tree = Some(FileTree::open(std::env::current_dir().unwrap()));
    assert!(app.tree.as_ref().unwrap().focused);

    app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    app.on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    app.sync_after_input();
    assert!(
        !app.tree.as_ref().unwrap().focused,
        "<C-w><Right> handed focus back to the editor"
    );
}

/// The text width is `layout.measure` in the config; `:set measure=N`
/// changes it live, which is what trying one on actually needs.
#[test]
fn set_measure_changes_the_text_width() {
    let mut app = app_with("hi\n");
    assert_eq!(app.config.layout.measure, 72, "the default");

    cmd(&mut app, ":set measure=50");
    assert_eq!(app.config.layout.measure, 50);
    cmd(&mut app, ":set width 88");
    assert_eq!(app.config.layout.measure, 88, "`width` is an alias, space or =");

    // A number is not a toggle: with no value it reports rather than flips.
    cmd(&mut app, ":set measure");
    assert_eq!(app.config.layout.measure, 88);

    cmd(&mut app, ":set measure=nope");
    assert_eq!(app.config.layout.measure, 88, "a bad value changes nothing");
    cmd(&mut app, ":set measure=2");
    assert_eq!(app.config.layout.measure, 88, "…and so does an absurd one");
}

/// `:set tab_width N` changes the width live, and the block kinds and the
/// list commands follow it at once: under `\t- b`, a fence 5 columns in is
/// in `b` at 4 and out of it at 8.
#[test]
fn set_tab_width_reclassifies_tabbed_lists() {
    let mut app = app_with("- a\n\t- b\n     ```\n     x\n     ```\n");
    let bounds = |app: &mut App| {
        app.refresh_blocks();
        Lists::new(&app.buffer, &app.blocks.kinds, app.config.editor.tab_width).item_bounds(1)
    };
    assert_eq!(bounds(&mut app), Some((1, 4)));
    cmd(&mut app, ":set tab_width 8");
    assert_eq!(app.config.editor.tab_width, 8);
    assert_eq!(bounds(&mut app), Some((1, 1)));
    cmd(&mut app, ":set ts=2");
    assert_eq!(app.config.editor.tab_width, 2, "`ts` is an alias, space or =");
    bounds(&mut app);
    assert!(matches!(app.blocks.kinds[1], BlockKind::ListItem { depth: 2, .. }));

    // A number, not a toggle; bad values change nothing.
    cmd(&mut app, ":set tab_width");
    assert_eq!(app.flash.as_ref().and_then(|f| f.text.clone()).as_deref(), Some("tab_width=2"));
    for bad in ["nope", "0", "17", "x"] {
        cmd(&mut app, &format!(":set tab_width={bad}"));
        assert_eq!(app.config.editor.tab_width, 2, "{bad}");
        assert!(matches!(app.flash.as_ref().map(|f| &f.kind), Some(FlashKind::Error)), "{bad}");
    }
    for bad in ["0", "x"] {
        cmd(&mut app, &format!(":set tab_width {bad}"));
        assert_eq!(app.config.editor.tab_width, 2, "{bad}, with a space");
    }
}

/// `:set tab_width N` redraws at once: a tab-indented item moves to its new
/// depth on screen, its bullet with it, and the cache is not left at the old
/// width.
#[test]
fn set_tab_width_redraws_tabbed_lines() {
    use ratatui::{backend::TestBackend, Terminal};
    let mut app = app_with("top\n- a\n\t- b\n\tx\ty\n");
    let draw = |app: &App| {
        let mut t = Terminal::new(TestBackend::new(40, 8)).unwrap();
        t.draw(|f| crate::render::frame::render(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        let at = |sym: &str| {
            (0..8u16)
                .flat_map(|y| (0..40u16).map(move |x| (x, y)))
                .find(|&(x, y)| buf[(x, y)].symbol() == sym)
                .map(|(x, _)| x)
                .unwrap()
        };
        let a = at("\u{2022}"); // `a`'s bullet, at the item's left edge
        (at("b") - a, at("x") - a, at("y") - at("x"))
    };
    assert_eq!(draw(&app), (6, 4, 4), "at 4: `b` 4 + 2 in, `x` a tab in, `y` at the next stop");
    cmd(&mut app, ":set tab_width 2");
    app.refresh_blocks(); // as the event loop does before every frame
    assert_eq!(app.config.editor.tab_width, 2);
    assert!(matches!(app.blocks.kinds[2], BlockKind::ListItem { depth: 2, .. }), "reclassified");
    assert_eq!(draw(&app), (4, 2, 2), "at 2");
    cmd(&mut app, ":set tab_width=8");
    app.refresh_blocks();
    assert_eq!(draw(&app), (10, 8, 8), "at 8");
}

/// `:number [relative|absolute]` — an explicit mode sets it directly, and
/// with no argument it TOGGLES, returning to whichever mode was last
/// chosen rather than defaulting to absolute (mirrors `:embed`).
#[test]
fn number_command_toggles_and_remembers_the_last_mode() {
    let mut app = app_with("hi\n");
    assert_eq!(app.numbers, NumberMode::Off, "the default");

    cmd(&mut app, ":number relative");
    assert_eq!(app.numbers, NumberMode::Relative);

    cmd(&mut app, ":number"); // bare: off
    assert_eq!(app.numbers, NumberMode::Off);
    cmd(&mut app, ":number"); // bare again: back to relative, not absolute
    assert_eq!(app.numbers, NumberMode::Relative);

    cmd(&mut app, ":number absolute");
    assert_eq!(app.numbers, NumberMode::Absolute);
    cmd(&mut app, ":number");
    assert_eq!(app.numbers, NumberMode::Off);
    cmd(&mut app, ":number");
    assert_eq!(app.numbers, NumberMode::Absolute, "now the last mode was absolute");

    // A bad argument changes nothing, same as an unset `:set` value would.
    cmd(&mut app, ":number sideways");
    assert_eq!(app.numbers, NumberMode::Absolute);

    // `:set numbers=...` reaches the same place.
    cmd(&mut app, ":set numbers=relative");
    assert_eq!(app.numbers, NumberMode::Relative);
}

/// `:align [left|center|right|justified]` — the default is `left`, an
/// explicit mode sets it directly, and with no argument it CYCLES through
/// every value (there is no single "off" to toggle back to, unlike
/// `:number`/`:embed`).
#[test]
fn align_command_sets_directly_and_cycles_with_no_argument() {
    let mut app = app_with("hi\n");
    assert_eq!(app.text_align, TextAlign::Left, "the default");

    cmd(&mut app, ":align center");
    assert_eq!(app.text_align, TextAlign::Center);
    cmd(&mut app, ":align right");
    assert_eq!(app.text_align, TextAlign::Right);
    cmd(&mut app, ":align justified");
    assert_eq!(app.text_align, TextAlign::Justified);
    cmd(&mut app, ":align left");
    assert_eq!(app.text_align, TextAlign::Left);

    cmd(&mut app, ":align"); // bare: cycles forward
    assert_eq!(app.text_align, TextAlign::Center);
    cmd(&mut app, ":align");
    assert_eq!(app.text_align, TextAlign::Right);
    cmd(&mut app, ":align");
    assert_eq!(app.text_align, TextAlign::Justified);
    cmd(&mut app, ":align");
    assert_eq!(app.text_align, TextAlign::Left, "wraps back around");

    // A bad argument changes nothing, same as an unset `:set` value would.
    cmd(&mut app, ":align sideways");
    assert_eq!(app.text_align, TextAlign::Left);

    // `:set align=...` reaches the same place.
    cmd(&mut app, ":set align=center");
    assert_eq!(app.text_align, TextAlign::Center);
}

/// `:export [format] [justify]` takes the two tokens in either order, and
/// refuses `justify` on `md`/`pdf` — neither has a visual layout of its
/// own to stretch (raw markup; pandoc's own template, respectively) — by
/// never opening the save dialog at all.
#[test]
fn export_command_takes_format_and_justify_in_either_order() {
    let d = two_files();
    let mut app = app_with("scratch\n");
    app.open_file(d.join("one.md"));

    cmd(&mut app, ":export html justify");
    let Mode::Prompt(p) = app.mode.clone() else { panic!("no prompt") };
    assert_eq!(p.kind, PromptKind::Export { format: Format::Html, justify: true });
    app.mode = Mode::Normal;

    cmd(&mut app, ":export justify txt");
    let Mode::Prompt(p) = app.mode.clone() else { panic!("no prompt") };
    assert_eq!(p.kind, PromptKind::Export { format: Format::Text, justify: true });
    app.mode = Mode::Normal;

    cmd(&mut app, ":export justify");
    assert!(matches!(app.mode, Mode::Normal), "md has nothing to stretch");
    cmd(&mut app, ":export pdf justify");
    assert!(matches!(app.mode, Mode::Normal), "pandoc's template decides, not us");

    std::fs::remove_dir_all(&d).ok();
}

/// `:export`'s refusals name the formats `values` lists.
#[test]
fn export_refusals_name_the_value_lists() {
    let d = two_files();
    let mut app = app_with("scratch\n");
    app.open_file(d.join("one.md"));
    cmd(&mut app, ":export docx");
    let flash = app.flash.as_ref().and_then(|f| f.text.clone()).unwrap_or_default();
    assert_eq!(flash, format!("export: \"docx\" is not {}", crate::values::or_list(crate::values::EXPORT_VALUES)));
    assert_eq!(flash, "export: \"docx\" is not md, txt, html or pdf");
    cmd(&mut app, ":export md justify");
    let flash = app.flash.as_ref().and_then(|f| f.text.clone()).unwrap_or_default();
    assert_eq!(flash, "export: justify only applies to txt or html");
    std::fs::remove_dir_all(&d).ok();
}

/// The bare no-argument toggle is also reachable as a leader binding, the
/// way `toggle_conceal`/`toggle_zen` are.
#[test]
fn toggle_numbers_action_flips_it_on_and_off() {
    let mut app = app_with_keys("hi\n", &[("<leader>n", "toggle_numbers")]);
    assert_eq!(app.numbers, NumberMode::Off);
    feed(&mut app, " n");
    assert_eq!(app.numbers, NumberMode::Absolute, "turns on to the default last mode");
    feed(&mut app, " n");
    assert_eq!(app.numbers, NumberMode::Off);
}

/// `cycle_align` reaches the same bare cycle as `:align` with no argument,
/// from a single key press — there is no "off" to toggle back to, so
/// unlike `toggle_numbers` every press moves forward one step.
#[test]
fn cycle_align_action_steps_through_every_value() {
    let mut app = app_with_keys("hi\n", &[("<leader>a", "cycle_align")]);
    assert_eq!(app.text_align, TextAlign::Left);
    feed(&mut app, " a");
    assert_eq!(app.text_align, TextAlign::Center);
    feed(&mut app, " a");
    assert_eq!(app.text_align, TextAlign::Right);
    feed(&mut app, " a");
    assert_eq!(app.text_align, TextAlign::Justified);
    feed(&mut app, " a");
    assert_eq!(app.text_align, TextAlign::Left);
}

/// Changing the measure re-wraps: the render cache keys its entries on it.
#[test]
fn a_new_measure_rewraps_the_document() {
    let mut app = app_with(&("word ".repeat(40) + "\n"));
    let rows = |app: &App| {
        let mut cache = app.cache.borrow_mut();
        cache.sync(app, app.config.layout.measure);
        cache.total_rows()
    };
    app.refresh_blocks();
    let wide = rows(&app);
    cmd(&mut app, ":set measure=30");
    app.refresh_blocks();
    assert!(rows(&app) > wide, "a narrower measure wraps into more rows");
}

/// The start screen is for a blank start, not for an editor mid-flight: a
/// split means work is under way whatever happens to be in this pane.
#[test]
fn splashing_stops_once_the_window_is_split() {
    use crate::render::splash;
    let mut app = App::new(Config::default(), None, None).unwrap();
    app.term_size = (90, 28);
    assert!(splash::active(&app), "a bare start splashes");

    cmd(&mut app, ":vs");
    assert!(!splash::active(&app), "…a split window does not");
    cmd(&mut app, ":only");
    assert!(splash::active(&app), "…and closing it brings the blank page back");
}

/// Opening a file retires it, even though that file might itself be empty.
#[test]
fn splashing_stops_once_a_file_is_open() {
    let dir = two_files();
    let mut app = App::new(Config::default(), None, None).unwrap();
    assert!(crate::render::splash::active(&app));
    app.open_file(dir.join("one.md"));
    assert!(!crate::render::splash::active(&app), "a document is open now");
    std::fs::remove_dir_all(&dir).ok();
}

/// vim multiplies an operator count by its motion count: `2d3w` is six.
#[test]
fn operator_and_motion_counts_multiply() {
    let mut app = app_with("a b c d e f g h\n");
    feed(&mut app, "2d3w");
    assert_eq!(text(&app), "g h\n");
}

/// `cw` stops at the end of the word, keeping the space after it.
#[test]
fn cw_behaves_like_ce() {
    let mut app = app_with("alpha beta\n");
    feed(&mut app, "cw");
    assert_eq!(text(&app), " beta\n");
    feed(&mut app, "one");
    esc(&mut app);
    assert_eq!(text(&app), "one beta\n");
}

/// On whitespace it stays `dw`-like, deleting up to the next word.
#[test]
fn cw_on_a_blank_still_reaches_the_next_word() {
    let mut app = app_with("alpha  beta\n");
    feed(&mut app, "5lcw");
    assert_eq!(text(&app), "alphabeta\n");
}

#[test]
fn indent_and_outdent_lines() {
    let mut app = app_with("one\ntwo\n\nthree\n");
    feed(&mut app, ">>");
    assert_eq!(text(&app), "    one\ntwo\n\nthree\n");
    feed(&mut app, "<<");
    assert_eq!(text(&app), "one\ntwo\n\nthree\n");
    // `>ip` is linewise over the paragraph and skips the blank line.
    feed(&mut app, ">ip");
    assert_eq!(text(&app), "    one\n    two\n\nthree\n");
}

/// `>al` nests an item under its previous sibling, children and all, and
/// leaves the cursor on the same character.
#[test]
fn indent_a_list_item_with_its_children() {
    let mut app = app_with("- a\n- b\n    - c\n- d\n");
    app.buffer.cursor = Cursor::new(1, 2);
    feed(&mut app, ">al");
    assert_eq!(text(&app), "- a\n    - b\n        - c\n- d\n");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 6));
    feed(&mut app, "<al");
    assert_eq!(text(&app), "- a\n- b\n    - c\n- d\n");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 2));
}

#[test]
fn list_item_indent_is_one_undo_step_and_repeats() {
    let mut app = app_with("- a\n- b\n- c\n");
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, ">al");
    feed(&mut app, "j.");
    assert_eq!(text(&app), "- a\n    - b\n    - c\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), "- a\n    - b\n- c\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), "- a\n- b\n- c\n");
}

/// The shifts that would break a list's structure change nothing.
#[test]
fn list_shifts_that_break_the_structure_are_refused() {
    let t = "- a\n  - b\n  - c\n";
    // A first child has nothing to nest under.
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, ">al");
    assert_eq!(text(&app), t);
    assert_eq!(flash_text(&app), "nothing to indent under");
    // A top-level item has nowhere to outdent to.
    let mut app = app_with(t);
    feed(&mut app, "<al");
    assert_eq!(text(&app), t);
    assert_eq!(flash_text(&app), "already at the top level");
    // A top-level level 4 columns in would read as code.
    let t = "- a\n    - b\n";
    let mut app = app_with(t);
    feed(&mut app, ">aL");
    assert_eq!(text(&app), t);
    assert_eq!(flash_text(&app), "a top-level list 4 columns in reads as code");
}

/// A nested level shifts as one: the first sibling goes with the rest, so
/// nothing is re-parented.
#[test]
fn indent_and_outdent_a_nested_level() {
    let mut app = app_with("- p\n  - a\n    - a1\n  - b\n- q\n");
    app.buffer.cursor = Cursor::new(3, 0);
    feed(&mut app, ">aL");
    assert_eq!(text(&app), "- p\n    - a\n      - a1\n    - b\n- q\n");
    feed(&mut app, "<aL");
    assert_eq!(text(&app), "- p\n  - a\n    - a1\n  - b\n- q\n");
}

const NINE_ITEMS: &str = "1. a\n2. b\n3. c\n4. d\n5. e\n6. f\n7. g\n8. h\n9. i\n";

#[test]
fn fixlist_re_indents_in_one_undo_step() {
    let t = format!("{NINE_ITEMS}10. j\n   - y\n     - z\n");
    let mut app = app_with(&t);
    app.buffer.cursor = Cursor::new(10, 5);
    ex(&mut app, "fixlist");
    assert_eq!(text(&app), format!("{NINE_ITEMS}10. j\n    - y\n      - z\n"));
    assert!(flash_text(&app).contains("re-indented 2 lines"), "{}", flash_text(&app));
    // The cursor stays on the same character.
    assert_eq!(app.buffer.cursor, Cursor::new(10, 6));
    feed(&mut app, "u");
    assert_eq!(text(&app), t);
    ex(&mut app, "redo");
    ex(&mut app, "fixlist");
    assert_eq!(flash_text(&app), "list already lined up");
}

#[test]
fn fixlist_says_what_it_left_and_why() {
    // Moved to the text column, the prose would open a fence.
    let t = "- a\n\n      ```\n      x\n";
    let mut app = app_with(t);
    ex(&mut app, "fixlist");
    assert_eq!(text(&app), t);
    assert!(flash_text(&app).contains("left 1 item as written (line 1)"), "{}", flash_text(&app));
    let mut app = app_with("para\n");
    ex(&mut app, "fixlist");
    assert_eq!(flash_text(&app), "not in a list");
}

#[test]
fn fixlist_in_insert_is_one_step_and_keeps_the_session_grouped() {
    let t = format!("{NINE_ITEMS}10. j\n   - y\n");
    let mut cfg = Config::default();
    cfg.keys.insert.insert("<C-l>".into(), "fix_list".into());
    let mut app = App::new(cfg, None, None).unwrap();
    app.buffer.insert_str(Cursor::new(0, 0), &t);
    app.buffer.cursor = Cursor::new(9, 0);
    app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width);
    feed(&mut app, "Axy");
    app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    app.sync_after_input();
    feed(&mut app, "pqr");
    esc(&mut app);
    let fixed = format!("{NINE_ITEMS}10. jxy\n    - y\n");
    assert_eq!(text(&app), fixed.replace("jxy", "jxypqr"));
    feed(&mut app, "u");
    assert_eq!(text(&app), fixed, "pqr is one step");
    feed(&mut app, "u");
    assert_eq!(text(&app), format!("{NINE_ITEMS}10. jxy\n   - y\n"));
}

#[test]
fn fixlist_is_refused_read_only_and_works_on_a_key() {
    let t = format!("{NINE_ITEMS}10. j\n   - y\n");
    let mut app = app_with(&t);
    app.set_view(true);
    ex(&mut app, "fixlist");
    assert_eq!(text(&app), t);
    assert!(flash_text(&app).contains("read-only"), "{}", flash_text(&app));
    let mut app = app_with_keys(&t, &[("<C-l>", "fix_list")]);
    app.on_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    app.sync_after_input();
    assert_eq!(text(&app), format!("{NINE_ITEMS}10. j\n    - y\n"));
    feed(&mut app, "u");
    assert_eq!(text(&app), t);
}

// ------------------------------------------------- lists inside a blockquote

fn hit(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    app.sync_after_input();
}

#[test]
fn a_list_in_a_quote_indents_and_outdents_behind_its_markers() {
    let t = "> - a\n> - b\n> > - c\n";
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(1, 4);
    // Nothing nests yet, so the step is `tab_width`.
    feed(&mut app, ">al");
    assert_eq!(text(&app), "> - a\n>     - b\n> > - c\n");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 8), "on the same character");
    feed(&mut app, "<al");
    assert_eq!(text(&app), t);
    feed(&mut app, "u");
    assert_eq!(text(&app), "> - a\n>     - b\n> > - c\n", "one undo step each");
    // The first item has nothing to nest under, inside the quote as out.
    let mut app = app_with(t);
    feed(&mut app, ">al");
    assert_eq!(text(&app), t);
    assert_eq!(flash_text(&app), "nothing to indent under");
    // A deeper quote is a list of its own.
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(2, 5);
    feed(&mut app, ">al");
    assert_eq!(flash_text(&app), "nothing to indent under");
    // A quote line in no list is left alone, as a paragraph is.
    let mut app = app_with("> text\n");
    feed(&mut app, ">al");
    assert_eq!(text(&app), "> text\n");
}

#[test]
fn alt_j_in_a_quote_moves_items_and_leaves_the_markers() {
    let t = "> - a\n>   - a1\n>- b\n> - c\n";
    let mut app = app_with(t);
    alt(&mut app, 'j');
    // Each line keeps the markers it had; the text moves.
    assert_eq!(text(&app), "> - b\n> - a\n>   - a1\n> - c\n");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 2));
    alt(&mut app, 'j');
    assert_eq!(text(&app), "> - b\n> - c\n> - a\n>   - a1\n");
    alt(&mut app, 'j');
    assert_eq!(flash_text(&app), "last item", "the quote ends the list");
    feed(&mut app, "u");
    assert_eq!(text(&app), "> - b\n> - a\n>   - a1\n> - c\n");
}

#[test]
fn a_quote_ending_mid_list_ends_the_list() {
    let t = "> - a\n> - b\n- c\n";
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(1, 2);
    alt(&mut app, 'j');
    assert_eq!(flash_text(&app), "last item");
    assert_eq!(text(&app), t);
    feed(&mut app, "dal");
    assert_eq!(text(&app), "> - a\n- c\n");
    // Lines with more markers are another container.
    let mut app = app_with("> - a\n> > - b\n> - c\n");
    alt(&mut app, 'j');
    assert_eq!(flash_text(&app), "last item");
}

#[test]
fn a_visual_move_in_a_quote_must_stay_in_it() {
    let t = "> - a\n> - b\n\n- c\n";
    let mut app = app_with(t);
    feed(&mut app, "Vj");
    alt(&mut app, 'j');
    assert_eq!(flash_text(&app), "last item");
    let mut app = app_with(t);
    feed(&mut app, "Vjjj");
    alt(&mut app, 'j');
    assert_eq!(flash_text(&app), "selection leaves the quote");
    assert_eq!(text(&app), t);
    let mut app = app_with("> - a\n> - b\n> - c\n");
    feed(&mut app, "Vj");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "> - c\n> - a\n> - b\n");
    assert_eq!(app.anchor.map(|a| a.line), Some(1), "the selection follows the items");
}

#[test]
fn a_tab_after_the_marker_is_the_markers_space() {
    // `>` and a tab: one column is the marker's space, the rest indent, so
    // the tabbed line is a sub-item of `a` (tab_width 4: 3 columns in).
    let t = "> - a\n>\t- b\n> - c\n";
    let mut app = app_with(t);
    feed(&mut app, "dal");
    assert_eq!(text(&app), "> - c\n");
    // Moved, the tab stays where the text after it does not change.
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(1, 3);
    feed(&mut app, "<al");
    assert_eq!(text(&app), "> - a\n> - b\n> - c\n");
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(2, 2);
    feed(&mut app, ">al");
    // `c`'s line has a space after its marker, and keeps it.
    assert_eq!(text(&app), "> - a\n>\t- b\n>    - c\n");
}

#[test]
fn an_ordered_list_in_a_quote_is_renumbered() {
    let t = "> 1. a\n> 2. b\n> 3. c\n";
    let mut app = app_with(t);
    alt(&mut app, 'j');
    assert_eq!(text(&app), "> 1. b\n> 2. a\n> 3. c\n");
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(1, 5);
    feed(&mut app, ">al");
    assert_eq!(text(&app), "> 1. a\n>     1. b\n> 2. c\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), t, "renumbering is in the same step");
}

#[test]
fn enter_in_a_quoted_list_continues_both() {
    let mut app = app_with("> - a\n");
    feed(&mut app, "A");
    hit(&mut app, KeyCode::Enter);
    feed(&mut app, "b");
    hit(&mut app, KeyCode::Enter);
    hit(&mut app, KeyCode::Enter);
    feed(&mut app, "c");
    esc(&mut app);
    // An empty item ends the list, not the quote.
    assert_eq!(text(&app), "> - a\n> - b\n>\n> c\n");
    let mut app = app_with("> > 3. a\n");
    feed(&mut app, "A");
    hit(&mut app, KeyCode::Enter);
    esc(&mut app);
    assert_eq!(text(&app), "> > 3. a\n> > 4. \n");
}

#[test]
fn tab_in_insert_indents_a_quoted_item() {
    let mut app = app_with("> - a\n> - b\n");
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, "A");
    hit(&mut app, KeyCode::Tab);
    assert_eq!(text(&app), "> - a\n>     - b\n");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 9));
    app.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    app.sync_after_input();
    assert_eq!(text(&app), "> - a\n> - b\n");
    // A plain quote line types a tab.
    let mut app = app_with("> a\n");
    feed(&mut app, "A");
    hit(&mut app, KeyCode::Tab);
    assert!(text(&app).starts_with("> a\t") || text(&app).starts_with("> a "), "{:?}", text(&app));
}

#[test]
fn the_list_objects_work_in_a_quote() {
    let t = "> - a\n>   - a1\n> - b\n> - c\n\nafter\n";
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(2, 3);
    feed(&mut app, "yal");
    assert_eq!(app.registers.get(&'"').map(|r| r.text.clone()), Some("> - b\n".into()));
    app.buffer.cursor = Cursor::new(0, 3);
    feed(&mut app, "dil");
    assert_eq!(text(&app), ">   - a1\n> - b\n> - c\n\nafter\n");
}

#[test]
fn fixlist_in_a_quote_re_indents_behind_the_markers() {
    let t = "> 10. j\n>    - y\n";
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(1, 5);
    ex(&mut app, "fixlist");
    assert_eq!(text(&app), "> 10. j\n>     - y\n");
    assert!(flash_text(&app).contains("re-indented 1 line"), "{}", flash_text(&app));
    feed(&mut app, "u");
    assert_eq!(text(&app), t);
}

#[test]
fn fixlist_in_visual_re_indents_every_list_touched_in_one_step() {
    let t = "10. j\n   - y\n\npara\n\n> 10. k\n>    - z\n\n11. m\n   - w\n".to_string();
    let mut app = app_with(&t);
    // From `10. j` to the quote: two lists (the second is not touched).
    feed(&mut app, "V5j");
    feed(&mut app, ":fixlist");
    hit(&mut app, KeyCode::Enter);
    assert_eq!(text(&app), t.replace("   - y", "    - y").replace(">    - z", ">     - z"));
    assert!(flash_text(&app).contains("re-indented 2 lines"), "{}", flash_text(&app));
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.buffer.cursor, Cursor::new(0, 0));
    feed(&mut app, "u");
    assert_eq!(text(&app), t, "one undo step");
    // A selection touching no list.
    feed(&mut app, "4GV");
    feed(&mut app, ":fixlist");
    hit(&mut app, KeyCode::Enter);
    assert_eq!(flash_text(&app), "not in a list");
    assert_eq!(text(&app), t);
    // `:` from Normal afterwards is the cursor's list only.
    feed(&mut app, "9G");
    ex(&mut app, "fixlist");
    assert_eq!(text(&app), t.replace("   - w", "    - w"));
    // Twice over lists already lined up.
    feed(&mut app, "ggVG");
    feed(&mut app, ":fixlist");
    hit(&mut app, KeyCode::Enter);
    feed(&mut app, "ggVG");
    feed(&mut app, ":fixlist");
    hit(&mut app, KeyCode::Enter);
    assert_eq!(flash_text(&app), "lists already lined up");
}

#[test]
fn inner_level_takes_the_item_through_its_last_sibling() {
    let t = "- a\n- b\n  - b1\n- c\n  - c1\n\nafter\n";
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(1, 2);
    feed(&mut app, "yiL");
    assert_eq!(app.registers.get(&'"').map(|r| r.text.clone()), Some("- b\n  - b1\n- c\n  - c1\n".into()));
    feed(&mut app, "diL");
    assert_eq!(text(&app), "- a\n\nafter\n");
    feed(&mut app, "u");
    app.buffer.cursor = Cursor::new(1, 2);
    feed(&mut app, "ciLx");
    esc(&mut app);
    assert_eq!(text(&app), "- a\nx\n\nafter\n");
    // On a sub-item, its own level only.
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(2, 4);
    feed(&mut app, "diL");
    assert_eq!(text(&app), "- a\n- b\n- c\n  - c1\n\nafter\n");
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(3, 0);
    feed(&mut app, "viL");
    assert_eq!(app.mode, Mode::VisualLine);
    assert_eq!(app.anchor.map(|a| a.line), Some(3));
    assert_eq!(app.buffer.cursor.line, 4);
    // `>iL` nests the items under the one before them.
    let mut app = app_with("- a\n- b\n- c\n");
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, ">iL");
    assert_eq!(text(&app), "- a\n    - b\n    - c\n");
    feed(&mut app, "<iL");
    assert_eq!(text(&app), "- a\n- b\n- c\n");
    // In a quote too.
    let mut app = app_with("> - a\n> - b\n> - c\n");
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, "diL");
    assert_eq!(text(&app), "> - a\n");
}

/// An item nested into a sub-list of its own starts it at 1, and the list
/// it left closes the gap.
#[test]
fn indenting_an_ordered_item_renumbers_both_lists() {
    let mut app = app_with("1. a\n2. b\n3. c\n");
    app.buffer.cursor = Cursor::new(1, 3);
    feed(&mut app, ">al");
    assert_eq!(text(&app), "1. a\n    1. b\n2. c\n");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 7));
    // Joining a sub-list that is already there puts it at the end.
    let mut app = app_with("1. a\n    1. x\n2. b\n3. c\n");
    app.buffer.cursor = Cursor::new(2, 0);
    feed(&mut app, ">al");
    assert_eq!(text(&app), "1. a\n    1. x\n    2. b\n2. c\n");
    // Renumbering is part of the same undo step.
    feed(&mut app, "u");
    assert_eq!(text(&app), "1. a\n    1. x\n2. b\n3. c\n");
}

/// Every list keeps the number it started at: the one joined, the one
/// left, and the one the moved item's old siblings now form under it.
#[test]
fn renumbered_lists_keep_their_start() {
    let mut app = app_with("5. p\n    3. x\n    4. y\n    5. z\n6. q\n");
    app.buffer.cursor = Cursor::new(2, 0);
    feed(&mut app, "<al");
    assert_eq!(text(&app), "5. p\n    3. x\n6. y\n    3. z\n7. q\n");
}

/// `10.` becoming `1.` moves the text after it, and the cursor with it.
#[test]
fn a_renumbered_ordinal_can_change_width() {
    let t: String = (1..=10).map(|i| format!("{i}. i{i}\n")).collect();
    let mut app = app_with(&t);
    app.buffer.cursor = Cursor::new(9, 4); // on the `i` of `10. i10`
    feed(&mut app, ">al");
    assert_eq!(app.buffer.line_text(9), "    1. i10");
    assert_eq!(app.buffer.cursor, Cursor::new(9, 7));
}

#[test]
fn list_renumber_off_leaves_the_numbers() {
    let mut app = app_with("1. a\n2. b\n3. c\n");
    app.config.editor.list_renumber = false;
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, ">al");
    assert_eq!(text(&app), "1. a\n    2. b\n3. c\n");
}

/// `Alt-{c}`, the way a terminal reports it.
fn alt(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
    app.sync_after_input();
}

#[test]
fn alt_j_and_alt_k_move_an_item_with_its_children() {
    let t = "- a\n  - a1\n- b\n- c\n";
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(0, 2);
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- b\n- a\n  - a1\n- c\n");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 2), "the cursor follows the item");
    alt(&mut app, 'k');
    assert_eq!(text(&app), t);
    assert_eq!(app.buffer.cursor, Cursor::new(0, 2));
}

/// The two items swap; the blank line between them stays put.
#[test]
fn moving_a_loose_item_keeps_the_gap() {
    let mut app = app_with("- a\n\n- b\n  more\n");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- b\n  more\n\n- a\n");
    assert_eq!(app.buffer.cursor.line, 3);
}

#[test]
fn a_count_moves_past_that_many_and_stops_at_the_end() {
    let mut app = app_with("- a\n- b\n- c\n- d\n");
    feed(&mut app, "2");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- b\n- c\n- a\n- d\n");
    feed(&mut app, "9");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- b\n- c\n- d\n- a\n");
}

/// An item never leaves its list by moving: past the first or last
/// sibling is refused, and so is a line outside any list.
#[test]
fn moves_off_the_end_of_a_list_are_refused() {
    let t = "- p\n  - x\n  - y\n- q\n";
    let mut app = app_with(t);
    app.buffer.cursor = Cursor::new(2, 0);
    alt(&mut app, 'j');
    assert_eq!(text(&app), t);
    assert_eq!(flash_text(&app), "last item");
    app.buffer.cursor = Cursor::new(0, 0);
    alt(&mut app, 'k');
    assert_eq!(text(&app), t);
    assert_eq!(flash_text(&app), "first item");
    let mut app = app_with("prose\n");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "prose\n");
    assert_eq!(flash_text(&app), "not in a list");
}

#[test]
fn moving_an_ordered_item_renumbers_its_list() {
    let mut app = app_with("1. a\n2. b\n3. c\n");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "1. b\n2. a\n3. c\n");
    // The list keeps the number it starts at.
    let mut app = app_with("3. a\n4. b\n");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "3. b\n4. a\n");
}

#[test]
fn a_move_is_one_undo_step_and_repeats() {
    let mut app = app_with("- a\n- b\n- c\n");
    alt(&mut app, 'j');
    feed(&mut app, ".");
    assert_eq!(text(&app), "- b\n- c\n- a\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), "- b\n- a\n- c\n");
}

#[test]
fn moving_the_last_item_of_a_file_with_no_final_newline() {
    let mut app = app_with("- a\n- b");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- b\n- a");
    alt(&mut app, 'k');
    assert_eq!(text(&app), "- a\n- b");
}

/// Normal-mode edit keys act on the Visual selection and end Visual,
/// instead of editing at the cursor with the selection still up.
#[test]
fn visual_edit_keys_act_on_the_selection() {
    for (start, keys, want) in [
        ("one two three\n", "yiwwviwp", "one one three\n"),
        ("abc def\n", "vll~", "ABC def\n"),
        ("abc def\n", "vllrx", "xxx def\n"),
        ("a\nb\nc\n", "VjJ", "a b\nc\n"),
        ("abc def\nnext\n", "vlD", "next\n"),
        ("abc def\nnext\n", "vlX", "next\n"),
        ("one\ntwo\nthree\n", "jVp", "one\ntwo\nthree\n"),
        ("ab\ncd\n", "vjl~", "AB\nCD\n"),
        ("ab\ncd\n", "Vjrz", "zz\nzz\n"),
        ("a\nb\n", "vJ", "a b\n"),
    ] {
        let mut app = app_with(start);
        feed(&mut app, keys);
        assert_eq!(text(&app), want, "{keys} on {start:?}");
        assert_eq!(app.mode, Mode::Normal, "{keys} ends Visual");
        assert!(app.anchor.is_none(), "{keys} drops the selection");
    }
}

/// `p` swaps: the replaced text goes to the unnamed register. `P` keeps
/// the register, so one yank replaces several selections. One undo step
/// takes the whole `viwp` back.
#[test]
fn visual_p_replaces_and_undoes_in_one_step() {
    let mut app = app_with("one two three\n");
    feed(&mut app, "yiwwviwp");
    assert_eq!(text(&app), "one one three\n");
    assert_eq!(app.registers.get(&'"').map(|r| r.text.as_str()), Some("two"));
    feed(&mut app, "u");
    assert_eq!(text(&app), "one two three\n");

    let mut app = app_with("aa bb cc\n");
    feed(&mut app, "yiwwviwPwviwP");
    assert_eq!(text(&app), "aa aa aa\n");
    assert_eq!(app.registers.get(&'"').map(|r| r.text.as_str()), Some("aa"));
}

/// A named register works in Visual too, and lines and text mix the way
/// vim mixes them.
#[test]
fn visual_p_takes_a_named_register_and_mixes_lines_and_text() {
    let mut app = app_with("alpha beta\n");
    feed(&mut app, "\"ayiwwviw\"ap");
    assert_eq!(text(&app), "alpha alpha\n");

    // A yanked line in place of a word splits the line around it.
    let mut app = app_with("top\nx mid y\n");
    feed(&mut app, "yyjwviwp");
    assert_eq!(text(&app), "top\nx \ntop\n y\n");

    // A word in place of lines is a line of its own; the last line of the
    // file gets no blank line after it.
    let mut app = app_with("word\nl1\nl2\n");
    feed(&mut app, "yiwjVjp");
    assert_eq!(text(&app), "word\nword\n");
    let mut app = app_with("word\nl1\nl2");
    feed(&mut app, "yiwjVjp");
    assert_eq!(text(&app), "word\nword");
    let mut app = app_with("keep\nl1\nl2");
    feed(&mut app, "yyjVjp");
    assert_eq!(text(&app), "keep\nkeep");

    // `vp` on an empty line fills it; the next line stays its own.
    let mut app = app_with("word\n\nnext\n");
    feed(&mut app, "yiwjvp");
    assert_eq!(text(&app), "word\nword\nnext\n");

    // Text spanning lines leaves the cursor at its start, as Normal `p`.
    let mut app = app_with("ab\ncd\nxy zz\n");
    feed(&mut app, "lvjyjjwviwp");
    assert_eq!(text(&app), "ab\ncd\nxy b\ncd\n");
    assert_eq!(app.buffer.cursor, Cursor::new(2, 3));
}

/// `Y` yanks the selected lines, `C`/`S`/`R` change them, `I`/`A` end
/// Visual and start Insert, and an edit with no Visual meaning (`O`)
/// does nothing.
#[test]
fn visual_line_keys_and_unbound_edits() {
    let mut app = app_with("ab\ncd\nef\n");
    feed(&mut app, "vjY");
    assert_eq!(app.registers.get(&'"').map(|r| (r.text.as_str(), r.linewise)), Some(("ab\ncd\n", true)));
    assert_eq!(app.mode, Mode::Normal);
    for key in ["C", "S", "R"] {
        let mut app = app_with("ab\ncd\nef\n");
        feed(&mut app, &format!("vj{key}"));
        assert_eq!(text(&app), "\nef\n", "{key}");
        assert_eq!(app.mode, Mode::Insert, "{key}");
    }
    // `I` before the selection, `A` after it, whichever end the cursor is on.
    for (keys, want) in [
        ("lvjIX", "aXb\ncd\n"),
        ("jlvkIX", "aXb\ncd\n"),
        ("vjAX", "ab\ncXd\n"),
        ("jvkAX", "ab\ncXd\n"),
        ("VjIX", "Xab\ncd\n"),
        ("VjAX", "ab\ncdX\n"),
    ] {
        let mut app = app_with("ab\ncd\n");
        feed(&mut app, keys);
        assert_eq!(app.mode, Mode::Insert, "{keys}");
        assert!(app.anchor.is_none(), "{keys}");
        assert_eq!(text(&app), want, "{keys}");
    }
    let mut app = app_with("ab\ncd\n");
    feed(&mut app, "vlO");
    assert_eq!(text(&app), "ab\ncd\n");
    assert_eq!(app.mode, Mode::Visual, "O is not an edit here");
}

/// `.` after a Visual edit repeats it on the same span from the cursor.
#[test]
fn visual_tilde_repeats_with_dot() {
    let mut app = app_with("abc def\n");
    feed(&mut app, "vll~w.");
    assert_eq!(text(&app), "ABC DEF\n");
}

/// In Visual, `Alt-j`/`Alt-k` move the items the selection touches, and
/// the selection rides along for the next press.
#[test]
fn visual_alt_j_moves_the_selected_items_as_one_block() {
    let mut app = app_with("- a\n- b\n- c\n- d\n");
    feed(&mut app, "jVj"); // b and c
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- a\n- d\n- b\n- c\n");
    assert_eq!(app.mode, Mode::VisualLine, "still selecting");
    assert_eq!((app.anchor.unwrap().line, app.buffer.cursor.line), (2, 3));

    alt(&mut app, 'k');
    alt(&mut app, 'k');
    assert_eq!(text(&app), "- b\n- c\n- a\n- d\n");
    assert_eq!((app.anchor.unwrap().line, app.buffer.cursor.line), (0, 1));
    alt(&mut app, 'k'); // nothing left to pass
    assert_eq!(text(&app), "- b\n- c\n- a\n- d\n");
    assert_eq!(flash_text(&app), "first item");
}

/// A line in a sub-item stands for its parent when the selection also
/// touches a shallower item, so a parent never leaves its children.
#[test]
fn visual_alt_j_lifts_a_deeper_line_to_its_parent() {
    let mut app = app_with("- a\n  - a1\n- b\n- c\n");
    feed(&mut app, "jllvj"); // from inside a1 to b
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- c\n- a\n  - a1\n- b\n");
    assert_eq!(app.mode, Mode::Visual);
    assert_eq!(app.anchor.unwrap(), Cursor::new(2, 2), "the anchor stays on its character");
    assert_eq!(app.buffer.cursor.line, 3);
}

#[test]
fn visual_alt_j_within_sub_items_stays_under_the_parent() {
    let mut app = app_with("- a\n  - x\n  - y\n  - z\n- b\n");
    feed(&mut app, "jVj");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- a\n  - z\n  - x\n  - y\n- b\n");
    alt(&mut app, 'j');
    assert_eq!(flash_text(&app), "last item");
    assert_eq!(text(&app), "- a\n  - z\n  - x\n  - y\n- b\n");
}

#[test]
fn visual_alt_j_renumbers_and_keeps_the_gap_of_a_loose_list() {
    let mut app = app_with("1. a\n2. b\n3. c\n");
    feed(&mut app, "Vj");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "1. c\n2. a\n3. b\n");
    assert_eq!((app.anchor.unwrap().line, app.buffer.cursor.line), (1, 2));

    // A blank line touches nothing: here only b moves, and the end that
    // was on the blank line lands on it.
    let mut app = app_with("- a\n\n- b\n\n- c\n");
    feed(&mut app, "jVj");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- a\n\n- c\n\n- b\n");
    assert_eq!((app.anchor.unwrap().line, app.buffer.cursor.line), (4, 4));
}

/// Each press is its own undo step: two presses, two undos.
#[test]
fn visual_alt_j_undoes_one_press_at_a_time() {
    let mut app = app_with("- a\n- b\n- c\n- d\n");
    feed(&mut app, "V");
    alt(&mut app, 'j');
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- b\n- c\n- a\n- d\n");
    esc(&mut app);
    feed(&mut app, "u");
    assert_eq!(text(&app), "- b\n- a\n- c\n- d\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), "- a\n- b\n- c\n- d\n");
}

/// However Visual is left, no selection survives it, and Normal-mode
/// `Alt-j` moves only the item under the cursor.
#[test]
fn alt_j_after_leaving_visual_moves_one_item() {
    for leave in ["Vj:", "Vj/", "Vj?", "VjA", "VjI"] {
        let mut app = app_with("- a\n- b\n- c\n- d\n");
        feed(&mut app, leave);
        esc(&mut app);
        assert!(app.anchor.is_none(), "no selection survives {leave}");
        // To line 3 from the top: `I` leaves the cursor on the first line.
        feed(&mut app, "ggjj");
        assert_eq!(app.mode, Mode::Normal);
        alt(&mut app, 'j');
        assert_eq!(text(&app), "- a\n- b\n- d\n- c\n", "after {leave}");
    }
}

/// A drag can leave the anchor on the empty line past the last newline;
/// it is kept on the last character, and a move takes the real last line.
#[test]
fn an_anchor_past_the_last_line_is_kept_inside_the_text() {
    let mut app = app_with("a\nbbb\nccc\n");
    app.config.keys.visual.insert("K".into(), "move_lines_up".into());
    app.keymap = Keymap::from_config(&app.config.keys, &app.config.input.leader).0;
    feed(&mut app, "jv");
    app.anchor = Some(Cursor::new(app.buffer.last_line() + 1, 0));
    app.sync_after_input();
    assert_eq!(app.anchor, Some(Cursor::new(2, 2)), "the last character");
    feed(&mut app, "K");
    assert_eq!(text(&app), "bbb\nccc\na\n");
    assert_eq!(app.anchor, Some(Cursor::new(1, 2)));
    feed(&mut app, "d");
    // Both moved lines were still selected, up to the last character.
    assert_eq!(text(&app), "\na\n");
}

/// A Visual session that a pane key ends is not a change `.` repeats:
/// `.` keeps the change before it, and the pane key is never replayed.
#[test]
fn a_visual_session_ended_by_a_pane_key_leaves_dot_alone() {
    let mut app = app_with("- a\n- b\n- c\n- d\n");
    app.split_pane(true);
    app.split_pane(true);
    feed(&mut app, "x");
    let after_x = text(&app);
    feed(&mut app, "u");
    feed(&mut app, "jV");
    alt(&mut app, 'j');
    ctrl(&mut app, 'w');
    feed(&mut app, "q");
    assert_eq!(app.layout.ids().len(), 2);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.pending.is_clean(), "no half-typed pane key left");
    assert_eq!(text(&app), "- a\n- c\n- b\n- d\n");
    // `.` is still the `u`: it undoes the move, closes no pane.
    feed(&mut app, ".");
    assert_eq!(app.layout.ids().len(), 2);
    assert_eq!(text(&app), "- a\n- b\n- c\n- d\n");
    assert_ne!(text(&app), after_x);
    assert_eq!(app.mode, Mode::Normal);

    // The same for `<C-w>v`.
    let mut app = app_with("- a\n- b\n- c\n- d\n");
    feed(&mut app, "V");
    alt(&mut app, 'j');
    ctrl(&mut app, 'w');
    feed(&mut app, "v");
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.dot.is_empty(), "nothing filed for `.`");
}

/// `.` that ends up in its own recording (`x`, then `2.`) runs once, it
/// does not replay itself until the stack runs out.
#[test]
fn a_counted_dot_does_not_recurse() {
    let mut app = app_with("abcdefghij\n");
    feed(&mut app, "x2.");
    feed(&mut app, ".");
    assert!(text(&app).len() < "abcdefghij\n".len());
}

/// Moving to the file tree ends a selection too.
#[test]
fn moving_to_the_tree_ends_a_selection() {
    let mut app = app_with("- a\n- b\n");
    app.tree = Some(FileTree::open(std::env::current_dir().unwrap()));
    ctrl(&mut app, 'w');
    feed(&mut app, "l");
    feed(&mut app, "V");
    alt(&mut app, 'j');
    ctrl(&mut app, 'w');
    feed(&mut app, "h");
    assert!(app.tree.as_ref().unwrap().focused);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.anchor.is_none());
    assert!(app.dot.is_empty());
}

/// Blank lines inside a list that touch no item get their own message.
#[test]
fn a_selection_of_blank_lines_says_it_touches_no_item() {
    let mut app = app_with("- a\n\n  - a1\n- b\n");
    feed(&mut app, "jV");
    alt(&mut app, 'j');
    assert_eq!(flash_text(&app), "selection touches no item");
    assert_eq!(text(&app), "- a\n\n  - a1\n- b\n");
}

/// In a `.` replay the text can shrink under the anchor between keys,
/// before `sync_after_input` would keep it inside: `move_lines` clamps.
#[test]
fn move_lines_in_a_replay_with_the_anchor_past_the_end() {
    let mut app = app_with("a\nb\nc\nd\n");
    app.config.keys.visual.insert("J".into(), "join_lines".into());
    app.config.keys.visual.insert("K".into(), "move_lines_up".into());
    app.keymap = Keymap::from_config(&app.config.keys, &app.config.input.leader).0;
    feed(&mut app, "GVkJK");
    esc(&mut app);
    feed(&mut app, "G.");
    assert!(text(&app).ends_with('\n'), "no newline lost: {:?}", text(&app));
    assert!(!text(&app).contains("\n\n"), "no line made up: {:?}", text(&app));
}

/// A selection belongs to the pane it was made in. Focusing, splitting
/// or closing panes ends it in Normal, even one dragged out of Insert,
/// so `d` can never act on text nobody selected.
#[test]
fn a_selection_ends_when_focus_leaves_its_pane() {
    let mut app = app_with("one two three\n");
    let first = app.focus_pane;
    app.split_pane(true);
    assert_ne!(app.focus_pane, first);
    feed(&mut app, "v");
    app.visual_resumes_insert = true;
    app.focus_pane_id(first);
    assert_eq!(app.mode, Mode::Normal, "not back into an insert session");
    assert!(app.anchor.is_none());

    // Splitting moves focus to the new pane.
    let mut app = app_with("one two three\n");
    feed(&mut app, "v");
    app.split_pane(true);
    app.sync_after_input();
    assert_eq!(app.mode, Mode::Normal);

    // Closing the pane hands focus to the other one.
    let mut app = app_with("one two three\n");
    app.split_pane(true);
    feed(&mut app, "Vd");
    assert_eq!(text(&app), "", "deleted in the pane it was selected in");
    feed(&mut app, "u");
    feed(&mut app, "V");
    app.close_pane();
    app.sync_after_input();
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.anchor.is_none());
}

/// Leaving Visual from a drag out of Insert drops the resume too, so the
/// tinted caret does not linger.
#[test]
fn leaving_visual_any_way_drops_the_insert_resume() {
    for leave in [":", "/", "A"] {
        let mut app = app_with("one two\n");
        feed(&mut app, "v");
        app.visual_resumes_insert = true;
        feed(&mut app, leave);
        esc(&mut app);
        assert!(!app.visual_resumes_insert, "after {leave}");
    }
}

/// A selection only on a loose item's inner blank line is that item, as
/// the line is for Normal-mode `Alt-j`.
#[test]
fn visual_alt_j_on_a_blank_line_inside_an_item() {
    let mut app = app_with("- a\n\n  para\n- b\n");
    feed(&mut app, "jV");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- b\n- a\n\n  para\n");
}

/// Other Alt chords in Visual do what the bare key does, and the Visual
/// move can be turned off in `[keys.visual]`.
#[test]
fn visual_alt_chords_and_rebinding() {
    let mut app = app_with("one two three\n");
    feed(&mut app, "v");
    app.on_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT));
    app.sync_after_input();
    assert_eq!(app.mode, Mode::Visual);
    assert_eq!(app.buffer.cursor.col, 4, "Alt-w is w");

    let mut app = app_with("- a\n- b\n");
    app.config.keys.visual.insert("<M-j>".into(), "nop".into());
    app.keymap = Keymap::from_config(&app.config.keys, &app.config.input.leader).0;
    feed(&mut app, "V");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- a\n- b\n");
}

#[test]
fn visual_alt_j_refuses_what_is_not_one_list() {
    let mut app = app_with("- a\n- b\n1. c\n2. d\n");
    feed(&mut app, "jVj"); // b and c: two lists
    alt(&mut app, 'j');
    assert_eq!(text(&app), "- a\n- b\n1. c\n2. d\n");
    assert_eq!(flash_text(&app), "selection spans two lists");

    let mut app = app_with("text\n- a\n- b\n");
    feed(&mut app, "Vj");
    alt(&mut app, 'j');
    assert_eq!(text(&app), "text\n- a\n- b\n");
    assert_eq!(flash_text(&app), "not in a list");
}

/// The move is an action like any other, so `keys.conf` can rebind it.
#[test]
fn list_item_moves_can_be_rebound() {
    let mut app = app_with_keys("- a\n- b\n", &[("<C-j>", "list_item_down")]);
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    app.sync_after_input();
    assert_eq!(text(&app), "- b\n- a\n");
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    app.on_key(KeyEvent::new(code, modifiers));
    app.sync_after_input();
}

/// The writer's flow: Enter continues the list, Tab nests the new item,
/// Shift-Tab brings it back — without leaving Insert.
#[test]
fn tab_in_insert_nests_a_list_item() {
    let mut app = app_with("- a\n");
    feed(&mut app, "A");
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    feed(&mut app, "b");
    assert_eq!(text(&app), "- a\n    - b\n");
    assert_eq!(app.mode, Mode::Insert);
    key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
    feed(&mut app, "c");
    assert_eq!(text(&app), "- a\n- bc\n", "the cursor kept its place in the text");
}

#[test]
fn tab_on_an_ordered_item_renumbers() {
    let mut app = app_with("1. a\n2. b\n");
    app.buffer.cursor = Cursor::new(1, 3);
    feed(&mut app, "i");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), "1. a\n    1. b\n");
}

/// The same refusals as `>al`, and Insert carries on.
#[test]
fn tab_on_a_first_item_is_refused() {
    let mut app = app_with("- a\n");
    feed(&mut app, "A");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), "- a\n");
    assert_eq!(flash_text(&app), "nothing to indent under");
    assert_eq!(app.mode, Mode::Insert);
}

/// Off a list item's own line, or with `list_tab` off, Tab is a tab.
#[test]
fn tab_elsewhere_inserts_a_tab_unit() {
    let mut app = app_with("prose\n");
    feed(&mut app, "A");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), "prose    \n");
    let mut app = app_with("- a\n- b\n");
    app.config.editor.list_tab = false;
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, "A");
    key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(text(&app), "- a\n- b    \n");
}

/// Terminals send Shift-Tab as a key of its own; it must be the key a
/// config writes as `<S-Tab>`.
#[test]
fn shift_tab_is_the_s_tab_of_a_config() {
    let ev = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    let parsed = crate::config::keys::parse_seq("<S-Tab>", " ").unwrap();
    assert_eq!(Key::from_event(ev), Some(parsed[0]));
}

/// `>il` leaves the children where they are, so they re-parent.
#[test]
fn indent_an_item_without_its_children() {
    let mut app = app_with("- a\n- b\n  - c\n");
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, ">il");
    assert_eq!(text(&app), "- a\n  - b\n  - c\n");
}

/// A list shifts by the step the file already nests by, not `tab_width`:
/// found in another list when its own does not nest, and taken from the
/// children an item joins when it has some.
#[test]
fn indent_matches_the_files_own_step() {
    let mut app = app_with("- x\n  - y\n\ntext\n\n- a\n- b\n");
    app.buffer.cursor = Cursor::new(6, 0);
    feed(&mut app, ">al");
    assert_eq!(text(&app), "- x\n  - y\n\ntext\n\n- a\n  - b\n");
    // Joining children three columns in, it lines up with them.
    let mut app = app_with("- a\n   - a1\n- b\n");
    app.buffer.cursor = Cursor::new(2, 0);
    feed(&mut app, ">al");
    assert_eq!(text(&app), "- a\n   - a1\n   - b\n");
    // With nothing nested anywhere, one step is `tab_width`.
    let mut app = app_with("- a\n- b\n");
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, ">al");
    assert_eq!(text(&app), "- a\n    - b\n");
}

/// Out, an item lands on its parent's column, one level up — never past
/// it, whatever `tab_width` is.
#[test]
fn outdent_lands_on_the_parents_column() {
    let mut app = app_with("- a\n  - b\n    - c\n");
    app.buffer.cursor = Cursor::new(2, 0);
    feed(&mut app, "<al");
    assert_eq!(text(&app), "- a\n  - b\n  - c\n");
}

/// A file that nests with tabs keeps nesting with tabs.
#[test]
fn a_tab_nested_list_indents_with_a_tab() {
    let mut app = app_with("- a\n\t- a1\n- b\n");
    app.config.editor.expand_tab = false;
    app.buffer.cursor = Cursor::new(2, 0);
    feed(&mut app, ">al");
    assert_eq!(text(&app), "- a\n\t- a1\n\t- b\n");
    feed(&mut app, "<al");
    assert_eq!(text(&app), "- a\n\t- a1\n- b\n");
}

#[test]
fn delete_and_select_a_list_item() {
    let mut app = app_with("- a\n  - a1\n- b\n");
    feed(&mut app, "dal");
    assert_eq!(text(&app), "- b\n");
    let mut app = app_with("- a\n  - a1\n- b\n");
    feed(&mut app, "val");
    assert_eq!(app.mode, Mode::VisualLine);
    assert_eq!(app.buffer.cursor.line, 1);
}

/// Outside a list the object fails like `i(` outside parentheses.
#[test]
fn list_object_outside_a_list_does_nothing() {
    let mut app = app_with("prose\n");
    feed(&mut app, ">al");
    assert_eq!(text(&app), "prose\n");
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn case_operators() {
    let mut app = app_with("hello world\n");
    feed(&mut app, "gUiw");
    assert_eq!(text(&app), "HELLO world\n");
    feed(&mut app, "guu");
    assert_eq!(text(&app), "hello world\n");
    // Bare `u` is still undo, not a case operator.
    feed(&mut app, "u");
    assert_eq!(text(&app), "HELLO world\n");
}

#[test]
fn named_registers_are_independent() {
    let mut app = app_with("alpha\nbravo\ncharlie\n");
    feed(&mut app, "\"ayy"); // yank line 0 into "a
    feed(&mut app, "jdd"); // delete bravo into the unnamed register
    assert_eq!(text(&app), "alpha\ncharlie\n");
    feed(&mut app, "\"ap"); // pastes "a, not the deleted line
    assert_eq!(text(&app), "alpha\ncharlie\nalpha\n");
    feed(&mut app, "p"); // the unnamed register still holds bravo
    assert_eq!(text(&app), "alpha\ncharlie\nalpha\nbravo\n");
}

/// A yank fills `"0`, which a later delete does not clobber.
#[test]
fn the_yank_register_survives_a_delete() {
    let mut app = app_with("keep\ndrop\nlast\n");
    feed(&mut app, "yy");
    feed(&mut app, "jdd");
    assert_eq!(text(&app), "keep\nlast\n");
    feed(&mut app, "\"0p");
    assert_eq!(text(&app), "keep\nlast\nkeep\n");
}

/// An uppercase register name appends instead of replacing.
#[test]
fn uppercase_register_appends() {
    let mut app = app_with("one\ntwo\n");
    feed(&mut app, "\"ayy");
    feed(&mut app, "j\"Ayy");
    feed(&mut app, "\"ap");
    assert_eq!(text(&app), "one\ntwo\none\ntwo\n");
}

#[test]
fn change_inner_word_leaves_insert_mode_open() {
    let mut app = app_with("the quick fox\n");
    feed(&mut app, "wciw");
    assert_eq!(text(&app), "the  fox\n");
    assert_eq!(app.mode, Mode::Insert);
    feed(&mut app, "slow");
    esc(&mut app);
    assert_eq!(text(&app), "the slow fox\n");
}

#[test]
fn delete_around_word_takes_the_trailing_space() {
    let mut app = app_with("the quick fox\n");
    feed(&mut app, "wdaw");
    assert_eq!(text(&app), "the fox\n");
}

/// Like vim, a bracket object needs the cursor inside the pair or on one of
/// its brackets — from outside there is no object and nothing happens.
#[test]
fn delete_inside_brackets_and_quotes() {
    let mut app = app_with("call(one, two)\n");
    feed(&mut app, "di(");
    assert_eq!(text(&app), "call(one, two)\n", "cursor outside: no object");
    feed(&mut app, "$di(");
    assert_eq!(text(&app), "call()\n");

    let mut app = app_with("say \"hello\" now\n");
    feed(&mut app, "da\"");
    assert_eq!(text(&app), "say now\n");
}

/// `dap` is linewise and takes the paragraph's blank line with it.
#[test]
fn delete_around_paragraph() {
    let mut app = app_with("one\ntwo\n\nthree\n");
    feed(&mut app, "dap");
    assert_eq!(text(&app), "three\n");
}

/// `i`/`a` only mean "text object" while an operator is pending — bare `i`
/// must still enter Insert.
#[test]
fn bare_i_still_inserts() {
    let mut app = app_with("abc\n");
    feed(&mut app, "iX");
    assert_eq!(app.mode, Mode::Insert);
    assert_eq!(text(&app), "Xabc\n");
}

/// In Visual mode the object becomes the selection, and any verb follows.
#[test]
fn visual_selects_a_text_object() {
    let mut app = app_with("the quick fox\n");
    feed(&mut app, "wviwd");
    assert_eq!(text(&app), "the  fox\n");
}

/// A `.` repeat replays the whole object command, not just the operator.
#[test]
fn dot_repeats_a_text_object_command() {
    let mut app = app_with("aa bb cc\n");
    feed(&mut app, "daw");
    assert_eq!(text(&app), "bb cc\n");
    feed(&mut app, ".");
    assert_eq!(text(&app), "cc\n");
}

#[test]
fn delete_word() {
    let mut app = app_with("the quick fox\n");
    feed(&mut app, "dw");
    assert_eq!(text(&app), "quick fox\n");
}

/// `dw` on a line's last word keeps the line break: the next line is not
/// joined on.
#[test]
fn dw_on_the_last_word_of_a_line_keeps_the_newline() {
    let mut app = app_with("alpha\nbravo\ncharlie\n");
    feed(&mut app, "jdw");
    assert_eq!(text(&app), "alpha\n\ncharlie\n");
    let mut app = app_with("one two\nthree\n");
    feed(&mut app, "wdw");
    assert_eq!(text(&app), "one \nthree\n");
}

#[test]
fn yw_on_the_last_word_of_a_line_yanks_no_newline() {
    let mut app = app_with("alpha\nbravo\n");
    feed(&mut app, "jyw");
    let yanked = app.registers.get(&'"').expect("the unnamed register").text.clone();
    assert_eq!(yanked, "bravo");
}

/// The bug as found: on a file ending in a newline, `G` went to the empty
/// string ropey counts after it, so `GiX` then `:w` wrote `one\ntwo\nX\n`.
/// vim's `G` is the last line of text, and the file keeps its newline.
#[test]
fn g_then_insert_on_a_file_with_a_trailing_newline_edits_the_last_line() {
    for (on_disk, want) in [("one\ntwo\n", "one\nXtwo\n"), ("one\ntwo", "one\nXtwo")] {
        let path = std::env::temp_dir().join(format!(
            "shoin-G-{}-{}.md",
            on_disk.len(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, on_disk).unwrap();
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.buffer = Buffer::open(path.clone(), &[]).unwrap();
        app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width);

        feed(&mut app, "G");
        assert_eq!(app.buffer.cursor.line, 1, "{on_disk:?}: `G` is on `two`");
        feed(&mut app, "iX");
        esc(&mut app);
        app.buffer.save(SavePolicy::default(), false).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), want, "{on_disk:?}");
        let _ = std::fs::remove_file(path);
    }
}

/// `Go` opens one line below the last, with no blank line left between.
#[test]
fn g_then_o_opens_directly_below_the_last_line() {
    let mut app = app_with("one\ntwo\n");
    feed(&mut app, "GoX");
    esc(&mut app);
    assert_eq!(text(&app), "one\ntwo\nX\n");
}

/// `j` at the bottom and a counted `G` past the end both stop on the last
/// line of text.
#[test]
fn j_and_a_counted_g_stop_on_the_last_line_of_text() {
    let mut app = app_with("one\ntwo\n");
    feed(&mut app, "jj");
    assert_eq!(app.buffer.cursor.line, 1, "`j` on the last line stays put");
    feed(&mut app, "gg9G");
    assert_eq!(app.buffer.cursor.line, 1, "`9G` clamps to the last line");
    feed(&mut app, "ggVG");
    assert_eq!(app.buffer.cursor.line, 1, "visual `G` too");
    feed(&mut app, "d");
    assert_eq!(text(&app), "", "and `VGd` takes every line");
}

/// `dd` on the last line leaves the cursor on the new last line, not on
/// the empty string after the newline.
#[test]
fn dd_on_the_last_line_lands_on_the_line_above() {
    let mut app = app_with("one\ntwo\nthree\n");
    feed(&mut app, "Gdd");
    assert_eq!(text(&app), "one\ntwo\n");
    assert_eq!(app.buffer.cursor.line, 1, "on `two`");
    feed(&mut app, "dd");
    assert_eq!(text(&app), "one\n", "a second `dd` takes `two`, not a phantom line");
}

/// `yy` and `p` on the last line put the copy directly below it.
#[test]
fn yy_then_p_on_the_last_line_duplicates_it() {
    let mut app = app_with("one\ntwo\n");
    feed(&mut app, "Gyyp");
    assert_eq!(text(&app), "one\ntwo\ntwo\n");
    assert_eq!(app.buffer.cursor.line, 2);
    feed(&mut app, "P");
    assert_eq!(text(&app), "one\ntwo\ntwo\ntwo\n");
}

/// `dG` and `yG` cover through the last line of text and no further.
#[test]
fn d_g_and_y_g_run_to_the_last_line() {
    let mut app = app_with("one\ntwo\nthree\n");
    feed(&mut app, "jyG");
    let yanked = app.registers.get(&'"').expect("the unnamed register").text.clone();
    assert_eq!(yanked, "two\nthree\n");
    feed(&mut app, "dG");
    assert_eq!(text(&app), "one\n");
    assert_eq!(app.buffer.cursor.line, 0);
}

/// `J` on the last line has nothing to join, as in vim — it used to join
/// the trailing newline away.
#[test]
fn j_join_on_the_last_line_does_nothing() {
    let mut app = app_with("one\ntwo\n");
    let before = app.buffer.revision;
    feed(&mut app, "GJ");
    assert_eq!(text(&app), "one\ntwo\n");
    assert_eq!(app.buffer.revision, before, "nothing was changed");
}

/// `dap` on the last paragraph has no blank line after it to take, so it
/// takes the blank lines before it instead — vim's rule.
#[test]
fn dap_on_the_last_paragraph_takes_the_blank_lines_before_it() {
    let mut app = app_with("x\n\na\nb\n");
    feed(&mut app, "Gdap");
    assert_eq!(text(&app), "x\n");
}

/// Insert mode may still sit on the line an Enter at the very end opens,
/// where there was no trailing newline to begin with.
#[test]
fn insert_mode_can_type_on_the_line_after_the_last_newline() {
    let mut app = app_with("");
    feed(&mut app, "ia");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    assert_eq!(app.buffer.cursor.line, 1, "not clamped back while inserting");
    feed(&mut app, "b");
    assert_eq!(text(&app), "a\nb");
}

/// Trailing whitespace after the last word still goes, as in vim.
#[test]
fn dw_at_a_line_end_takes_trailing_whitespace() {
    let mut app = app_with("foo bar   \nbaz\n");
    feed(&mut app, "wdw");
    assert_eq!(text(&app), "foo \nbaz\n");
}

/// With a count the rule applies to the LAST word moved over: `d3w`
/// across a line break stops at the end of the line the third word ends.
#[test]
fn counted_dw_across_lines_stops_at_the_last_words_line_end() {
    // Started mid-line, the delete stays charwise.
    let mut app = app_with("x a\nb c\nd\n");
    feed(&mut app, "wd3w");
    assert_eq!(text(&app), "x \nd\n");
}

/// Started in the indent, a multi-line `dw` takes whole lines (vim's
/// `op_delete` rule).
#[test]
fn counted_dw_from_the_indent_deletes_whole_lines() {
    let mut app = app_with("a\nb c\nd\n");
    feed(&mut app, "d3w");
    assert_eq!(text(&app), "d\n");
    let mut app = app_with("  a\nb\nc\n");
    feed(&mut app, "^d2w");
    assert_eq!(text(&app), "c\n", "on the first non-blank is still in the indent");
    let mut app = app_with("  a\nb\nc\n");
    feed(&mut app, "^d3w");
    assert_eq!(text(&app), "", "to the end of the buffer");
}

/// An empty line is a word to `w`, so it is where the rule looks, and a
/// `dw` that starts on one is left as it was. Each case checked in vim.
#[test]
fn dw_around_empty_lines_matches_vim() {
    let cases = [
        ("a\n\nb\n", "dw", "\n\nb\n"),
        ("a\n\nb\n", "d2w", "b\n"),
        ("\nb\n", "dw", "b\n"),
    ];
    for (before, keys, after) in cases {
        let mut app = app_with(before);
        feed(&mut app, keys);
        assert_eq!(text(&app), after, "{keys} on {before:?}");
    }
}

/// `y` never goes linewise: the rule is `d`'s alone.
#[test]
fn counted_yw_across_lines_stays_charwise() {
    let mut app = app_with("a\nb c\nd\n");
    feed(&mut app, "y3w");
    let yanked = app.registers.get(&'"').expect("the unnamed register").text.clone();
    assert_eq!(yanked, "a\nb c");
}

/// `cw` on the blank before a line end changes the blanks, not the line
/// break.
#[test]
fn cw_on_trailing_blanks_keeps_the_newline() {
    let mut app = app_with("ab  \ncd\n");
    feed(&mut app, "llcw");
    esc(&mut app);
    assert_eq!(text(&app), "ab\ncd\n");
}

/// Operators, motions, counts and registers against vim. Every expected
/// buffer here was produced by running the same keys in `nvim --clean`.
#[test]
fn editing_commands_match_vim() {
    let cases = [
        ("foo bar   \n  baz\n", "wdw", "foo \n  baz\n"),
        ("ab  \n  cd\n", "lldw", "ab\n  cd\n"),
        ("1\n2\n3\n4\n5\n6\n7\n", "5dG", "6\n7\n"),
        ("1\n2\n3\n4\n5\n6\n7\n", "3j3dgg", "1\n2\n5\n6\n7\n"),
        ("foo bar\nbaz\n", "jdb", "foo \nbaz\n"),
        ("  a\nb\n", "jdb", "b\n"),
        ("a\nb\n\nc\n", "d}", "\nc\n"),
        ("\na\nb\n", "jjd{", "b\n"),
        ("a b\nc\n", "wd}", "a \n"),
        ("ab\n", "ldl", "a\n"),
        ("abc\n", "ld5l", "a\n"),
        ("ab\ncd\n", "\"ayy\"Ayl\"ap", "ab\nab\na\ncd\n"),
        ("ab\ncd\n", "\"ayl\"Ayy\"ap", "ab\na\nab\ncd\n"),
        ("ab\ncd\nef\n", "d2$", "ef\n"),
        ("ab\ncd\nef\n", "d2e", "ef\n"),
        ("a\nb\n", "dgg", "b\n"),
        ("a\nb\n", "jdG", "a\n"),
        ("one\n", "yy3p", "one\none\none\none\n"),
        ("  ab\n", "yypx", "  ab\n  b\n"),
        ("ab\n", "yl3p", "aaaab\n"),
        ("axbxcxd\n", "tx;;x", "axbxxd\n"),
        ("\n  foo\n", "wx", "\n  oo\n"),
        ("a\n   \nb\n", "wx", "a\n   \n\n"),
        ("foo bar\n", "dcw", "foo bar\n"),
        ("\n  foo\n", "dw", "  foo\n"),
        ("a  \nb c\n", "d2w", "c\n"),
        ("a\nb c\n", "d2w", "c\n"),
        ("a\nb\nc d\n", "d3w", "d\n"),
        ("foo\nbar\n", "jdw", "foo\n\n"),
        ("alpha\nbravo\ncharlie\n", "jdw", "alpha\n\ncharlie\n"),
    ];
    let mut failures = Vec::new();
    for (before, keys, after) in cases {
        let mut app = app_with(before);
        feed(&mut app, keys);
        if text(&app) != after {
            failures.push(format!("{keys} on {before:?}: got {:?}, vim {after:?}", text(&app)));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// `cw` on a word's last character changes that character alone (vim's
/// `end_word` with `stop`), not the next word as well.
#[test]
fn cw_on_a_words_last_character_changes_only_it() {
    for (before, keys, after) in [("a b\n", "cwX", "X b\n"), ("foo bar\n", "llcwX", "foX bar\n")] {
        let mut app = app_with(before);
        feed(&mut app, keys);
        esc(&mut app);
        assert_eq!(text(&app), after, "{keys} on {before:?}");
    }
}

/// A counted `w` whose last word is mid-line is not cut back to an
/// earlier line end, and the buffer's end counts as a line end.
#[test]
fn counted_dw_ending_mid_line_or_at_buffer_end_matches_vim() {
    let cases = [
        ("a\nb c\n", "d2w", "c\n"),
        ("x a\nb c d\n", "wd2w", "x c d\n"),
        ("a\nb\nc d\n", "d3w", "d\n"),
        ("a\nb", "d2w", ""),
        ("foo\nbar", "jdw", "foo\n"),
    ];
    for (before, keys, after) in cases {
        let mut app = app_with(before);
        feed(&mut app, keys);
        assert_eq!(text(&app), after, "{keys} on {before:?}");
    }
    let mut app = app_with("a\nb c");
    feed(&mut app, "y2w");
    let yanked = app.registers.get(&'"').expect("the unnamed register").text.clone();
    assert_eq!(yanked, "a\nb ");
}

#[test]
fn delete_to_end_of_word_inclusive() {
    // `de` deletes through the last char of the word.
    let mut app = app_with("alpha beta\n");
    feed(&mut app, "de");
    assert_eq!(text(&app), " beta\n");
}

#[test]
fn count_before_operator_and_motion() {
    let mut app = app_with("a b c d e\n");
    feed(&mut app, "d3w"); // delete three words
    assert_eq!(text(&app), "d e\n");
}

#[test]
fn doubled_operator_is_linewise() {
    let mut app = app_with("one\ntwo\nthree\n");
    feed(&mut app, "dd");
    assert_eq!(text(&app), "two\nthree\n");
    feed(&mut app, "2dd"); // delete two lines
    assert_eq!(text(&app), "");
}

#[test]
fn delete_char_with_count() {
    let mut app = app_with("hello\n");
    feed(&mut app, "3x");
    assert_eq!(text(&app), "lo\n");
}

#[test]
fn yank_line_and_paste_below() {
    let mut app = app_with("one\ntwo\n");
    feed(&mut app, "yyp");
    assert_eq!(text(&app), "one\none\ntwo\n");
    assert_eq!(app.buffer.cursor.line, 1);
}

#[test]
fn charwise_delete_and_paste() {
    let mut app = app_with("abcdef\n");
    feed(&mut app, "x"); // delete 'a' -> register "a", buffer "bcdef"
    feed(&mut app, "p"); // paste after cursor
    assert_eq!(text(&app), "bacdef\n");
}

#[test]
fn counted_motion_moves_without_editing() {
    let mut app = app_with("abcdef\n");
    feed(&mut app, "3l");
    assert_eq!(app.buffer.cursor.col, 3);
    assert_eq!(text(&app), "abcdef\n"); // unchanged
}

#[test]
fn escape_cancels_pending_operator() {
    let mut app = app_with("hello\n");
    app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
    esc(&mut app);
    feed(&mut app, "x"); // a normal delete-char, proving 'd' was cancelled
    assert_eq!(text(&app), "ello\n");
}

#[test]
fn visual_delete_selection_is_inclusive() {
    let mut app = app_with("abcdef\n");
    feed(&mut app, "vll"); // select a,b,c
    assert_eq!(app.mode, Mode::Visual);
    feed(&mut app, "d");
    assert_eq!(text(&app), "def\n");
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn visual_line_delete_removes_whole_lines() {
    let mut app = app_with("one\ntwo\nthree\n");
    feed(&mut app, "Vj"); // linewise select lines 0-1
    feed(&mut app, "d");
    assert_eq!(text(&app), "three\n");
}

#[test]
fn find_char_then_delete() {
    let mut app = app_with("hello, world\n");
    feed(&mut app, "df,"); // delete up to and including the comma
    assert_eq!(text(&app), " world\n");
}

fn ctrl(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    app.sync_after_input();
}

#[test]
fn undo_restores_a_delete() {
    let mut app = app_with("the quick fox\n");
    feed(&mut app, "dw");
    assert_eq!(text(&app), "quick fox\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), "the quick fox\n");
}

#[test]
fn redo_reapplies_the_undone_change() {
    let mut app = app_with("the quick fox\n");
    feed(&mut app, "dw");
    feed(&mut app, "u");
    assert_eq!(text(&app), "the quick fox\n");
    ctrl(&mut app, 'r');
    assert_eq!(text(&app), "quick fox\n");
}

#[test]
fn dd_is_undone_in_one_step() {
    let mut app = app_with("one\ntwo\nthree\n");
    feed(&mut app, "dd");
    assert_eq!(text(&app), "two\nthree\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), "one\ntwo\nthree\n");
}

#[test]
fn whole_insert_session_is_one_undo() {
    let mut app = app_with("X\n");
    feed(&mut app, "A"); // append -> Insert
    feed(&mut app, " hello");
    esc(&mut app);
    assert_eq!(text(&app), "X hello\n");
    feed(&mut app, "u"); // a single undo removes the whole insert
    assert_eq!(text(&app), "X\n");
}

#[test]
fn change_is_one_undo_across_delete_and_insert() {
    let mut app = app_with("foo bar\n");
    feed(&mut app, "C"); // change to EOL -> delete "foo bar", Insert
    feed(&mut app, "new");
    esc(&mut app);
    assert_eq!(text(&app), "new\n");
    feed(&mut app, "u"); // restores the original line in one step
    assert_eq!(text(&app), "foo bar\n");
}

#[test]
fn a_new_edit_clears_the_redo_stack() {
    let mut app = app_with("abcdef\n");
    feed(&mut app, "x"); // "bcdef"
    feed(&mut app, "u"); // "abcdef"
    feed(&mut app, "x"); // "bcdef" again — forks history, redo dropped
    ctrl(&mut app, 'r'); // nothing to redo
    assert_eq!(text(&app), "bcdef\n");
}

#[test]
fn gb_wraps_word_in_bold_and_toggles_off() {
    let mut app = app_with("make bold here\n");
    feed(&mut app, "w"); // onto "bold"
    feed(&mut app, "gb");
    assert_eq!(text(&app), "make **bold** here\n");
    // Cursor is inside; toggling again strips the markers.
    feed(&mut app, "gb");
    assert_eq!(text(&app), "make bold here\n");
}

#[test]
fn gi_italic_and_gk_code() {
    let mut app = app_with("word\n");
    feed(&mut app, "gi");
    assert_eq!(text(&app), "*word*\n");
    let mut app = app_with("word\n");
    feed(&mut app, "gk");
    assert_eq!(text(&app), "`word`\n");
}

/// `_word_` and `__word__` are emphasis too, and `_` is a word
/// character, so the word under the cursor INCLUDES the underscores. The
/// toggles act on the span the renderer styles, so they strip those
/// underscores rather than wrapping them in stars.
#[test]
fn gi_and_gb_strip_underscore_emphasis() {
    let cases = [
        ("gi", "_word_\n", "word\n"),
        ("gb", "__word__\n", "word\n"),
        ("gi", "say _hi_ now\n", "say hi now\n"),
        ("gb", "say __hi__ now\n", "say hi now\n"),
        ("gi", "_snake_case_\n", "snake_case\n"),
        // Anywhere in the span: the toggle is about the span.
        ("gi", "_two words_\n", "two words\n"),
        // Not emphasis: an underscore inside a word is plain text.
        ("gi", "snake_case\n", "*snake_case*\n"),
        ("gi", "_private\n", "*_private*\n"),
    ];
    for (keys, before, after) in cases {
        assert_eq!(toggled(keys, before), after, "{keys} on {before:?}");
    }
}

/// Run `keys` with the cursor on the first letter after the first
/// underscore (or the first letter, with none), and return the text.
fn toggled(keys: &str, before: &str) -> String {
    let mut app = app_with(before);
    let from = before.find('_').unwrap_or(0);
    let col = from + before[from..].find(char::is_alphanumeric).unwrap();
    app.buffer.cursor = Cursor::new(0, before[..col].chars().count());
    feed(&mut app, keys);
    text(&app)
}

/// The renderer styles only the OUTER span of a line, and inside a link's
/// text one level more: `_word_` inside `**…**` shows its underscores, so
/// it is bold text, not italic. The toggles follow that: they strip the
/// outer span when it has the emphasis, and otherwise lengthen its runs
/// to three, the one bold-and-italic the renderer draws.
#[test]
fn gi_and_gb_act_on_the_span_the_renderer_styles() {
    let cases = [
        ("gb", "**_word_**\n", "_word_\n"),
        ("gi", "_**word**_\n", "**word**\n"),
        ("gi", "*__word__*\n", "__word__\n"),
        ("gb", "__*word*__\n", "*word*\n"),
        // Not italic, as drawn: italic is added to the bold span.
        ("gi", "**_word_**\n", "***_word_***\n"),
        ("gi", "**word**\n", "***word***\n"),
        ("gi", "__word__\n", "___word___\n"),
        ("gb", "_word_\n", "___word___\n"),
        ("gi", "***word***\n", "**word**\n"),
        ("gb", "___word___\n", "_word_\n"),
        // Emphasis in a link's text is styled, so it is stripped there
        // too, or lengthened.
        ("gi", "[_word_](u)\n", "[word](u)\n"),
        ("gb", "[see __word__ now](u)\n", "[see word now](u)\n"),
        ("gb", "[**word** x](u)\n", "[word x](u)\n"),
        ("gi", "[**word** x](u)\n", "[***word*** x](u)\n"),
        ("gi", "[***word***](u)\n", "[**word**](u)\n"),
        // Emphasis in a link in emphasis is styled too, and is the span
        // acted on; without it, the emphasis holding the link is.
        ("gi", "**[_word_](u)**\n", "**[word](u)**\n"),
        ("gb", "**[_word_](u)**\n", "**[___word___](u)**\n"),
        ("gi", "**[word](u)**\n", "***[word](u)***\n"),
    ];
    for (keys, before, after) in cases {
        assert_eq!(toggled(keys, before), after, "{keys} on {before:?}");
    }
}

/// Where the renderer styles nothing inline — a fence, a table row — the
/// toggles look only at the characters, and only for the marker they
/// write: an underscore the reader sees literally is never stripped.
#[test]
fn toggles_read_no_emphasis_where_nothing_is_styled() {
    let mut app = app_with("```\n_word_\n*code*\n```\n");
    app.buffer.cursor = Cursor::new(1, 1);
    feed(&mut app, "gi");
    assert_eq!(text(&app), "```\n*_word_*\n*code*\n```\n");
    app.buffer.cursor = Cursor::new(2, 1);
    feed(&mut app, "gi");
    assert_eq!(text(&app), "```\n*_word_*\ncode\n```\n");

    let table = "| x | y |\n| - | - |\n| _a_ | b |\n";
    let mut app = app_with(table);
    app.buffer.cursor = Cursor::new(2, 3);
    feed(&mut app, "gi");
    assert_eq!(text(&app), "| x | y |\n| - | - |\n| *_a_* | b |\n");
    // With `[markdown] tables = false` the row renders as prose, styled.
    let mut app = app_with(table);
    app.config.markdown.tables = false;
    app.buffer.cursor = Cursor::new(2, 3);
    feed(&mut app, "gi");
    assert_eq!(text(&app), "| x | y |\n| - | - |\n| a | b |\n");
}

/// The same in Visual, whether the selection is the text between the
/// markers or includes them. A selection that runs OUT of a span is not
/// about that span, and is wrapped whole.
#[test]
fn visual_gi_and_gb_strip_underscore_emphasis() {
    let cases = [
        ("gi", "a _word_ b\n", (3, 6), "a word b\n"),
        ("gi", "a _word_ b\n", (2, 7), "a word b\n"),
        ("gb", "a __word__ b\n", (4, 7), "a word b\n"),
        ("gb", "a __word__ b\n", (2, 9), "a word b\n"),
        ("gi", "a *word* b\n", (3, 6), "a word b\n"),
        ("gi", "a *word* b\n", (2, 7), "a word b\n"),
        ("gb", "a **word** b\n", (2, 9), "a word b\n"),
        ("gi", "a _two words_ b\n", (3, 5), "a two words b\n"),
        // Underscores inside a word open nothing: wrap, do not strip.
        ("gi", "a_b_c\n", (2, 2), "a_*b*_c\n"),
        // Two spans selected together are not one span to strip.
        ("gi", "*a* *b*\n", (0, 6), "**a* *b**\n"),
        ("gb", "_a_ b\n", (0, 4), "**_a_ b**\n"),
        ("gi", "a **word** b\n", (2, 9), "a ***word*** b\n"),
        ("gb", "a _word_ b\n", (2, 7), "a ___word___ b\n"),
        // In a link's text, as outside one.
        ("gi", "[a _word_ b](u)\n", (3, 8), "[a word b](u)\n"),
        ("gb", "[a **word** b](u)\n", (5, 8), "[a word b](u)\n"),
    ];
    for (keys, before, (lo, hi), after) in cases {
        let mut app = app_with(before);
        app.buffer.cursor = Cursor::new(0, lo);
        feed(&mut app, "v");
        app.buffer.cursor = Cursor::new(0, hi);
        feed(&mut app, keys);
        assert_eq!(text(&app), after, "{keys} on {before:?} {lo}..={hi}");
        assert_eq!(app.mode, Mode::Normal);
    }
}

#[test]
fn gb_wraps_the_visual_selection() {
    let mut app = app_with("alpha beta gamma\n");
    feed(&mut app, "wv"); // start visual at "beta"
    feed(&mut app, "e"); // extend to end of "beta"
    feed(&mut app, "gb");
    assert_eq!(text(&app), "alpha **beta** gamma\n");
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn gt_toggles_and_adds_task_checkbox() {
    let mut app = app_with("- [ ] todo\n");
    feed(&mut app, "gt");
    assert_eq!(text(&app), "- [x] todo\n");
    feed(&mut app, "gt");
    assert_eq!(text(&app), "- [ ] todo\n");
    // A plain list item gains a checkbox.
    let mut app = app_with("- item\n");
    feed(&mut app, "gt");
    assert_eq!(text(&app), "- [ ] item\n");
}

/// Only the box after the marker is a task's: a `[x]` later on the line is
/// link text or prose and is left alone.
#[test]
fn gt_never_touches_brackets_past_the_marker() {
    let cases = [
        ("See [x](notes.md) here\n", "See [x](notes.md) here\n"),
        ("- see [x](notes.md)\n", "- [ ] see [x](notes.md)\n"),
        ("- [x] see [ ](a.md)\n", "- [ ] see [ ](a.md)\n"),
        ("  12. [ ] numbered\n", "  12. [x] numbered\n"),
    ];
    for (before, after) in cases {
        let mut app = app_with(before);
        feed(&mut app, "gt");
        assert_eq!(text(&app), after, "gt on {before:?}");
    }
}

/// `ga` aligns the whole table from any row, keeps the cursor on its
/// character, and is one undo step.
#[test]
fn ga_aligns_the_table_under_the_cursor() {
    let messy = "intro\n\n| Name | Role |\n|---|---|\n| Alice | Maintainer |\n| Bob | Docs |\n";
    let mut app = app_with(messy);
    feed(&mut app, "5G$ga");
    assert_eq!(
        text(&app),
        "intro\n\n| Name  | Role       |\n|-------|------------|\n| Alice | Maintainer |\n| Bob   | Docs       |\n"
    );
    assert_eq!(app.buffer.cursor, Cursor::new(4, 21), "still on the closing bar");
    feed(&mut app, "u");
    assert_eq!(text(&app), messy);
}

/// `table_align = "auto"`: Esc after typing in a table aligns it, and
/// one `u` takes back both.
#[test]
fn leaving_insert_in_a_table_aligns_it() {
    let messy = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut app = app_with(messy);
    feed(&mut app, "3G$iXYZ");
    esc(&mut app);
    assert_eq!(text(&app), "| a   | b     |\n|-----|-------|\n| 1   | 2 XYZ |\n");
    assert_eq!(app.buffer.cursor, Cursor::new(2, 12), "still on the Z");
    feed(&mut app, "u");
    assert_eq!(text(&app), messy);
}

#[test]
fn leaving_insert_without_an_edit_leaves_the_table() {
    let messy = "| a | b |\n|---|---|\n";
    let mut app = app_with(messy);
    feed(&mut app, "i");
    esc(&mut app);
    assert_eq!(text(&app), messy);
}

#[test]
fn table_align_manual_waits_for_ga() {
    let mut app = app_with("| a | b |\n|---|---|\n");
    app.config.editor.table_align = "manual".into();
    feed(&mut app, "AX");
    esc(&mut app);
    assert_eq!(text(&app), "| a | b |X\n|---|---|\n");
    feed(&mut app, "ga");
    assert_eq!(text(&app), "| a   | b   | X   |\n|-----|-----|-----|\n");
}

/// Aligning rewrites the header row, and it has to come out of that
/// still the table's header, not a paragraph of `|`s.
#[test]
fn aligning_keeps_the_header_row_a_header() {
    let mut app = app_with("# T\n\nlist\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nend\n");
    feed(&mut app, "7Go| longer cell | x |");
    esc(&mut app);
    app.refresh_blocks();
    assert_eq!(app.blocks.kinds[4], BlockKind::Table);
}

#[test]
fn ga_outside_a_table_says_so() {
    let mut app = app_with("just prose\n");
    feed(&mut app, "ga");
    assert_eq!(text(&app), "just prose\n");
    assert_eq!(flash_text(&app), "not in a table");
}

#[test]
fn heading_level_set_and_strip() {
    let mut app = app_with("Title\n");
    feed(&mut app, "g2");
    assert_eq!(text(&app), "## Title\n");
    feed(&mut app, "g4"); // re-level replaces the prefix
    assert_eq!(text(&app), "#### Title\n");
    feed(&mut app, "g0"); // strip
    assert_eq!(text(&app), "Title\n");
}

#[test]
fn writer_verb_is_one_undo() {
    let mut app = app_with("word\n");
    feed(&mut app, "gb");
    assert_eq!(text(&app), "**word**\n");
    feed(&mut app, "u"); // both inserted markers undone at once
    assert_eq!(text(&app), "word\n");
}

fn enter_key(app: &mut App) {
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
}

#[test]
fn search_forward_then_n_wraps() {
    let mut app = app_with("foo bar foo baz\n");
    feed(&mut app, "/foo");
    enter_key(&mut app);
    assert_eq!(app.buffer.cursor.col, 8); // second "foo"
    assert_eq!(app.mode, Mode::Normal);
    feed(&mut app, "n"); // wraps to the first
    assert_eq!(app.buffer.cursor.col, 0);
    feed(&mut app, "N"); // N reverses -> back to the second
    assert_eq!(app.buffer.cursor.col, 8);
}

#[test]
fn search_crosses_lines() {
    let mut app = app_with("alpha\nbeta target\n");
    feed(&mut app, "/target");
    enter_key(&mut app);
    assert_eq!(app.buffer.cursor.line, 1);
    assert_eq!(app.buffer.cursor.col, 5);
}

fn search_for(app: &mut App, sigil: char, pattern: &str) {
    feed(app, &format!("{sigil}{pattern}"));
    enter_key(app);
}

/// `/` starts AFTER the cursor and wraps to the top, where a match AT the
/// cursor counts: it is the only one, so the cursor stays on it.
#[test]
fn search_wraps_to_a_match_at_the_cursor() {
    let mut app = app_with("foo bar\n");
    search_for(&mut app, '/', "foo");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 0));
    assert!(!flash_text(&app).contains("not found"), "{}", flash_text(&app));
    let mut app = app_with("one\ntwo foo\nthree\n");
    app.buffer.cursor = Cursor::new(2, 2);
    search_for(&mut app, '/', "foo");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 4), "wrapped past the end");
}

/// `?` finds the last match that STARTS strictly before the cursor, even
/// one that runs over it, and wraps to the last match of the document.
#[test]
fn search_backward_is_strictly_before_the_cursor() {
    let mut app = app_with("aaaa\n");
    app.buffer.cursor = Cursor::new(0, 2);
    search_for(&mut app, '?', "aa");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 1), "overlaps the cursor");
    feed(&mut app, "n");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 0));
    feed(&mut app, "n");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 2), "wrapped to the last match");

    let mut app = app_with("foo\nbar foo\nbaz\n");
    app.buffer.cursor = Cursor::new(1, 4);
    search_for(&mut app, '?', "foo");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 0), "the match AT the cursor is skipped");
    feed(&mut app, "n");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 4), "wrapped to the end");
}

/// Positions are characters, not bytes: a search past accented letters,
/// CJK and an emoji lands on the character the match starts at.
#[test]
fn search_counts_multibyte_characters() {
    let mut app = app_with("é 日本 🎉 café\ncafé ü\n");
    search_for(&mut app, '/', "café");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 7));
    feed(&mut app, "n");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 0));
    feed(&mut app, "N");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 7));
    search_for(&mut app, '/', "ü");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 5));
    search_for(&mut app, '?', "日本");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 2));
}

/// The rope search against the obvious one over a `Vec<char>`, on random
/// text built from few letters (so matches overlap and repeat), multi-byte
/// characters and line breaks, at every start position.
#[test]
fn rope_search_matches_the_naive_search() {
    fn naive_find(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
        if needle.is_empty() || needle.len() > hay.len() {
            return None;
        }
        (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == needle[..])
    }
    fn naive_rfind(hay: &[char], needle: &[char], before: usize) -> Option<usize> {
        if needle.is_empty() || needle.len() > hay.len() {
            return None;
        }
        let max = before.min(hay.len() - needle.len());
        (0..=max).rev().find(|&i| hay[i..i + needle.len()] == needle[..])
    }
    let alphabet = ['a', 'b', 'é', '日', '\n', '\r', '\u{2028}'];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    for _ in 0..400 {
        let hay: String = (0..next(30)).map(|_| alphabet[next(alphabet.len())]).collect();
        let needle: String = (0..next(4)).map(|_| alphabet[next(alphabet.len())]).collect();
        let rope = ropey::Rope::from_str(&hay);
        let (h, n): (Vec<char>, Vec<char>) = (hay.chars().collect(), needle.chars().collect());
        for at in 0..=h.len() + 1 {
            assert_eq!(
                rope_find(&rope, &needle, at),
                naive_find(&h, &n, at),
                "find {needle:?} in {hay:?} from {at}"
            );
            assert_eq!(
                rope_rfind(&rope, &needle, at),
                naive_rfind(&h, &n, at),
                "rfind {needle:?} in {hay:?} before {at}"
            );
        }
    }
}

#[test]
fn search_escape_cancels() {
    let mut app = app_with("needle here\n");
    feed(&mut app, "/need");
    esc(&mut app);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.search.is_none()); // never executed
    assert_eq!(app.buffer.cursor.col, 0);
}

#[test]
fn dot_repeats_a_delete() {
    let mut app = app_with("aaaa\n");
    feed(&mut app, "x"); // "aaa"
    feed(&mut app, "."); // "aa"
    feed(&mut app, "."); // "a"
    assert_eq!(text(&app), "a\n");
}

#[test]
fn dot_repeats_a_multi_key_operator() {
    let mut app = app_with("one two three four\n");
    feed(&mut app, "dw"); // delete "one "
    assert_eq!(text(&app), "two three four\n");
    feed(&mut app, "."); // repeat: delete "two "
    assert_eq!(text(&app), "three four\n");
}

#[test]
fn dot_repeats_an_insert_session() {
    let mut app = app_with("X\n");
    feed(&mut app, "A"); // append -> Insert
    feed(&mut app, "!");
    esc(&mut app);
    assert_eq!(text(&app), "X!\n");
    feed(&mut app, "."); // repeat the whole A!<Esc>
    assert_eq!(text(&app), "X!!\n");
}

#[test]
fn dot_repeats_a_writer_verb() {
    let mut app = app_with("aa bb\n");
    feed(&mut app, "gb"); // **aa**
    feed(&mut app, "$"); // move onto "bb" (a motion, not a change)
    feed(&mut app, "."); // repeat gb on the word under cursor
    assert_eq!(text(&app), "**aa** **bb**\n");
}

#[test]
fn a_bare_motion_does_not_become_the_dot() {
    let mut app = app_with("hello world\n");
    feed(&mut app, "x"); // change: delete 'h' -> "ello world"
    feed(&mut app, "w"); // motion only, no edit — must not overwrite dot
    feed(&mut app, "."); // still repeats the delete
    assert_eq!(text(&app), "ello orld\n");
}

fn app_with_keys(text: &str, bindings: &[(&str, &str)]) -> App {
    let mut cfg = Config::default();
    for (k, a) in bindings {
        cfg.keys.normal.insert(k.to_string(), a.to_string());
    }
    app_with_config(text, cfg)
}

/// `move_lines_down` / `move_lines_up` bound in Visual, as people bind
/// `J` / `K`: the selected lines move a line a press and stay selected,
/// each press is one undo step, and a count moves further.
#[test]
fn visual_move_lines_keeps_the_selection() {
    let mut app = app_with("a\nb\nc\nd\ne\n");
    app.config.keys.visual.insert("J".into(), "move_lines_down".into());
    app.config.keys.visual.insert("K".into(), "move_lines_up".into());
    app.keymap = Keymap::from_config(&app.config.keys, &app.config.input.leader).0;

    feed(&mut app, "jVj"); // b and c
    feed(&mut app, "J");
    assert_eq!(text(&app), "a\nd\nb\nc\ne\n");
    assert!(matches!(app.mode, Mode::VisualLine), "still selecting");
    assert_eq!((app.anchor.unwrap().line, app.buffer.cursor.line), (2, 3));

    feed(&mut app, "J");
    assert_eq!(text(&app), "a\nd\ne\nb\nc\n");
    feed(&mut app, "J"); // nowhere left to go
    assert_eq!(text(&app), "a\nd\ne\nb\nc\n");

    feed(&mut app, "3K"); // a count past the top stops at it
    assert_eq!(text(&app), "b\nc\na\nd\ne\n");
    assert_eq!((app.anchor.unwrap().line, app.buffer.cursor.line), (0, 1));

    esc(&mut app);
    feed(&mut app, "u");
    assert_eq!(text(&app), "a\nd\ne\nb\nc\n", "one press, one undo step");
    feed(&mut app, "u");
    assert_eq!(text(&app), "a\nd\nb\nc\ne\n");
}

/// Outside Visual the cursor's line moves, and its column comes along.
#[test]
fn move_lines_in_normal_moves_the_cursor_line() {
    let mut app = app_with_keys("one\ntwo\nthree", &[("<C-j>", "move_lines_down")]);
    app.buffer.cursor = Cursor::new(0, 2);
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    app.sync_after_input();
    assert_eq!(text(&app), "two\none\nthree");
    assert_eq!(app.buffer.cursor, Cursor::new(1, 2));
    // Onto the last line, which has no newline after it.
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    app.sync_after_input();
    assert_eq!(text(&app), "two\nthree\none");
}

#[test]
fn leader_sequence_binding_fires() {
    // Default leader is space.
    let mut app = app_with_keys("word\n", &[("<leader>b", "toggle_bold")]);
    feed(&mut app, " b"); // <leader>b
    assert_eq!(text(&app), "**word**\n");
}

/// `<leader>hl` opens the shortcuts reference. Once open it owns all
/// input (like `:help`), so `q` is what closes it, not a second press.
#[test]
fn leader_hl_opens_the_shortcuts_help() {
    let mut app = app_with_keys("word\n", &[("<leader>hl", "help")]);
    feed(&mut app, " hl");
    assert_eq!(app.help.as_ref().map(|h| h.cats[h.cat].name), Some("Basics"));

    feed(&mut app, "q");
    assert!(app.help.is_none(), "q closes it");
}

fn cmd(app: &mut App, line: &str) {
    for c in line.chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
}

#[test]
fn screen_motions_h_m_l() {
    let doc: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    let mut app = app_with(&doc);
    app.term_size = (80, 30); // set a real viewport (app_with doesn't run())

    let at = |app: &mut App, key: &str| {
        app.buffer.cursor = Cursor::new(0, 0);
        feed(app, key);
        app.buffer.cursor.line
    };
    let (h, m, l) = (at(&mut app, "H"), at(&mut app, "M"), at(&mut app, "L"));
    assert!(h < m && m < l, "H<M<L, got {h} {m} {l}");
    assert!(h <= 5, "H near the top, got {h}");
    assert!(l >= 15, "L near the bottom, got {l}");

    // `dL` composes: delete from the top of the screen down to L.
    app.buffer.cursor = Cursor::new(0, 0);
    feed(&mut app, "dL");
    assert!(app.buffer.line_count() < 41, "dL removed the visible block");
}

fn scroll_app() -> App {
    let doc: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    let mut app = app_with(&doc);
    app.term_size = (80, 30);
    app
}

/// `Ctrl-e` moves the page, not the cursor, while the cursor has room.
#[test]
fn ctrl_e_scrolls_the_page_under_the_cursor() {
    let mut app = scroll_app();
    app.buffer.cursor = Cursor::new(10, 2);
    ctrl(&mut app, 'e');
    assert_eq!(app.scroll_hint(), 1);
    assert_eq!(app.buffer.cursor, Cursor::new(10, 2), "the cursor stays put");
    feed(&mut app, "4");
    ctrl(&mut app, 'e');
    assert_eq!(app.scroll_hint(), 5, "a count scrolls that many rows");
}

/// A cursor about to leave the screen is carried along, `scroll_off` in
/// from the edge, keeping its column.
#[test]
fn scrolling_carries_a_cursor_at_the_edge() {
    let mut app = scroll_app();
    let pad = app.config.editor.scroll_off as usize;
    app.buffer.cursor = Cursor::new(0, 3);
    feed(&mut app, "20");
    ctrl(&mut app, 'e');
    assert_eq!(app.scroll_hint(), 20);
    assert_eq!(app.buffer.cursor, Cursor::new(20 + pad, 3));

    feed(&mut app, "20");
    ctrl(&mut app, 'y');
    assert_eq!(app.scroll_hint(), 0);
    let bottom = app.viewport_height() - 1 - pad;
    assert_eq!(app.buffer.cursor, Cursor::new(bottom, 3));
    ctrl(&mut app, 'y');
    assert_eq!(app.scroll_hint(), 0, "nothing above the first line");
}

/// The wheel scrolls the page the same way, three rows a notch.
#[test]
fn the_wheel_scrolls_the_page() {
    let mut app = scroll_app();
    app.buffer.cursor = Cursor::new(10, 0);
    mouse(&mut app, MouseEventKind::ScrollDown, 10, 10);
    assert_eq!(app.scroll_hint(), 3);
    assert_eq!(app.buffer.cursor.line, 10);
    mouse(&mut app, MouseEventKind::ScrollUp, 10, 10);
    assert_eq!(app.scroll_hint(), 0);
    assert_eq!(app.buffer.cursor.line, 10);
}

/// Typewriter mode pins the cursor's row, so scrolling moves the cursor.
#[test]
fn scrolling_in_typewriter_mode_moves_the_cursor() {
    let mut app = scroll_app();
    app.config.layout.typewriter = true;
    app.buffer.cursor = Cursor::new(10, 0);
    ctrl(&mut app, 'e');
    assert_eq!(app.buffer.cursor.line, 11);
    ctrl(&mut app, 'y');
    assert_eq!(app.buffer.cursor.line, 10);
}

#[test]
fn delete_before_join_and_toggle_case() {
    let mut app = app_with("hello\n");
    feed(&mut app, "$"); // onto 'o'
    feed(&mut app, "X"); // delete char before -> remove 'l' -> "helo"
    assert_eq!(text(&app), "helo\n");

    let mut app = app_with("foo\nbar\n");
    feed(&mut app, "J"); // join -> "foo bar"
    assert_eq!(text(&app), "foo bar\n");

    let mut app = app_with("aBc\n");
    feed(&mut app, "3~"); // flip all three
    assert_eq!(text(&app), "AbC\n");
}

/// A count on `i`, `a`, `A` and `o`/`O` repeats what was typed, in
/// one undo step; `Ctrl-C` leaves without the copies.
#[test]
fn insert_counts_repeat_the_typed_text() {
    for (start, keys, want) in [
        ("\n", "3ix", "xxx\n"),
        ("ab\n", "2aXY", "aXYXYb\n"),
        ("ab\n", "3A;", "ab;;;\n"),
        ("one\n", "2otext", "one\ntext\ntext\n"),
        ("one\n", "2Otext", "text\ntext\none\n"),
    ] {
        let mut app = app_with(start);
        feed(&mut app, keys);
        esc(&mut app);
        assert_eq!(text(&app), want, "{keys}");
        assert_eq!(app.mode, Mode::Normal);
        feed(&mut app, "u");
        assert_eq!(text(&app), start, "{keys}: one undo step");
    }
    let mut app = app_with("\n");
    feed(&mut app, "3ix");
    ctrl(&mut app, 'c');
    assert_eq!(text(&app), "x\n", "Ctrl-C drops the copies");
    // Backspace and auto-pair replay as typed.
    let mut app = app_with("\n");
    feed(&mut app, "3iab");
    key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    feed(&mut app, "c");
    esc(&mut app);
    assert_eq!(text(&app), "acacac\n");
    let mut app = app_with("\n");
    app.config.editor.auto_pair = true;
    feed(&mut app, "2i(x");
    esc(&mut app);
    assert_eq!(text(&app), "(x)(x)\n");
    // An arrow key ends the count.
    let mut app = app_with("ab\n");
    feed(&mut app, "3ix");
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    esc(&mut app);
    assert_eq!(text(&app), "xab\n");
    // The count is part of the change `.` repeats.
    let mut app = app_with("\n");
    feed(&mut app, "2ix");
    esc(&mut app);
    feed(&mut app, ".");
    assert_eq!(text(&app), "xxxx\n");
}

/// `3.` replays the change with 3 in place of its own count, and a
/// later `.` keeps the 3.
#[test]
fn a_count_on_dot_replaces_the_changes_count() {
    let mut app = app_with("ab\n");
    feed(&mut app, "A;");
    esc(&mut app);
    feed(&mut app, "3.");
    assert_eq!(text(&app), "ab;;;;\n");
    feed(&mut app, ".");
    assert_eq!(text(&app), "ab;;;;;;;\n", "the 3 sticks");

    let mut app = app_with("a b c d e f g\n");
    feed(&mut app, "2dw");
    assert_eq!(text(&app), "c d e f g\n");
    feed(&mut app, "3.");
    assert_eq!(text(&app), "f g\n", "3 words, not 2 times 3");

    // An operator's own count is replaced too.
    let mut app = app_with("a b c d e f g h\n");
    feed(&mut app, "d2w3.");
    assert_eq!(text(&app), "f g h\n", "d2w then 3. is three words");
    // `1.` does not break `.` afterwards.
    let mut app = app_with("a b c d e\n");
    feed(&mut app, "dw1..");
    assert_eq!(text(&app), "d e\n");
    // A Visual change ignores the count rather than counting the `v`.
    let mut app = app_with("abcdef\n");
    feed(&mut app, "vld3.");
    assert_eq!(text(&app), "ef\n");

    let mut app = app_with("a b c d e f g\n");
    feed(&mut app, "\"qdw");
    feed(&mut app, "2.");
    assert_eq!(text(&app), "d e f g\n", "after a register");
    assert_eq!(app.registers.get(&'q').map(|r| r.text.as_str()), Some("b c "));
}

/// A count on the word objects takes that many words.
#[test]
fn counted_word_objects() {
    for (keys, want) in [
        ("d2aw", "c d e\n"),
        ("d3aw", "d e\n"),
        ("d2iw", "b c d e\n"),
        ("d3iw", " c d e\n"),
        ("wwd9aw", "a b \n"),
        ("daw", "b c d e\n"),
    ] {
        let mut app = app_with("a b c d e\n");
        feed(&mut app, keys);
        assert_eq!(text(&app), want, "{keys}");
    }
}

/// `cc` keeps the indent with `auto_indent`, and `yy` leaves the
/// cursor where it was; `yk` and `yip` take it up to where they start.
#[test]
fn cc_keeps_the_indent_and_yy_keeps_the_cursor() {
    let mut app = app_with("  indented\n");
    app.config.editor.auto_indent = true;
    feed(&mut app, "wccX");
    esc(&mut app);
    assert_eq!(text(&app), "  X\n");
    // Nothing typed: the unused indent goes.
    let mut app = app_with("  indented\n");
    app.config.editor.auto_indent = true;
    feed(&mut app, "cc");
    esc(&mut app);
    assert_eq!(text(&app), "\n");
    let mut app = app_with("  indented\n");
    app.config.editor.auto_indent = false;
    feed(&mut app, "ccX");
    esc(&mut app);
    assert_eq!(text(&app), "X\n");

    let mut app = app_with("one two three\nfour five six\n");
    feed(&mut app, "wwyy");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 8));
    feed(&mut app, "yj");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 8));
    feed(&mut app, "jyk");
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (0, 8));
    let mut app = app_with("p one\np two long line\n\nnext\n");
    feed(&mut app, "jwwyip");
    assert_eq!(app.buffer.cursor.line, 0, "yip goes to the paragraph's first line");
}

/// `F` and `T` are exclusive under an operator, so the character
/// under the cursor stays; a forward `t` to the very next character
/// still gives the operator that one character; `;` and `,` take an
/// operator too.
#[test]
fn find_motions_under_an_operator() {
    for (start, keys, want) in [
        ("abcxdef\n", "$dFx", "abcf\n"),
        ("abcxdef\n", "$dTx", "abcxf\n"),
        ("(a)\n", "ldt)", "()\n"),
        ("(a)\n", "lct)Z\x1b", "(Z)\n"),
        ("abcxdef\n", "$cTxZ\x1b", "abcxZf\n"),
        ("a.b.c.d\n", "f.0d;", "b.c.d\n"),
        ("a.b.c.d\n", "f.d;", "ac.d\n"),
        ("a.b.c.d\n", "f.;d,", "a.c.d\n"),
        ("a-b-c\n", "t-d;", "-c\n"),
        ("abcxdef\n", "dtx", "xdef\n"),
        ("(abc\n", "lcT(Z\x1b", "(Zabc\n"),
        ("(abc\n", "ldT(", "(abc\n"),
        ("abcxdef\n", "dfx", "def\n"),
    ] {
        let mut app = app_with(start);
        for c in keys.chars() {
            if c == '\x1b' {
                esc(&mut app);
            } else {
                feed(&mut app, &c.to_string());
            }
        }
        assert_eq!(text(&app), want, "{keys} on {start:?}");
    }
    let mut app = app_with("abcxdef\n");
    feed(&mut app, "$yFx");
    assert_eq!(app.registers.get(&'"').map(|r| r.text.as_str()), Some("xde"));
    assert_eq!(app.buffer.cursor.col, 3, "y leaves the cursor at the start");
}

/// As plain motions the finds land where they always did.
#[test]
fn find_motions_alone_are_unchanged() {
    for (keys, col) in [("$Fx", 3), ("$Tx", 4), ("tx", 2), ("ltx", 2), ("lltx", 2), ("fx", 3)] {
        let mut app = app_with("abcxdef\n");
        feed(&mut app, keys);
        assert_eq!(app.buffer.cursor.col, col, "{keys}");
        assert_eq!(text(&app), "abcxdef\n");
    }
}

#[test]
fn repeat_find_with_semicolon_and_comma() {
    let mut app = app_with("a.b.c.d\n");
    feed(&mut app, "f."); // to first '.'
    assert_eq!(app.buffer.cursor.col, 1);
    feed(&mut app, ";"); // next '.'
    assert_eq!(app.buffer.cursor.col, 3);
    feed(&mut app, ","); // reverse -> back to first '.'
    assert_eq!(app.buffer.cursor.col, 1);
}

#[test]
fn star_searches_word_under_cursor() {
    let mut app = app_with("foo bar foo\n");
    feed(&mut app, "*"); // search "foo" forward -> second occurrence
    assert_eq!(app.buffer.cursor.col, 8);
}

#[test]
fn gl_wraps_word_as_link_and_enters_insert() {
    let mut app = app_with("word\n");
    feed(&mut app, "gl");
    assert_eq!(text(&app), "[word]()\n");
    assert_eq!(app.mode, Mode::Insert);
    feed(&mut app, "url"); // typed into the URL slot
    assert_eq!(text(&app), "[word](url)\n");
}

#[test]
fn window_commands_move_focus_between_panes() {
    let mut app = app_with("hi\n");
    app.tree = Some(FileTree::open(std::env::current_dir().unwrap()));
    assert!(app.tree.as_ref().unwrap().focused, "opens focused");

    ctrl(&mut app, 'w');
    feed(&mut app, "l"); // Ctrl-w l -> editor
    assert!(!app.tree.as_ref().unwrap().focused);

    ctrl(&mut app, 'w');
    feed(&mut app, "h"); // Ctrl-w h -> tree
    assert!(app.tree.as_ref().unwrap().focused);

    ctrl(&mut app, 'w');
    feed(&mut app, "w"); // Ctrl-w w -> cycle back to editor
    assert!(!app.tree.as_ref().unwrap().focused);
}

/// Markup typed into an existing line has to take effect on the NEXT frame,
/// not on the next time the file is opened. End to end: keystrokes in, the
/// concealed display text out.
#[test]
fn markup_typed_into_a_line_conceals_without_a_reload() {
    let mut app = app_with("intro\n\nplain para\n\ntail\n");
    // The event loop refreshes the blocks once per key; without that first
    // pass the setup's own edit is still pending and forces a FULL rebuild,
    // which is exactly the path this test must not take.
    app.refresh_blocks();

    feed(&mut app, "jj"); // onto "plain para"
    feed(&mut app, "I- "); // make it a list item
    esc(&mut app);
    app.refresh_blocks(); // the frame right after the edit
    feed(&mut app, "gg"); // move off, so the line conceals
    app.refresh_blocks();

    let mut cache = app.cache.borrow_mut();
    cache.sync(&app, 40);
    assert_eq!(
        cache.entry(2).map(|e| e.cmap.display_text(&e.source)),
        Some("• plain para".to_string())
    );
}

/// Splitting shows the same document twice; each pane then goes its own
/// way — its own buffer, its own scroll.
#[test]
fn splitting_gives_each_pane_its_own_view() {
    let dir = two_files();
    let mut app = app_with("draft\n");
    app.term_size = (100, 24);
    assert_eq!(app.layout.count(), 1);

    app.split_pane(true);
    assert_eq!(app.layout.count(), 2);
    assert_ne!(app.focus_pane, 1, "focus follows the new pane");
    assert_eq!(app.current(), 0, "which still shows the same document");

    // Opening a file changes THIS pane only.
    app.open_file(dir.join("one.md"));
    assert_eq!(app.current(), 1);
    let other = app.layout.ids().into_iter().find(|id| *id != app.focus_pane).unwrap();
    assert_eq!(app.layout.pane(other).unwrap().doc, 0, "the other pane is untouched");

    // …and moving back returns to the first document.
    app.focus_pane_id(other);
    assert_eq!(app.current(), 0);
    assert_eq!(text(&app), "draft\n");
    std::fs::remove_dir_all(&dir).ok();
}

/// Two panes on ONE document keep their own place in it.
#[test]
fn each_pane_remembers_its_own_cursor() {
    let mut app = app_with(&(1..=40).map(|i| format!("line {i}\n")).collect::<String>());
    app.term_size = (100, 24);
    feed(&mut app, "gg");

    app.split_pane(true);
    let right = app.focus_pane;
    let left = app.layout.ids().into_iter().find(|id| *id != right).unwrap();
    feed(&mut app, "20j");
    assert_eq!(app.buffer.cursor.line, 20);

    // Back to the left pane: it is still at the top.
    app.focus_pane_id(left);
    assert_eq!(app.buffer.cursor.line, 0, "the left pane never moved");
    feed(&mut app, "5j");

    // …and the right pane kept its own position.
    app.focus_pane_id(right);
    assert_eq!(app.buffer.cursor.line, 20);
    app.focus_pane_id(left);
    assert_eq!(app.buffer.cursor.line, 5);
}

/// A saved cursor goes stale when the OTHER pane shortens the document.
/// It is clamped on the way back in rather than tracked through every edit.
#[test]
fn a_stale_pane_cursor_is_clamped_not_trusted() {
    let mut app = app_with(&(1..=40).map(|i| format!("line {i}\n")).collect::<String>());
    app.term_size = (100, 24);
    feed(&mut app, "gg");
    app.split_pane(true);
    let right = app.focus_pane;
    let left = app.layout.ids().into_iter().find(|id| *id != right).unwrap();

    feed(&mut app, "35j"); // the right pane sits near the end
    app.focus_pane_id(left);
    feed(&mut app, "30dd"); // …which the left pane then deletes

    app.focus_pane_id(right);
    let last = app.buffer.line_count() - 1;
    assert!(app.buffer.cursor.line <= last, "clamped into the document that is left");
}

#[test]
fn window_commands_split_navigate_and_close() {
    let mut app = app_with("hi\n");
    app.term_size = (100, 24);

    app.window_command('v'); // vertical split
    app.window_command('s'); // then split THAT pane horizontally
    assert_eq!(app.layout.count(), 3);

    let left = app.layout.ids()[0];
    app.window_command('h'); // back to the left column
    assert_eq!(app.focus_pane, left);
    app.window_command('l'); // into the right column's top pane
    assert_ne!(app.focus_pane, left);
    app.window_command('j'); // and down to the one below it
    let bottom = app.focus_pane;
    app.window_command('k');
    assert_ne!(app.focus_pane, bottom);

    app.window_command('o'); // only this one
    assert_eq!(app.layout.count(), 1);
    assert!(!app.close_pane(), "the last pane never closes");
}

/// `<leader>sv` opens a split and closes it again — the toggle symmetry
/// every panel in this editor follows.
/// `<C-w>>` and friends, through the real key path — and a count multiplies
/// the step, which the grammar hands over for free.
#[test]
fn window_resize_keys_move_the_boundary() {
    let mut app = app_with("hi\n");
    app.term_size = (100, 24);
    app.split_pane(true);

    let area = app.pane_area();
    let width = |app: &App| {
        app.layout
            .geometry(app.pane_area())
            .panes
            .into_iter()
            .find(|(id, _)| *id == app.focus_pane)
            .map(|(_, r)| r.width)
            .unwrap()
    };
    let before = width(&app);
    assert!(area.width > 0);

    ctrl(&mut app, 'w');
    feed(&mut app, ">");
    assert_eq!(width(&app), before + 4, "one step is four columns");

    // A count multiplies it.
    feed(&mut app, "3");
    ctrl(&mut app, 'w');
    feed(&mut app, "<");
    assert_eq!(width(&app), before + 4 - 12);

    ctrl(&mut app, 'w');
    feed(&mut app, "=");
    assert_eq!(width(&app), before, "equalized");
}

#[test]
fn the_split_binding_toggles() {
    let mut app = app_with("hi\n");
    app.term_size = (100, 24);
    app.run(Command::bare(Action::ToggleSplit { vertical: true }));
    assert_eq!(app.layout.count(), 2);
    app.run(Command::bare(Action::ToggleSplit { vertical: true }));
    assert_eq!(app.layout.count(), 1);
}

/// Closing a document has to fix up every pane pointing past it, or a pane
/// would be left indexing a document that moved.
#[test]
fn closing_a_buffer_repoints_the_panes() {
    let dir = two_files();
    let mut app = app_with("draft\n");
    app.term_size = (100, 24);
    app.open_file(dir.join("one.md")); // doc 1
    app.open_file(dir.join("two.md")); // doc 2, current

    app.split_pane(true); // second pane, also on doc 2
    let second = app.focus_pane;
    let keep = app.layout.ids().into_iter().find(|id| *id != second).unwrap();
    app.focus_pane_id(keep);
    app.switch_to(0); // the LEFT pane shows doc 0

    // Close doc 1, which sits between them, from the other pane.
    app.focus_pane_id(second);
    app.switch_to(1);
    cmd(&mut app, ":bd");
    assert_eq!(app.docs.len(), 2);
    for id in app.layout.ids() {
        let doc = app.layout.pane(id).unwrap().doc;
        assert!(doc < app.docs.len(), "pane {id} points at a live document");
    }
    app.focus_pane_id(keep);
    assert_eq!(text(&app), "draft\n", "still showing what it was");
    std::fs::remove_dir_all(&dir).ok();
}

/// Two files on disk, for the multi-buffer tests.
/// A scratch directory of two files, unique per call.
///
/// The counter is not decoration. Naming these by clock alone let two tests
/// running in parallel land on the same directory, and whichever finished
/// first deleted the other's files out from under it — a failure that
/// showed up about once in four full runs and passed every time it was
/// re-run alone.
fn two_files() -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let n = N.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("shoin-bufs-{t}-{n}"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("one.md"), "file one\n").unwrap();
    std::fs::write(d.join("two.md"), "file two\n").unwrap();
    d
}

// ------------------------------------------------- file tree operations

/// A tree focused on a scratch directory, cursor on `one.md`.
fn tree_on(dir: &std::path::Path) -> App {
    let mut app = app_with("scratch\n");
    app.term_size = (100, 30);
    app.tree = Some(FileTree::open(dir.to_path_buf()));
    app.tree.as_mut().unwrap().select_path(&dir.join("one.md"));
    app
}

/// Type into an open prompt and answer it.
fn prompt_type(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        app.sync_after_input();
    }
}
fn prompt_enter(app: &mut App) {
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
}
/// Clear a pre-filled prompt.
fn prompt_clear(app: &mut App) {
    app.on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    app.sync_after_input();
}

/// `a` creates a file beside the selected one; a trailing `/` makes a
/// directory instead.
#[test]
fn tree_creates_files_and_directories() {
    let dir = two_files();
    let mut app = tree_on(&dir);

    feed(&mut app, "a");
    assert!(matches!(app.mode, Mode::Prompt(_)), "a opens a prompt");
    prompt_type(&mut app, "new.md");
    prompt_enter(&mut app);
    assert!(dir.join("new.md").is_file(), "created beside one.md");
    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(
        app.tree.as_ref().unwrap().selected_entry().unwrap().path,
        dir.join("new.md"),
        "and the cursor followed it"
    );

    feed(&mut app, "a");
    prompt_type(&mut app, "sub/");
    prompt_enter(&mut app);
    assert!(dir.join("sub").is_dir(), "a trailing / makes a directory");

    std::fs::remove_dir_all(&dir).ok();
}

/// With a DIRECTORY selected, `a` creates inside it — that is what pointing
/// at a directory means.
#[test]
fn tree_creates_inside_a_selected_directory() {
    let dir = two_files();
    std::fs::create_dir(dir.join("notes")).unwrap();
    let mut app = tree_on(&dir);
    app.tree.as_mut().unwrap().select_path(&dir.join("notes"));

    feed(&mut app, "a");
    prompt_type(&mut app, "inner.md");
    prompt_enter(&mut app);
    assert!(dir.join("notes/inner.md").is_file());
    std::fs::remove_dir_all(&dir).ok();
}

/// The leader keys work from the tree: `<leader>hl` opens help at the
/// tree's section — `q` there comes back to the tree — and `<leader>fe`
/// closes the tree it opened.
#[test]
fn leader_keys_work_from_the_tree() {
    let mut app = app_with_keys("x\n", &[("<leader>hl", "help"), ("<leader>fe", "file_tree")]);
    let root = app.cwd();
    app.toggle_tree(root);
    feed(&mut app, " hl");
    let h = app.help.as_ref().expect("help open");
    assert_eq!(h.cats[h.cat].name, "Files");
    assert!(h.rows()[h.mark.unwrap()].plain().contains("File tree"));
    app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(app.help.is_none());
    assert!(app.tree.as_ref().is_some_and(|t| t.focused), "back in the tree");
    feed(&mut app, " fe");
    assert!(app.tree.is_none(), "the same key that opened it closes it");
}

/// Opening a file leaves the tree beside the editor for the next one,
/// unless `tree.close_on_open` asks for it to go.
#[test]
fn tree_stays_open_after_opening_unless_asked() {
    let dir = two_files();
    let mut app = tree_on(&dir);
    app.tree.as_mut().unwrap().focused = true;
    app.tree.as_mut().unwrap().select_path(&dir.join("one.md"));
    feed(&mut app, "l");
    assert_eq!(app.buffer.path.as_deref(), Some(dir.join("one.md").as_path()));
    assert!(app.tree.as_ref().is_some_and(|t| !t.focused), "open, focus handed back");

    let mut app = tree_on(&dir);
    app.config.tree.close_on_open = true;
    app.tree.as_mut().unwrap().focused = true;
    app.tree.as_mut().unwrap().select_path(&dir.join("one.md"));
    feed(&mut app, "l");
    assert_eq!(app.buffer.path.as_deref(), Some(dir.join("one.md").as_path()));
    assert!(app.tree.is_none(), "closed once the file opened");
    std::fs::remove_dir_all(&dir).ok();
}

/// `r` renames in place, and an OPEN buffer follows its file — otherwise
/// the next `:w` would recreate the old name.
#[test]
fn tree_renames_and_carries_the_open_buffer() {
    let dir = two_files();
    let mut app = tree_on(&dir);
    app.open_file(dir.join("one.md"));
    assert_eq!(app.buffer.path.as_deref(), Some(dir.join("one.md").as_path()));
    app.tree.as_mut().unwrap().focused = true;
    app.tree.as_mut().unwrap().select_path(&dir.join("one.md"));

    feed(&mut app, "r");
    let Mode::Prompt(p) = app.mode.clone() else { panic!("no prompt") };
    assert_eq!(p.input, "one.md", "pre-filled with the current name");
    prompt_clear(&mut app);
    prompt_type(&mut app, "renamed.md");
    prompt_enter(&mut app);

    assert!(!dir.join("one.md").exists());
    assert!(dir.join("renamed.md").is_file());
    assert_eq!(
        app.buffer.path.as_deref(),
        Some(dir.join("renamed.md").as_path()),
        "the open buffer followed its file"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `m` moves, creating the destination's parents, and takes every buffer
/// under a moved directory with it.
#[test]
fn tree_moves_paths_and_nested_buffers() {
    let dir = two_files();
    std::fs::create_dir(dir.join("box")).unwrap();
    std::fs::write(dir.join("box/deep.md"), "deep\n").unwrap();

    let mut app = tree_on(&dir);
    app.open_file(dir.join("box/deep.md"));
    app.tree.as_mut().unwrap().focused = true;
    app.tree.as_mut().unwrap().refresh();
    app.tree.as_mut().unwrap().select_path(&dir.join("box"));

    feed(&mut app, "m");
    // The prompt is pre-filled with the current relative path; clear it.
    let Mode::Prompt(p) = app.mode.clone() else { panic!("no prompt") };
    assert_eq!(p.input, "box", "pre-filled with where it is now");
    prompt_clear(&mut app);
    prompt_type(&mut app, "archive/box");
    prompt_enter(&mut app);

    assert!(dir.join("archive/box/deep.md").is_file());
    assert_eq!(
        app.buffer.path.as_deref(),
        Some(dir.join("archive/box/deep.md").as_path()),
        "a buffer BENEATH the moved directory moved too"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `d` asks first, and only `y` goes through.
#[test]
fn tree_deletes_only_after_a_yes() {
    let dir = two_files();
    let mut app = tree_on(&dir);

    // Anything that is not a yes is a no.
    feed(&mut app, "d");
    assert!(matches!(app.mode, Mode::Prompt(_)));
    feed(&mut app, "n");
    assert!(dir.join("one.md").exists(), "n kept the file");
    assert!(matches!(app.mode, Mode::Normal));

    feed(&mut app, "d");
    feed(&mut app, "y");
    assert!(!dir.join("one.md").exists(), "y deleted it");

    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------- buffer path identity

/// `abs` spelled relative to the working directory, by climbing to `/`
/// with `..` and back down: a relative path to a scratch file that does
/// not need the test to change directory (tests run in parallel).
#[cfg(unix)]
fn relative_to_cwd(abs: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap();
    let mut rel = PathBuf::new();
    for _ in cwd.components().skip(1) {
        rel.push("..");
    }
    rel.join(abs.strip_prefix("/").unwrap())
}

#[test]
fn clean_path_is_absolute_and_lexical() {
    // A base that exists nowhere: `clean_path` follows a symlink before
    // `..`, and on macOS `/home` is one (to `/System/Volumes/Data/home`).
    let base = Path::new("/shoin-no-such-dir/me/notes");
    assert_eq!(clean_path(base, Path::new("n.md")), PathBuf::from("/shoin-no-such-dir/me/notes/n.md"));
    assert_eq!(clean_path(base, Path::new("./n.md")), PathBuf::from("/shoin-no-such-dir/me/notes/n.md"));
    assert_eq!(clean_path(base, Path::new("sub/../n.md")), PathBuf::from("/shoin-no-such-dir/me/notes/n.md"));
    assert_eq!(clean_path(base, Path::new("../x/./n.md")), PathBuf::from("/shoin-no-such-dir/me/x/n.md"));
    assert_eq!(clean_path(base, Path::new("/a/./b/../c.md")), PathBuf::from("/a/c.md"));
    assert_eq!(clean_path(base, Path::new("../../../../n.md")), PathBuf::from("/n.md"));
    // Nothing has to exist: a new file has a path too.
    assert_eq!(
        clean_path(base, Path::new("not/yet/there.md")),
        PathBuf::from("/shoin-no-such-dir/me/notes/not/yet/there.md")
    );
}

/// `shoin README.md`, then the same file from the tree (absolute) or by
/// `:e ./README.md`: one buffer, not two. README.md is read, never written.
#[test]
fn a_bare_name_and_its_other_spellings_are_one_buffer() {
    let cwd = std::env::current_dir().unwrap();
    let mut app = App::new(Config::default(), Some(PathBuf::from("README.md")), None).unwrap();
    assert_eq!(app.buffer.path.as_deref(), Some(cwd.join("README.md").as_path()));
    assert_eq!(app.buffer.display_name(), "README.md", "the screen still shows the short name");

    assert!(app.open_file(cwd.join("README.md")));
    assert_eq!(app.docs.len(), 1, "the tree's absolute path finds the open buffer");
    ex(&mut app, "e ./README.md");
    assert_eq!(app.docs.len(), 1, ":e ./README.md finds it too");
    ex(&mut app, "e src/../README.md");
    assert_eq!(app.docs.len(), 1, "and so does a path through ..");
}

/// `shoin sub/../n.md` and `shoin ./n.md` name the file `n.md` does.
#[cfg(unix)]
#[test]
fn startup_paths_are_cleaned_before_they_are_kept() {
    let dir = two_files();
    std::fs::create_dir(dir.join("sub")).unwrap();
    let one = dir.join("one.md");
    let via_sub = relative_to_cwd(&dir.join("sub")).join("..").join("one.md");
    let mut app = App::new(Config::default(), Some(via_sub), None).unwrap();
    assert_eq!(app.buffer.path.as_deref(), Some(one.as_path()));
    assert!(app.open_file(one.clone()));
    assert_eq!(app.docs.len(), 1);

    let dotted = PathBuf::from(".").join(relative_to_cwd(&one));
    let mut app = App::new(Config::default(), Some(dotted), None).unwrap();
    assert_eq!(app.buffer.path.as_deref(), Some(one.as_path()));
    app.open_file(relative_to_cwd(&one));
    assert_eq!(app.docs.len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

/// `link/..` is the parent of where `link` points, as the shell has it.
#[cfg(unix)]
#[test]
fn clean_path_follows_a_symlink_before_its_dot_dot() {
    // Canonical, because the temp folder may itself sit behind a symlink
    // (`/var` is `/private/var` on macOS), which `..` would resolve.
    let dir = std::fs::canonicalize(two_files()).unwrap();
    std::fs::create_dir_all(dir.join("real/sub")).unwrap();
    std::os::unix::fs::symlink(dir.join("real/sub"), dir.join("link")).unwrap();
    assert_eq!(
        clean_path(&dir, Path::new("link/../x.md")),
        dir.join("real/x.md"),
    );
    assert_eq!(clean_path(&dir, Path::new("real/sub/../x.md")), dir.join("real/x.md"));
    std::fs::remove_dir_all(&dir).ok();
}

/// Renaming a symlink in the tree leaves the buffer of the file it points
/// at alone: that file did not move.
#[cfg(unix)]
#[test]
fn renaming_a_symlink_leaves_its_targets_buffer() {
    let dir = two_files();
    std::os::unix::fs::symlink(dir.join("one.md"), dir.join("link.md")).unwrap();
    let mut app = App::new(Config::default(), Some(dir.join("one.md")), None).unwrap();
    app.move_path(&dir.join("link.md"), &dir.join("alias.md")).unwrap();
    assert_eq!(app.buffer.path.as_deref(), Some(dir.join("one.md").as_path()));
    std::fs::remove_dir_all(&dir).ok();
}

/// A symlink and the file it points at are one buffer.
#[cfg(unix)]
#[test]
fn a_symlink_and_its_target_are_one_buffer() {
    let dir = two_files();
    std::os::unix::fs::symlink(dir.join("one.md"), dir.join("link.md")).unwrap();
    let mut app = App::new(Config::default(), Some(dir.join("one.md")), None).unwrap();
    assert!(app.open_file(dir.join("link.md")));
    assert_eq!(app.docs.len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

/// `shoin n.md`, rename it in the tree: the buffer follows, and `:w`
/// writes the new name rather than recreating the old one.
#[cfg(unix)]
#[test]
fn a_tree_rename_carries_a_buffer_opened_by_relative_path() {
    let dir = two_files();
    let rel = relative_to_cwd(&dir.join("one.md"));
    let mut app = App::new(Config::default(), Some(rel), None).unwrap();
    app.move_path(&dir.join("one.md"), &dir.join("m.md")).unwrap();
    assert_eq!(app.buffer.path.as_deref(), Some(dir.join("m.md").as_path()));
    feed(&mut app, "Ax");
    esc(&mut app);
    ex(&mut app, "w");
    assert!(!dir.join("one.md").exists(), ":w did not recreate the old name");
    assert_eq!(std::fs::read_to_string(dir.join("m.md")).unwrap(), "file onex\n");
    std::fs::remove_dir_all(&dir).ok();
}

/// A buffer opened through a symlinked folder (`/home` → `/var/home`)
/// still follows a rename the tree makes under the real path.
#[cfg(unix)]
#[test]
fn a_rename_finds_a_buffer_spelled_through_a_symlinked_folder() {
    let dir = two_files();
    let alias = dir.with_extension("alias");
    std::os::unix::fs::symlink(&dir, &alias).unwrap();
    let mut app = App::new(Config::default(), Some(alias.join("one.md")), None).unwrap();
    app.move_path(&dir.join("one.md"), &dir.join("m.md")).unwrap();
    assert_eq!(app.buffer.path.as_deref(), Some(dir.join("m.md").as_path()));
    std::fs::remove_file(&alias).ok();
    std::fs::remove_dir_all(&dir).ok();
}

/// `:w <path>` keeps the path in the same clean, absolute form.
#[cfg(unix)]
#[test]
fn write_as_keeps_a_clean_absolute_path() {
    let dir = two_files();
    let mut app = app_with("new\n");
    let target = relative_to_cwd(&dir.join("sub")).join("..").join("new.md");
    ex(&mut app, &format!("w {}", target.display()));
    assert_eq!(app.buffer.path.as_deref(), Some(dir.join("new.md").as_path()));
    assert!(app.open_file(dir.join("new.md")));
    assert_eq!(app.docs.len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

// ------------------------------------------------------------ huge counts

/// A count typed by a sat-on keyboard neither panics (debug overflow) nor
/// allocates without bound, and leaves the buffer where undo can fix it.
#[test]
fn a_huge_count_neither_panics_nor_runs_away() {
    let original = "one two\nthree\nfour\n";
    for keys in ["p", "j", "$", "x", "J", "~", "dd", "G", "rx"] {
        let mut app = app_with(original);
        feed(&mut app, "yy");
        feed(&mut app, "99999999999999999999999");
        feed(&mut app, keys);
        let c = app.buffer.cursor;
        assert!(c.line <= app.buffer.last_line(), "{keys}: cursor line {c:?}");
        assert!(c.col <= app.buffer.line_len(c.line), "{keys}: cursor col {c:?}");
        assert!(app.pending.is_clean(), "{keys}: nothing left pending");
        assert!(app.buffer.rope.len_bytes() < 16 << 20, "{keys}: bounded");
        if text(&app) != original {
            feed(&mut app, "u");
        }
        assert!(text(&app) == original, "{keys}: undo restores the text");
    }
}

/// `100000p` of a 1 KB register would be 100 MB of text: refused, with a
/// message, before anything is allocated.
#[test]
fn a_paste_too_large_is_refused() {
    let mut app = app_with("x\n");
    app.registers.insert('"', Register { text: "a".repeat(1024), linewise: false });
    feed(&mut app, "100000p");
    assert_eq!(app.buffer.rope.len_bytes(), 2, "nothing pasted");
    assert!(flash_text(&app).contains("paste: too large"), "got: {}", flash_text(&app));
    // A sane count of the same register still pastes.
    feed(&mut app, "3p");
    assert_eq!(app.buffer.line_text(0).len(), 1 + 3 * 1024);
}

/// The limit is on the COUNT: one copy of a big register always pastes.
#[test]
fn one_copy_of_a_big_register_still_pastes() {
    let mut app = app_with("x\n");
    let big = (16 << 20) + 1;
    app.registers.insert('"', Register { text: "a".repeat(big), linewise: false });
    feed(&mut app, "p");
    assert_eq!(app.buffer.rope.len_bytes(), 2 + big);
}

/// A directory deletion says how much is going — it is the only warning
/// that `d` on that row is recursive.
#[test]
fn deleting_a_directory_counts_what_it_takes() {
    let dir = two_files();
    std::fs::create_dir(dir.join("box")).unwrap();
    std::fs::write(dir.join("box/a.md"), "").unwrap();
    std::fs::write(dir.join("box/b.md"), "").unwrap();

    let mut app = tree_on(&dir);
    app.tree.as_mut().unwrap().refresh();
    app.tree.as_mut().unwrap().select_path(&dir.join("box"));
    feed(&mut app, "d");
    let Mode::Prompt(p) = app.mode.clone() else { panic!("no prompt") };
    assert_eq!(
        p.kind,
        PromptKind::Delete { entries: 3 },
        "the directory and both files"
    );
    feed(&mut app, "y");
    assert!(!dir.join("box").exists());
    std::fs::remove_dir_all(&dir).ok();
}

/// The root row is the frame of reference for every path shown; it cannot
/// be renamed, moved or deleted from inside the pane.
#[test]
fn the_tree_root_is_not_editable() {
    let dir = two_files();
    let mut app = tree_on(&dir);
    app.tree.as_mut().unwrap().select_first();
    assert!(app.tree.as_ref().unwrap().selected_is_root());

    for key in ["r", "m", "d"] {
        feed(&mut app, key);
        assert!(matches!(app.mode, Mode::Normal), "{key} refused on the root");
    }
    // …but `a` inside the root is exactly right.
    feed(&mut app, "a");
    assert!(matches!(app.mode, Mode::Prompt(_)));
    prompt_type(&mut app, "top.md");
    prompt_enter(&mut app);
    assert!(dir.join("top.md").is_file());
    assert!(dir.exists(), "the root itself survived");
    std::fs::remove_dir_all(&dir).ok();
}

/// A collision is reported, never written through.
#[test]
fn tree_operations_refuse_to_clobber() {
    let dir = two_files();
    let mut app = tree_on(&dir);

    feed(&mut app, "r");
    prompt_clear(&mut app);
    prompt_type(&mut app, "two.md");
    prompt_enter(&mut app);

    assert!(dir.join("one.md").exists(), "the source is still there");
    assert_eq!(std::fs::read_to_string(dir.join("two.md")).unwrap(), "file two\n");
    let msg = app.flash.as_ref().and_then(|f| f.text.clone()).unwrap_or_default();
    assert!(msg.contains("already exists"), "got: {msg:?}");
    std::fs::remove_dir_all(&dir).ok();
}

/// `Ctrl-W` deletes like vim's command line: the trailing spaces,
/// then one run of word characters or one run of symbols.
#[test]
fn ctrl_w_deletes_the_last_word() {
    for (from, want) in [
        ("ab cd", "ab "),
        ("ab cd  ", "ab "),
        ("one.md", "one."),
        ("one..", "one"),
        ("wq", ""),
        ("", ""),
        ("   ", ""),
        ("été ça", "été "),
    ] {
        let mut s = from.to_string();
        delete_last_word(&mut s);
        assert_eq!(s, want, "{from:?}");
    }
}

/// In the `:` and `/` `?` lines a control chord is never typed as
/// its letter. `Ctrl-C` cancels, `Ctrl-U` clears, `Ctrl-W` deletes a
/// word, `Ctrl-H` is a backspace, and anything else is ignored.
#[test]
fn control_keys_in_the_command_and_search_lines() {
    let field = |app: &App| match &app.mode {
        Mode::Command(s) => Some(s.clone()),
        Mode::Search { query, .. } => Some(query.clone()),
        _ => None,
    };
    for open in [":", "/", "?"] {
        let mut app = app_with("text\n");
        feed(&mut app, open);
        feed(&mut app, "ab cd");
        ctrl(&mut app, 'w');
        assert_eq!(field(&app).as_deref(), Some("ab "), "{open} Ctrl-W");
        ctrl(&mut app, 'h');
        assert_eq!(field(&app).as_deref(), Some("ab"), "{open} Ctrl-H");
        ctrl(&mut app, 'x');
        alt(&mut app, 'q');
        assert_eq!(field(&app).as_deref(), Some("ab"), "{open} other chords ignored");
        ctrl(&mut app, 'u');
        assert_eq!(field(&app).as_deref(), Some(""), "{open} Ctrl-U");
        feed(&mut app, "zz");
        ctrl(&mut app, 'c');
        assert_eq!(app.mode, Mode::Normal, "{open} Ctrl-C cancels");
        assert_eq!(text(&app), "text\n");
    }
    // Ctrl-Shift-W is Ctrl-W; Alt- and Ctrl-Backspace delete a word.
    let mut app = app_with("text\n");
    feed(&mut app, ":ab cd ef");
    key(&mut app, KeyCode::Char('W'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    key(&mut app, KeyCode::Backspace, KeyModifiers::ALT);
    assert_eq!(app.mode, Mode::Command("ab ".into()));
    key(&mut app, KeyCode::Backspace, KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Backspace, KeyModifiers::CONTROL);
    assert_eq!(app.mode, Mode::Command("".into()), "a word delete never cancels");

    // Ctrl-C cancels: the command typed so far does not run.
    let mut app = app_with("text\n");
    feed(&mut app, ":d");
    ctrl(&mut app, 'c');
    assert_eq!(text(&app), "text\n");
    assert!(app.search.is_none());
}

/// The finder's query takes the same editing keys, but an empty
/// query stays open on Backspace.
#[test]
fn control_keys_in_the_finder() {
    let dir = two_files();
    let mut app = app_with("text\n");
    app.open_finder(dir.clone());
    feed(&mut app, "ab cd");
    ctrl(&mut app, 'w');
    assert_eq!(app.finder.as_ref().unwrap().query, "ab ");
    ctrl(&mut app, 'h');
    ctrl(&mut app, 'x');
    assert_eq!(app.finder.as_ref().unwrap().query, "ab");
    ctrl(&mut app, 'u');
    key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    assert!(app.finder.is_some(), "an empty query stays open");
    ctrl(&mut app, 'c');
    assert!(app.finder.is_none());
    std::fs::remove_dir_all(&dir).ok();
}

/// The tree's prompts take the same keys. `Ctrl-y` is not a yes.
#[test]
fn control_keys_in_a_tree_prompt() {
    let dir = two_files();
    let mut app = tree_on(&dir);
    feed(&mut app, "r"); // opens pre-filled with "one.md"
    ctrl(&mut app, 'w');
    assert!(matches!(&app.mode, Mode::Prompt(p) if p.input == "one."), "{:?}", app.mode);
    ctrl(&mut app, 'h');
    ctrl(&mut app, 'x');
    assert!(matches!(&app.mode, Mode::Prompt(p) if p.input == "one"), "{:?}", app.mode);
    ctrl(&mut app, 'c');
    assert_eq!(app.mode, Mode::Normal);
    assert!(dir.join("one.md").exists(), "nothing renamed");

    feed(&mut app, "d");
    ctrl(&mut app, 'y');
    assert_eq!(app.mode, Mode::Normal);
    assert!(dir.join("one.md").exists(), "Ctrl-y did not delete");
    std::fs::remove_dir_all(&dir).ok();
}

/// Esc abandons a prompt with nothing done — including a delete.
#[test]
fn a_prompt_can_be_abandoned() {
    let dir = two_files();
    let mut app = tree_on(&dir);
    feed(&mut app, "r");
    prompt_type(&mut app, "zzz");
    esc(&mut app);
    assert!(matches!(app.mode, Mode::Normal));
    assert!(dir.join("one.md").exists());
    assert!(!dir.join("zzz").exists());
    std::fs::remove_dir_all(&dir).ok();
}

/// Opening a file no longer displaces the one you are editing — and no
/// longer refuses because it has unsaved changes.
#[test]
fn opening_a_file_adds_a_buffer_and_keeps_the_old_one() {
    let dir = two_files();
    let mut app = app_with("draft\n");
    assert!(app.buffer.modified, "the setup typed into buffer 1");

    assert!(app.open_file(dir.join("one.md")));
    assert_eq!(app.docs.len(), 2);
    assert_eq!(app.current(), 1);
    assert_eq!(text(&app), "file one\n");

    assert!(app.open_file(dir.join("two.md")));
    assert_eq!(app.docs.len(), 3);

    // Back to the first: its text, cursor and modified flag are intact.
    app.switch_to(0);
    assert_eq!(text(&app), "draft\n");
    assert!(app.buffer.modified);

    // Opening a file that is already open switches rather than duplicating.
    assert!(app.open_file(dir.join("one.md")));
    assert_eq!(app.docs.len(), 3, "no duplicate");
    assert_eq!(app.current(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn buffer_commands_cycle_close_and_switch_by_name() {
    let dir = two_files();
    let mut app = app_with("draft\n");
    app.open_file(dir.join("one.md"));
    app.open_file(dir.join("two.md"));
    assert_eq!(app.current(), 2);

    cmd(&mut app, ":bn"); // wraps 3 -> 1
    assert_eq!(app.current(), 0);
    cmd(&mut app, ":bp"); // wraps back
    assert_eq!(app.current(), 2);
    cmd(&mut app, ":b one"); // by name
    assert_eq!(text(&app), "file one\n");
    cmd(&mut app, ":b 3"); // by number
    assert_eq!(text(&app), "file two\n");

    cmd(&mut app, ":bd"); // clean buffer closes
    assert_eq!(app.docs.len(), 2);

    // The unsaved first buffer refuses to close without a bang.
    app.switch_to(0);
    cmd(&mut app, ":bd");
    assert_eq!(app.docs.len(), 2, "refused");
    cmd(&mut app, ":bd!");
    assert_eq!(app.docs.len(), 1);

    // The last buffer never closes — an editor with none has nothing to draw.
    cmd(&mut app, ":bd!");
    assert_eq!(app.docs.len(), 1);
    std::fs::remove_dir_all(&dir).ok();
}

/// `:q` has to answer for every open document, not just the visible one.
#[test]
fn quitting_checks_every_buffer_for_unsaved_changes() {
    let dir = two_files();
    let mut app = app_with("draft\n"); // modified
    app.open_file(dir.join("one.md")); // clean, and now current

    cmd(&mut app, ":q");
    assert!(!app.quit, "the OTHER buffer is dirty");
    cmd(&mut app, ":q!");
    assert!(app.quit);
    std::fs::remove_dir_all(&dir).ok();
}

/// …and no quit path may silently discard a modified background buffer.
/// `:q` now unwinds one buffer at a time, so a dirty background buffer
/// simply survives the close — while `:Q`, which leaves in ONE step, still
/// has to answer for it.
#[test]
fn every_quit_path_checks_every_buffer() {
    let dir = two_files();

    // The ACTION path (`<leader>q` when it is bound), with the DIRTY buffer
    // in the background.
    let mut app = app_with("draft\n");
    app.open_file(dir.join("one.md"));
    assert!(!app.buffer.modified, "the visible buffer is clean");
    app.apply_action(Action::Quit { force: false });
    assert!(!app.quit, "the quit action closes a buffer here, it does not leave");
    assert_eq!(app.docs.len(), 1, "the clean buffer is the one that closed");
    assert!(app.buffer.modified, "and the dirty one is what is left open");

    // The last buffer takes the editor with it.
    cmd(&mut app, ":q");
    assert!(!app.quit, "still dirty — :q refuses rather than discarding");
    cmd(&mut app, ":q!");
    assert!(app.quit, "and ! discards it");

    // `:Q` leaves in one step, so it must count what is unsaved.
    let mut app = app_with("a\n");
    app.open_file(dir.join("one.md"));
    feed(&mut app, "x"); // dirty the second one as well
    cmd(&mut app, ":Q");
    assert!(!app.quit, ":Q must not discard unsaved work");
    let msg = app.flash.as_ref().and_then(|f| f.text.clone()).unwrap_or_default();
    assert!(msg.contains("2 buffers"), "got: {msg:?}");

    // And the forced form goes through regardless.
    cmd(&mut app, ":Q!");
    assert!(app.quit);
    std::fs::remove_dir_all(&dir).ok();
}

/// `:Q` with nothing unsaved is just "leave", however many buffers are open.
#[test]
fn q_capital_leaves_at_once_when_everything_is_clean() {
    let dir = two_files();
    let mut app = app_with("a\n");
    app.open_file(dir.join("one.md"));
    app.open_file(dir.join("two.md"));
    assert_eq!(app.docs.len(), 3);
    app.docs.iter_mut().for_each(|d| d.buffer.modified = false);

    cmd(&mut app, ":Q");
    assert!(app.quit, "clean buffers, so :Q leaves without asking");
    std::fs::remove_dir_all(&dir).ok();
}

/// Each document keeps its own cursor, scroll and parse cache, so coming
/// back to one is free and lands where you left it.
#[test]
fn every_buffer_keeps_its_own_cursor_and_caches() {
    let dir = two_files();
    let mut app = app_with("one\ntwo\nthree\nfour\n");
    feed(&mut app, "jj"); // line 2 in buffer 1
    assert_eq!(app.buffer.cursor.line, 2);

    app.open_file(dir.join("one.md"));
    assert_eq!(app.buffer.cursor.line, 0, "a fresh buffer starts at the top");
    app.refresh_blocks();
    app.cache.borrow_mut().sync(&app, 40);

    app.switch_to(0);
    assert_eq!(app.buffer.cursor.line, 2, "back where we left it");
    // The other document's cache is still its own.
    assert_eq!(app.docs[1].cache.borrow().entry(0).map(|e| e.source.clone()),
               Some("file one".to_string()));
    std::fs::remove_dir_all(&dir).ok();
}

/// A temp project with two files, for the finder tests.
fn finder_fixture() -> PathBuf {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("shoin-app-finder-{t}"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("alpha.md"), "alpha file\n").unwrap();
    std::fs::write(d.join("beta.md"), "beta file\n").unwrap();
    d
}

#[test]
fn finder_opens_from_its_binding_and_then_swallows_it() {
    let mut app = app_with_keys("hi\n", &[("<leader>ff", "find_file")]);
    feed(&mut app, " ff");
    assert!(app.finder.is_some(), "<leader>ff opens the finder");

    // Unlike the tree, the binding cannot double as the close key: an open
    // finder is a text field, so those keystrokes are the QUERY.
    feed(&mut app, " ff");
    assert!(app.finder.is_some());
    assert_eq!(app.finder.as_ref().unwrap().query, " ff");
    esc(&mut app);
    assert!(app.finder.is_none(), "Esc is what closes it");
}

#[test]
fn finder_typing_narrows_then_enter_opens_the_file() {
    let dir = finder_fixture();
    let mut app = app_with("hi\n");
    app.finder = Some(Finder::open(dir.clone()));

    // Keys go to the finder, not the buffer: `dd` would delete a line.
    feed(&mut app, "dd");
    assert_eq!(text(&app), "hi\n", "the overlay owns input while it is open");
    assert!(app.finder.as_ref().unwrap().matches.is_empty());

    app.finder.as_mut().unwrap().clear_query();
    feed(&mut app, "beta");
    assert_eq!(app.finder.as_ref().unwrap().matches.len(), 1);

    app.buffer.modified = false; // `app_with` types the text in; opening refuses if dirty
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
    assert!(app.finder.is_none(), "opening closes the finder");
    assert_eq!(text(&app), "beta file\n");
    assert_eq!(app.buffer.path.as_deref(), Some(dir.join("beta.md").as_path()));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn finder_esc_closes_without_touching_the_buffer() {
    let dir = finder_fixture();
    let mut app = app_with("hi\n");
    app.finder = Some(Finder::open(dir.clone()));
    esc(&mut app);
    assert!(app.finder.is_none());
    assert_eq!(text(&app), "hi\n");
    assert_eq!(app.mode, Mode::Normal, "and leaves the editor as it was");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn help_opens_scrolls_and_closes() {
    let mut app = app_with("hi\n");
    cmd(&mut app, ":help bindings");
    assert!(app.help.is_some());
    let h = app.help.as_ref().unwrap();
    assert_eq!(h.cats[h.cat].name, "Navigation");
    feed(&mut app, "jjj"); // scroll down
    assert_eq!(app.help.as_ref().unwrap().scroll, 3);
    feed(&mut app, "q"); // close
    assert!(app.help.is_none());
    // Normal editing resumes — the buffer wasn't touched by any of this.
    assert_eq!(text(&app), "hi\n");
}

#[test]
fn one_bad_binding_does_not_kill_the_rest() {
    // A bogus action name must be skipped, not wipe out every other binding
    // (the empty-keymap bug that broke all leader keys).
    let mut app = app_with_keys(
        "hello\n",
        &[("<leader>f", "cycle_focus"), ("q", "no_such_action")],
    );
    feed(&mut app, " f"); // <leader>f still works
    assert_eq!(app.focus, FocusMode::Paragraph);
}

#[test]
fn single_key_override_shadows_builtin() {
    let mut app = app_with_keys("hello\n", &[("z", "toggle_conceal")]);
    assert!(app.config.layout.conceal);
    feed(&mut app, "z");
    assert!(!app.config.layout.conceal); // z was remapped, not a motion
}

#[test]
fn unbound_keys_still_use_builtin_grammar() {
    // A leader binding exists, but plain built-in keys must keep working.
    let mut app = app_with_keys("hello\n", &[("<leader>w", "save")]);
    feed(&mut app, "x"); // built-in delete-char
    assert_eq!(text(&app), "ello\n");
    feed(&mut app, "dd"); // built-in operator still composes
    assert_eq!(text(&app), ""); // the only line, newline included, is gone
}

/// Undo must return the cursor to where the COMMAND began. For a Visual
/// operator that is not `Change::cursor_before`: the selection's motions
/// have already walked the cursor to the far end by the time the rope is
/// touched, so the naive answer leaves you at the end of the restored text.
#[test]
fn undo_of_a_visual_delete_returns_to_the_start_of_the_command() {
    let mut app = app_with("hello world\n");
    feed(&mut app, "vllll");
    feed(&mut app, "d");
    assert_eq!(text(&app), " world\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), "hello world\n");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 0));
}

/// The linewise case, where the miss was a whole line rather than a column.
#[test]
fn undo_of_a_visual_line_delete_returns_to_the_starting_line() {
    let mut app = app_with("aaa\nbbb\nccc\nddd\n");
    feed(&mut app, "jV");
    feed(&mut app, "jd");
    assert_eq!(text(&app), "aaa\nddd\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), "aaa\nbbb\nccc\nddd\n");
    // Position only: the cursor `j` left there also keeps its goal cell.
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (1, 0));
}

/// An operator-pending motion is a continuation too — `dw`'s `w` must not
/// re-anchor the step.
#[test]
fn undo_of_an_operator_motion_returns_to_the_start_of_the_command() {
    let mut app = app_with("hello world foo\n");
    feed(&mut app, "ll");
    feed(&mut app, "dw");
    feed(&mut app, "u");
    assert_eq!(text(&app), "hello world foo\n");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 2));
}

/// A sub-step split out of an insert session mid-flight has no anchor of its
/// own and must fall back to `cursor_before` — undoing the last typed word
/// leaves the cursor where that word started, not back at the `A`.
#[test]
fn undo_within_an_insert_session_still_falls_back_to_the_change() {
    let mut app = app_with("start\n");
    feed(&mut app, "A");
    feed(&mut app, " one two");
    esc(&mut app);
    feed(&mut app, "u");
    assert_eq!(text(&app), "start one\n");
    assert_eq!(app.buffer.cursor.line, 0);
    // The whole session unwinds to the pre-command cursor.
    feed(&mut app, "u");
    assert_eq!(text(&app), "start\n");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 0));
}

// --- :undo / :redo ---

/// Type a `:` command and run it, the way a user does — through Command
/// mode and Enter, not by calling the dispatcher.
fn ex(app: &mut App, cmd: &str) {
    feed(app, ":");
    feed(app, cmd);
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.sync_after_input();
}

/// `:u` takes back the last change, exactly as `u` does.
#[test]
fn ex_undo_takes_back_the_last_change() {
    let mut app = app_with("start\n");
    feed(&mut app, "x");
    assert_eq!(text(&app), "tart\n");
    ex(&mut app, "u");
    assert_eq!(text(&app), "start\n");
    assert_eq!(app.mode, Mode::Normal, "the : line is done with");
}

/// Vim's abbreviations all land on it, so muscle memory works.
#[test]
fn every_undo_abbreviation_runs_it() {
    for spelling in ["u", "un", "und", "undo"] {
        let mut app = app_with("start\n");
        feed(&mut app, "x");
        ex(&mut app, spelling);
        assert_eq!(text(&app), "start\n", ":{spelling} should undo");
    }
}

/// `:redo` is the way back, and `:red` abbreviates it.
#[test]
fn ex_redo_puts_the_change_back() {
    for spelling in ["red", "redo"] {
        let mut app = app_with("start\n");
        feed(&mut app, "x");
        ex(&mut app, "u");
        assert_eq!(text(&app), "start\n");
        ex(&mut app, spelling);
        assert_eq!(text(&app), "tart\n", ":{spelling} should redo");
    }
}

/// Nothing left to take back flashes the same hint `u` does, rather than
/// the dispatcher's "not a command".
#[test]
fn ex_undo_at_the_oldest_change_says_so() {
    let mut app = app_with("start\n");
    // `app_with` seeds its text through the buffer, so that seed is itself
    // a change: undo past it to reach the bottom of the history.
    for _ in 0..8 {
        ex(&mut app, "u");
    }
    let flash = app.flash.as_ref().and_then(|f| f.text.clone());
    assert_eq!(flash.as_deref(), Some("already at oldest change"));
}

/// `:r` is `:read` in vim, not undo. Claiming it here would train a reflex
/// that misfires in every other editor, so it stays unclaimed — as does
/// the ambiguous `:re`.
#[test]
fn r_is_not_an_undo_abbreviation() {
    for spelling in ["r", "re"] {
        let mut app = app_with("start\n");
        feed(&mut app, "x");
        ex(&mut app, spelling);
        assert_eq!(text(&app), "tart\n", ":{spelling} must not undo");
        let flash = app.flash.as_ref().and_then(|f| f.text.clone());
        assert_eq!(flash, Some(format!("not a command: {spelling}")));
    }
}

// --- mouse selection ---
//
// The geometry these coordinates rely on is the one
// `frame::tests::click_maps_to_the_clicked_line` pins down: in a 60x12
// terminal showing "# One\n\nsecond paragraph here\n", screen row 2 is
// line 0 and row 4 is line 2.

fn mouse_app() -> App {
    let mut app = app_with("# One\n\nsecond paragraph here\n");
    app.term_size = (60, 12);
    app
}

fn mouse(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    app.on_mouse(MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE });
    app.sync_after_input();
}

fn press(app: &mut App, col: u16, row: u16) {
    mouse(app, MouseEventKind::Down(MouseButton::Left), col, row);
}

fn drag(app: &mut App, col: u16, row: u16) {
    mouse(app, MouseEventKind::Drag(MouseButton::Left), col, row);
}

fn release(app: &mut App, col: u16, row: u16) {
    mouse(app, MouseEventKind::Up(MouseButton::Left), col, row);
}

/// Dragging out of a press is a charwise Visual selection anchored where
/// the button went down — and it outlives the release, so an operator
/// still has something to act on.
#[test]
fn a_drag_selects_from_the_press_to_the_pointer() {
    let mut app = mouse_app();
    press(&mut app, 8, 2);
    assert_eq!(app.mode, Mode::Normal, "a press alone selects nothing");
    assert_eq!(app.buffer.cursor.line, 0);

    drag(&mut app, 8, 4);
    assert_eq!(app.mode, Mode::Visual);
    assert_eq!(app.anchor.expect("an anchor").line, 0, "anchored at the press");
    assert_eq!(app.buffer.cursor.line, 2, "the cursor follows the pointer");

    release(&mut app, 8, 4);
    assert_eq!(app.mode, Mode::Visual, "the selection survives the release");
    assert_eq!(app.anchor.expect("an anchor").line, 0);
}

/// The selection a drag leaves behind is a real one: `y` takes it.
#[test]
fn a_dragged_selection_can_be_yanked() {
    let mut app = mouse_app();
    press(&mut app, 8, 4);
    drag(&mut app, 12, 4);
    release(&mut app, 12, 4);
    feed(&mut app, "y");
    assert_eq!(app.mode, Mode::Normal);
    // Inclusive of the cell under the pointer, as a Visual selection is.
    let yanked = app.registers.get(&'"').expect("the unnamed register").text.clone();
    assert_eq!(yanked, " para", "the five cells the drag crossed");
}

/// A press drops the selection instead of extending it to where you
/// clicked — the click puts the cursor down, and the next drag starts
/// from there.
#[test]
fn a_press_drops_the_previous_selection() {
    let mut app = mouse_app();
    press(&mut app, 8, 2);
    drag(&mut app, 8, 4);
    release(&mut app, 8, 4);
    assert_eq!(app.mode, Mode::Visual);

    press(&mut app, 8, 2);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.anchor.is_none());
}

/// A Drag event on the cell that was pressed is a shaky hand, not a
/// selection: every click would otherwise leave Visual mode up.
#[test]
fn a_drag_that_never_leaves_the_cell_is_not_a_selection() {
    let mut app = mouse_app();
    press(&mut app, 8, 2);
    drag(&mut app, 8, 2);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.anchor.is_none());
}

/// Text panes only: a press in the file tree arms no drag, so dragging out
/// of the sidebar and across the document selects nothing.
#[test]
fn a_drag_out_of_the_file_tree_selects_nothing() {
    let mut app = mouse_app();
    app.tree = Some(FileTree::open(std::env::current_dir().unwrap()));
    press(&mut app, 2, 3);
    drag(&mut app, 40, 4);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.anchor.is_none());
}

/// Dragging out of an insert session selects, and remembers to hand the
/// session back afterwards — vim's `-- (insert) VISUAL --`.
#[test]
fn a_drag_while_inserting_enters_insert_visual() {
    let mut app = mouse_app();
    feed(&mut app, "i");
    press(&mut app, 8, 2);
    drag(&mut app, 8, 4);
    assert_eq!(app.mode, Mode::Visual);
    assert!(app.visual_resumes_insert, "it will go back to Insert");
    assert_eq!(app.anchor.expect("an anchor").line, 0);
}

/// The mode is named once, on the way in. After that the caret carries it.
#[test]
fn entering_insert_visual_names_itself_once() {
    let mut app = mouse_app();
    feed(&mut app, "i");
    press(&mut app, 8, 2);
    drag(&mut app, 8, 4);
    let flash = app.flash.as_ref().and_then(|f| f.text.clone());
    assert_eq!(flash.as_deref(), Some("-- (insert) visual --"));
}

/// An operator takes the selection and hands the insert session back, so
/// typing carries on where it left off.
#[test]
fn an_operator_returns_to_the_insert_session() {
    let mut app = mouse_app();
    feed(&mut app, "i");
    press(&mut app, 8, 4);
    drag(&mut app, 12, 4);
    feed(&mut app, "y");
    assert_eq!(app.mode, Mode::Insert, "back to inserting");
    assert!(!app.visual_resumes_insert, "and the flag is spent");
    assert!(app.anchor.is_none());
    let yanked = app.registers.get(&'"').expect("the unnamed register").text.clone();
    assert_eq!(yanked, " para");
}

/// Esc does the same — it ends the selection, not the typing. A second Esc
/// is what leaves Insert, exactly as it would have without the detour.
#[test]
fn esc_returns_to_the_insert_session_then_leaves_it() {
    let mut app = mouse_app();
    feed(&mut app, "i");
    press(&mut app, 8, 2);
    drag(&mut app, 8, 4);
    esc(&mut app);
    assert_eq!(app.mode, Mode::Insert);
    esc(&mut app);
    assert_eq!(app.mode, Mode::Normal);
}

/// A drag begun in Normal is unaffected: it still ends in Normal.
#[test]
fn a_drag_from_normal_still_ends_in_normal() {
    let mut app = mouse_app();
    press(&mut app, 8, 2);
    drag(&mut app, 8, 4);
    assert!(!app.visual_resumes_insert);
    feed(&mut app, "y");
    assert_eq!(app.mode, Mode::Normal);
}

/// The resume must not outlive the selection that earned it. A keyboard
/// `v` after an insert-visual drag is an ordinary selection.
#[test]
fn a_later_keyboard_visual_does_not_inherit_the_resume() {
    let mut app = mouse_app();
    feed(&mut app, "i");
    press(&mut app, 8, 2);
    drag(&mut app, 8, 4);
    feed(&mut app, "y");
    esc(&mut app);
    assert_eq!(app.mode, Mode::Normal);

    feed(&mut app, "v");
    assert!(!app.visual_resumes_insert, "a keyboard v never resumes");
    feed(&mut app, "y");
    assert_eq!(app.mode, Mode::Normal, "and lands in Normal");
}

/// The undo shape through the detour, which is the subtle part. The
/// operator and the typing that resumes after it collapse into ONE step —
/// the same shape `cw` already has — and the insert before the drag stays
/// its own. Nothing is left half-sealed.
#[test]
fn insert_visual_undoes_in_cw_shaped_steps() {
    let mut app = mouse_app();
    feed(&mut app, "i");
    feed(&mut app, "XY");
    press(&mut app, 8, 4);
    drag(&mut app, 12, 4);
    feed(&mut app, "d");
    assert_eq!(app.mode, Mode::Insert);
    assert_eq!(text(&app), "XY# One\n\nsecondgraph here\n", "the selection is gone");
    feed(&mut app, "ZZ");
    esc(&mut app);
    assert_eq!(text(&app), "XY# One\n\nsecondZZgraph here\n");

    // One step takes back the delete AND the typing that resumed after it.
    feed(&mut app, "u");
    assert_eq!(text(&app), "XY# One\n\nsecond paragraph here\n");
    // The insert that ran BEFORE the drag is its own step.
    feed(&mut app, "u");
    assert_eq!(text(&app), "# One\n\nsecond paragraph here\n");
}

/// The writer verbs exit Visual through the same door, so they hand the
/// session back too.
#[test]
fn a_writer_verb_returns_to_the_insert_session() {
    let mut app = mouse_app();
    feed(&mut app, "i");
    press(&mut app, 8, 4);
    drag(&mut app, 12, 4);
    feed(&mut app, "gb");
    assert_eq!(app.mode, Mode::Insert, "bold, and still inserting");
    assert!(text(&app).contains("**"), "it wrapped the selection: {:?}", text(&app));
}

/// A key that changes mode between the press and the drag wins: `:` owns
/// the input, and a drag must not yank the user back out of it.
#[test]
fn a_mode_change_since_the_press_calls_the_drag_off() {
    let mut app = mouse_app();
    press(&mut app, 8, 2);
    feed(&mut app, ":");
    drag(&mut app, 8, 4);
    assert!(matches!(app.mode, Mode::Command(_)), "still on the : line");
    assert!(app.anchor.is_none());
}

/// An overlay owns the screen, so the text a drag would cross is not the
/// text being shown.
#[test]
fn a_drag_under_the_help_overlay_selects_nothing() {
    let mut app = mouse_app();
    app.open_help("");
    press(&mut app, 8, 2);
    drag(&mut app, 8, 4);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.anchor.is_none());
}

/// Capture off is the terminal's own selection, so shoin must not be
/// reading drags at all.
#[test]
fn mouse_off_ignores_a_drag() {
    let mut app = mouse_app();
    app.config.input.mouse = false;
    press(&mut app, 8, 2);
    drag(&mut app, 8, 4);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.buffer.cursor, Cursor::new(0, 0), "the cursor never moved");
}

// ------------------------------------------------------------- clipboard

/// An app whose system clipboard is `mem`, kept by the caller to inspect.
fn clip_app(text: &str, mem: &crate::clipboard::Memory) -> App {
    let mut app = app_with(text);
    app.clipboard = Box::new(mem.clone());
    app
}

#[test]
fn plus_register_yanks_to_the_clipboard() {
    let mem = crate::clipboard::Memory::default();
    let mut app = clip_app("alpha\nbravo\n", &mem);
    feed(&mut app, "j\"+yy");
    assert_eq!(mem.text().as_deref(), Some("bravo\n"));
    feed(&mut app, "\"*yiw");
    assert_eq!(mem.text().as_deref(), Some("bravo"), "\"* is the same clipboard");
}

#[test]
fn plus_register_pastes_what_another_program_copied() {
    let mem = crate::clipboard::Memory::holding("XY");
    let mut app = clip_app("ab\n", &mem);
    feed(&mut app, "\"+p");
    assert_eq!(text(&app), "aXYb\n", "charwise, after the cursor");
}

#[test]
fn clipboard_text_ending_in_a_newline_pastes_as_lines() {
    let mem = crate::clipboard::Memory::holding("new\n");
    let mut app = clip_app("one\ntwo\n", &mem);
    feed(&mut app, "\"+p");
    assert_eq!(text(&app), "one\nnew\ntwo\n");
}

/// Shoin's own copy keeps its shape: a charwise copy that happens to end
/// in a newline is still charwise when it comes back off the clipboard.
#[test]
fn our_own_copy_remembers_it_was_charwise() {
    let mem = crate::clipboard::Memory::default();
    let mut app = clip_app("ab\n", &mem);
    app.pending_register = Some('+');
    app.store_register("x\n".into(), false, false);
    assert_eq!(mem.text().as_deref(), Some("x\n"));
    feed(&mut app, "\"+p");
    assert_eq!(text(&app), "ax\nb\n", "inside the line, not a line below");
    // The same text from another program is taken for a line.
    let mut other = clip_app("ab\n", &crate::clipboard::Memory::holding("x\n"));
    feed(&mut other, "\"+p");
    assert_eq!(text(&other), "ab\nx\n");
}

/// No program to read from (or over SSH): `"+p` uses what was copied.
#[test]
fn an_unreadable_clipboard_falls_back_to_our_copy() {
    struct WriteOnly;
    impl Clipboard for WriteOnly {
        fn copy(&mut self, _: &str) {}
        fn paste(&mut self) -> Option<String> {
            None
        }
    }
    let mut app = app_with("ab\n");
    app.clipboard = Box::new(WriteOnly);
    feed(&mut app, "\"+yl\"+p");
    assert_eq!(text(&app), "aab\n");
}

/// A clipboard emptied (or holding an image) since our copy pastes
/// nothing — not the text it no longer holds.
#[test]
fn an_empty_clipboard_pastes_nothing() {
    let mem = crate::clipboard::Memory::default();
    let mut app = clip_app("ab\n", &mem);
    feed(&mut app, "\"+yl");
    mem.clone().copy("");
    feed(&mut app, "\"+p");
    assert_eq!(text(&app), "ab\n");
}

/// Charwise text of several lines leaves the cursor at its start, as vim.
#[test]
fn a_multi_line_charwise_paste_leaves_the_cursor_at_its_start() {
    let mem = crate::clipboard::Memory::holding("foo\nbar");
    let mut app = clip_app("ab\n", &mem);
    feed(&mut app, "\"+p");
    assert_eq!(text(&app), "afoo\nbarb\n");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 1));
}

#[test]
fn plain_yank_leaves_the_clipboard_alone_by_default() {
    let mem = crate::clipboard::Memory::holding("theirs");
    let mut app = clip_app("ours\n", &mem);
    feed(&mut app, "yyp");
    assert_eq!(mem.text().as_deref(), Some("theirs"));
    assert_eq!(text(&app), "ours\nours\n");
}

#[test]
fn clipboard_option_routes_unnamed_yank_and_paste() {
    let mem = crate::clipboard::Memory::default();
    let mut app = clip_app("ours\n", &mem);
    app.set_option("clipboard");
    assert!(app.config.editor.clipboard);
    feed(&mut app, "yy");
    assert_eq!(mem.text().as_deref(), Some("ours\n"));
    mem.clone().copy("theirs");
    feed(&mut app, "P");
    assert_eq!(text(&app), "theirsours\n", "plain P read the clipboard");
}

/// `"ayy` is how to keep something out of the clipboard with it on.
#[test]
fn a_named_register_never_reaches_the_clipboard() {
    let mem = crate::clipboard::Memory::holding("theirs");
    let mut app = clip_app("ours\n", &mem);
    app.config.editor.clipboard = true;
    feed(&mut app, "\"ayy");
    assert_eq!(mem.text().as_deref(), Some("theirs"));
}

// ----------------------------------------------------------------- paste

#[test]
fn a_paste_while_inserting_goes_in_verbatim() {
    let mut app = app_with("- one\n");
    feed(&mut app, "A");
    // Typed, the newline would continue the list and indent, and `jk`
    // would leave Insert.
    app.on_paste("\r\n  - two jk\n- three".into());
    assert_eq!(text(&app), "- one\n  - two jk\n- three\n");
    assert_eq!(app.mode, Mode::Insert);
    assert_eq!(app.buffer.cursor, Cursor::new(2, 7), "after the paste");
}

#[test]
fn a_paste_in_normal_mode_is_text_not_commands() {
    let mut app = app_with("ab\n");
    app.on_paste("dd".into());
    assert_eq!(text(&app), "ddab\n");
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.buffer.cursor, Cursor::new(0, 1), "on its last character");
    feed(&mut app, "u");
    assert_eq!(text(&app), "ab\n", "one undo step");
}

/// A Normal-mode paste ending in a line break leaves the cursor on the
/// pasted text, not on the line it pushed down.
#[test]
fn a_normal_mode_paste_ending_in_a_newline_stays_on_the_paste() {
    let mut app = app_with("ab\n");
    app.on_paste("foo\n".into());
    assert_eq!(text(&app), "foo\nab\n");
    assert_eq!(app.buffer.cursor, Cursor::new(0, 2));
    let mut app = app_with("ab\n");
    app.on_paste("x\nyz".into());
    assert_eq!(app.buffer.cursor, Cursor::new(1, 1));
}

#[test]
fn a_paste_drops_a_half_typed_command() {
    let mut app = app_with("ab\ncd\n");
    feed(&mut app, "d");
    app.on_paste("X".into());
    feed(&mut app, "j");
    assert_eq!(text(&app), "Xab\ncd\n", "j moved; it did not complete dj");
    assert_eq!(app.buffer.cursor.line, 1);
}

#[test]
fn dot_repeats_a_paste() {
    let mut app = app_with("ab\n");
    app.on_paste("X".into());
    feed(&mut app, "ll.");
    assert_eq!(text(&app), "XaXb\n");
}

#[test]
fn dot_repeats_an_insert_session_with_a_paste_in_it() {
    let mut app = app_with("\n\n");
    feed(&mut app, "i(");
    app.on_paste("jk".into());
    feed(&mut app, ")");
    esc(&mut app);
    feed(&mut app, "j.");
    assert_eq!(text(&app), "(jk)\n(jk)\n");
}

#[test]
fn a_paste_on_the_command_line_takes_its_first_line() {
    let mut app = app_with("ab\n");
    feed(&mut app, ":");
    app.on_paste("set mouse\nq!".into());
    assert_eq!(app.mode, Mode::Command("set mouse".into()));
    assert_eq!(text(&app), "ab\n");
}

#[test]
fn a_recovery_notice_waits_while_a_message_is_up() {
    let mut app = app_with("text\n");
    app.flash = None;
    assert!(app.can_show_notice());
    // The default status, shown briefly, holds no message to replace.
    app.touch_status();
    assert!(app.can_show_notice());
    app.notify("unsaved text from a crash: a.md.shoin-recover", FlashKind::Error);
    assert!(!app.can_show_notice(), "a second notice would replace the first a pass later");
    app.flash.as_mut().unwrap().expires_at = Instant::now();
    assert!(app.expire_flash());
    assert!(app.can_show_notice(), "its turn once the first has timed out");
}

// ------------------------------------------- autosave: created after opening

/// A scratch folder with nothing in it, for files that do not exist yet.
fn empty_dir(tag: &str) -> std::path::PathBuf {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!("shoin-created-{tag}-{t}"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// An app on `path`, which does not exist, holding the unsaved text "mine".
fn app_on_missing(path: &std::path::Path) -> App {
    let mut app = app_with("");
    app.open_file(path.to_path_buf());
    assert!(app.buffer.was_missing);
    feed(&mut app, "imine");
    esc(&mut app);
    assert!(app.buffer.modified);
    app
}

/// A file someone else made after the buffer opened it missing: autosave
/// names the refusal once, then stays quiet every interval after, while
/// `:w` still says it each time. The file is never touched.
#[test]
fn autosave_names_a_file_created_after_opening_it_once() {
    let d = empty_dir("once");
    let path = d.join("x.md");
    let mut app = app_on_missing(&path);
    std::fs::write(&path, "theirs\n").unwrap();

    autosave_due(&mut app);
    assert!(app.autosave_tick(), "the first refusal is said");
    let msg = flash_text(&app);
    assert_eq!(msg, "autosave: x.md was created after you opened it — :w! to overwrite");

    for _ in 0..3 {
        app.flash = None;
        autosave_due(&mut app);
        assert!(!app.autosave_tick(), "not again on the next interval");
        assert_eq!(flash_text(&app), "");
        assert!(app.autosave_at.is_some(), "still armed");
    }

    // `:w` is the reader asking: it answers every time.
    for _ in 0..2 {
        app.flash = None;
        cmd(&mut app, ":w");
        assert!(flash_text(&app).contains("x.md was created after you opened it"), "got: {}", flash_text(&app));
    }
    app.flash = None;
    autosave_due(&mut app);
    assert!(!app.autosave_tick(), "and :w does not make autosave say it again");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs\n");
    assert!(app.buffer.modified);

    // `:w!` settles it: the file is ours, and autosave writes as usual.
    cmd(&mut app, ":w!");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");
    feed(&mut app, "A more");
    esc(&mut app);
    app.flash = None;
    autosave_due(&mut app);
    assert!(app.autosave_tick());
    assert_eq!(flash_text(&app), "autosaved x.md");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine more\n");
    assert!(!app.buffer.created_reported);
    std::fs::remove_dir_all(&d).ok();
}

/// The file goes away again: the next autosave writes it, as nothing is
/// in the way any more.
#[test]
fn autosave_writes_once_the_created_file_is_gone_again() {
    let d = empty_dir("gone");
    let path = d.join("x.md");
    let mut app = app_on_missing(&path);
    std::fs::write(&path, "theirs\n").unwrap();
    autosave_due(&mut app);
    assert!(app.autosave_tick());
    assert!(app.buffer.created_reported);

    std::fs::remove_file(&path).unwrap();
    app.flash = None;
    autosave_due(&mut app);
    assert!(app.autosave_tick());
    assert_eq!(flash_text(&app), "autosaved x.md");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");
    assert!(!app.buffer.created_reported);
    assert!(!app.buffer.was_missing);
    std::fs::remove_dir_all(&d).ok();
}

/// Gone while the buffer had nothing to save, then made again: that is a
/// new file, and autosave names it again once.
#[test]
fn autosave_names_a_file_made_again_after_it_went_away() {
    let d = empty_dir("again");
    let path = d.join("x.md");
    let mut app = app_on_missing(&path);
    std::fs::write(&path, "theirs\n").unwrap();
    autosave_due(&mut app);
    assert!(app.autosave_tick());

    // Undone to unmodified, so the tick with the file gone writes nothing.
    feed(&mut app, "u");
    assert!(!app.buffer.modified);
    std::fs::remove_file(&path).unwrap();
    app.flash = None;
    autosave_due(&mut app);
    assert!(!app.autosave_tick(), "nothing to save");
    assert!(!app.buffer.created_reported, "but the situation changed");

    feed(&mut app, "iagain");
    esc(&mut app);
    std::fs::write(&path, "theirs, again\n").unwrap();
    autosave_due(&mut app);
    assert!(app.autosave_tick(), "named again");
    assert!(flash_text(&app).contains("x.md was created after you opened it"), "got: {}", flash_text(&app));
    app.flash = None;
    autosave_due(&mut app);
    assert!(!app.autosave_tick(), "and once only");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs, again\n");
    std::fs::remove_dir_all(&d).ok();
}

/// Two such buffers: the line names the first. The second has not been
/// said, so the next interval names it, and after that both are quiet.
#[test]
fn autosave_names_each_created_file_once() {
    let d = empty_dir("two");
    let (a, b) = (d.join("a.md"), d.join("b.md"));
    let mut app = app_on_missing(&a);
    app.open_file(b.clone());
    feed(&mut app, "imine");
    esc(&mut app);
    std::fs::write(&a, "theirs\n").unwrap();
    std::fs::write(&b, "theirs\n").unwrap();

    let mut said = Vec::new();
    for _ in 0..2 {
        app.flash = None;
        autosave_due(&mut app);
        assert!(app.autosave_tick());
        said.push(flash_text(&app));
    }
    said.sort();
    assert!(said[0].contains("a.md was created after you opened it"), "got: {said:?}");
    assert!(said[1].contains("b.md was created after you opened it"), "got: {said:?}");
    app.flash = None;
    autosave_due(&mut app);
    assert!(!app.autosave_tick(), "both said");
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "theirs\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "theirs\n");
    std::fs::remove_dir_all(&d).ok();
}

/// A reload (`:revert!`) takes the file and clears it; an edit after that is
/// written by autosave like any other.
#[test]
fn autosave_writes_after_a_reload_of_the_created_file() {
    let d = empty_dir("reload");
    let path = d.join("x.md");
    let mut app = app_on_missing(&path);
    std::fs::write(&path, "theirs\n").unwrap();
    autosave_due(&mut app);
    assert!(app.autosave_tick());

    cmd(&mut app, ":revert!");
    assert_eq!(text(&app), "theirs\n");
    assert!(!app.buffer.was_missing);
    feed(&mut app, "A too");
    esc(&mut app);
    app.flash = None;
    autosave_due(&mut app);
    assert!(app.autosave_tick());
    assert_eq!(flash_text(&app), "autosaved x.md");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "theirs too\n");
    assert!(!app.buffer.created_reported);
    std::fs::remove_dir_all(&d).ok();
}

// --- `.` across documents and panes ---
//
// A window or buffer key is never "the change": `.` repeats the last real
// edit, in whatever document is current when it is pressed.

/// The keys `.` would replay, as text: `"dw"`, `"vld"`.
fn dot_keys(app: &App) -> String {
    app.dot
        .iter()
        .map(|r| match r {
            Replay::Key(k) => match k.code {
                KeyCode::Char(c) if k.modifiers.contains(KeyModifiers::CONTROL) => format!("<C-{c}>"),
                KeyCode::Char(c) => c.to_string(),
                other => format!("<{other:?}>"),
            },
            Replay::Paste(t) => format!("<paste {t:?}>"),
            Replay::Select { lines, cols, backwards } => {
                format!("<select {lines} {cols}{}>", if *backwards { " back" } else { "" })
            }
            Replay::Insert => "<insert>".to_string(),
        })
        .collect()
}

/// `dw` in one document, `:bn` to the next, `.`: the word goes in the
/// second document too, and nobody switches buffers again.
#[test]
fn dot_after_bn_repeats_the_change_in_the_other_document() {
    let dir = two_files();
    let mut app = app_with("scratch\n");
    app.open_file(dir.join("one.md"));
    app.open_file(dir.join("two.md"));
    ex(&mut app, "b one");
    assert_eq!(shown(&app), "one.md");
    feed(&mut app, "dw");
    assert_eq!(text(&app), "one\n");
    ex(&mut app, "bn");
    assert_eq!(shown(&app), "two.md");
    assert_eq!(dot_keys(&app), "dw", "the switch is not the change");
    feed(&mut app, ".");
    assert_eq!(shown(&app), "two.md", "`.` does not switch again");
    assert_eq!(text(&app), "two\n");

    // `:bp` and `:e` the same way.
    ex(&mut app, "bp");
    ex(&mut app, "e two.md");
    assert_eq!(dot_keys(&app), "dw");
    std::fs::remove_dir_all(&dir).ok();
}

/// `gf` into a note, then `.`: the change made before the jump.
#[test]
fn dot_after_gf_repeats_the_change_in_the_linked_note() {
    let (d, mut app) = vault("word [[target]]\n", &[("target.md", "alpha beta\n")]);
    feed(&mut app, "dw");
    assert_eq!(text(&app), "[[target]]\n");
    cursor_on(&mut app, 0, "target");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "target.md");
    assert_eq!(dot_keys(&app), "dw");
    feed(&mut app, ".");
    assert_eq!(shown(&app), "target.md");
    assert_eq!(text(&app), "beta\n");
    std::fs::remove_dir_all(&d).ok();
}

/// `<C-^>` back to the note, then `.`: the change made in the note left.
#[test]
fn dot_after_ctrl_caret_repeats_the_change_in_the_alternate() {
    let (d, mut app) = vault("one [[target]]\n", &[("target.md", "alpha beta\n")]);
    cursor_on(&mut app, 0, "target");
    feed(&mut app, "gf");
    assert_eq!(shown(&app), "target.md");
    feed(&mut app, "ciwgamma");
    esc(&mut app);
    assert_eq!(text(&app), "gamma beta\n");
    ctrl(&mut app, '^');
    assert_eq!(shown(&app), "note.md");
    feed(&mut app, "0.");
    assert_eq!(shown(&app), "note.md", "`.` does not go back again");
    assert_eq!(text(&app), "gamma [[target]]\n");
    std::fs::remove_dir_all(&d).ok();
}

/// `<C-w>o` inside a selection: it ends the selection, as in vim, and
/// neither its keys nor the selection's become the change `.` repeats.
#[test]
fn ctrl_w_o_in_visual_stays_out_of_dot() {
    let mut app = app_with("abcdef\n");
    app.split_pane(true);
    app.split_pane(true);
    feed(&mut app, "x");
    feed(&mut app, "vl");
    ctrl(&mut app, 'w');
    feed(&mut app, "o");
    assert_eq!(app.layout.ids().len(), 1);
    assert_eq!(app.mode, Mode::Normal, "a window key ends the selection");
    assert!(app.recording.is_empty(), "nothing left in flight");
    feed(&mut app, ".");
    assert_eq!(text(&app), "bdef\n", "`.` is still the `x`");
    assert_eq!(dot_keys(&app), "x");
}

/// A `<C-w>q` refused on the last pane does the same.
#[test]
fn a_refused_ctrl_w_q_in_visual_stays_out_of_dot() {
    let mut app = app_with("abcdef\n");
    feed(&mut app, "x");
    feed(&mut app, "v");
    ctrl(&mut app, 'w');
    feed(&mut app, "q");
    assert_eq!(flash_text(&app), "last pane — :q to quit");
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.recording.is_empty());
    feed(&mut app, "ld");
    // `d` waits for a motion now, it has no selection to take.
    esc(&mut app);
    feed(&mut app, ".");
    assert_eq!(dot_keys(&app), "x");
    assert_eq!(text(&app), "bdef\n");
}

/// `3.` replays the change with the new count, as vim does, and is not a
/// recording of its own: the next `.` repeats the counted change.
#[test]
fn a_counted_dot_is_not_recorded() {
    let mut app = app_with("abcdefghij\n");
    feed(&mut app, "x3.");
    assert_eq!(text(&app), "efghij\n");
    assert!(app.recording.is_empty(), "the `3.` keys are not kept");
    assert_eq!(dot_keys(&app), "3x");
    feed(&mut app, ".");
    assert_eq!(text(&app), "hij\n");
}

/// `.` in Visual does nothing, as in vim, and does not end up in the
/// change the selection makes.
#[test]
fn a_dot_in_visual_is_not_recorded() {
    let mut app = app_with("abcdef\n");
    feed(&mut app, "x");
    feed(&mut app, "vl.");
    assert_eq!(text(&app), "bcdef\n", "`.` in Visual is refused");
    assert_eq!(app.mode, Mode::Visual);
    feed(&mut app, "2.d");
    assert_eq!(text(&app), "def\n");
    assert_eq!(dot_keys(&app), "vld");
    feed(&mut app, ".");
    assert_eq!(text(&app), "f\n");
}

/// A selection made with the mouse, then `d`: `.` deletes as many
/// characters again from the cursor, as vim repeats a Visual operator.
#[test]
fn dot_repeats_an_operator_on_a_dragged_selection() {
    let mut app = mouse_app();
    feed(&mut app, "l"); // a command before the drag, which `.` must not replay
    press(&mut app, 8, 4);
    drag(&mut app, 12, 4);
    release(&mut app, 12, 4);
    feed(&mut app, "d");
    assert_eq!(app.buffer.line_text(2), "secondgraph here");
    assert_eq!(dot_keys(&app), "<select 0 4>d");
    feed(&mut app, ".");
    assert_eq!(app.buffer.line_text(2), "second here");
    assert_eq!(app.mode, Mode::Normal);
}

/// Dragged leftwards, the cursor ends at the start of the selection, and
/// the repeat keeps it there for a key that moves it after the drag.
#[test]
fn dot_repeats_a_leftward_drag_with_its_cursor_at_the_start() {
    let mut app = mouse_app();
    press(&mut app, 12, 4);
    drag(&mut app, 8, 4);
    release(&mut app, 8, 4);
    feed(&mut app, "hd"); // the selection grows by one on the left
    assert_eq!(app.buffer.line_text(2), "secongraph here");
    assert_eq!(dot_keys(&app), "<select 0 4 back>hd");
    // From `g`: the repeat takes "ngraph", one to the left and four on.
    feed(&mut app, ".");
    assert_eq!(app.buffer.line_text(2), "seco here");
}

/// A drag over lines repeats over as many lines, ending in the same
/// column, as vim's Visual repeat does.
#[test]
fn dot_repeats_a_multi_line_drag() {
    let mut app = app_with("abcdef\nghijkl\nmnopqr\nstuvwx\n");
    app.term_size = (60, 12);
    press(&mut app, 4, 2);
    drag(&mut app, 4, 3);
    release(&mut app, 4, 3);
    assert_eq!(app.anchor, Some(Cursor::new(0, 2)));
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (1, 2));
    feed(&mut app, "d");
    assert_eq!(text(&app), "abjkl\nmnopqr\nstuvwx\n");
    assert_eq!(dot_keys(&app), "<select 1 2>d");
    feed(&mut app, "j0.");
    assert_eq!(text(&app), "abjkl\nvwx\n");
}

/// A split, a refused `:bn` and a closed buffer are not the change either.
#[test]
fn splits_and_closes_are_not_the_change() {
    let mut app = app_with("abcdef\n");
    feed(&mut app, "x");
    ctrl(&mut app, 'w');
    feed(&mut app, "v");
    ex(&mut app, "bn");
    assert_eq!(flash_text(&app), "only one buffer");
    feed(&mut app, ".");
    assert_eq!(app.layout.ids().len(), 2);
    assert_eq!(text(&app), "cdef\n");

    let dir = two_files();
    let mut app = app_with("scratch\n");
    app.open_file(dir.join("one.md"));
    app.open_file(dir.join("two.md"));
    feed(&mut app, "x");
    ex(&mut app, "bd!");
    assert_eq!(shown(&app), "one.md");
    feed(&mut app, ".");
    assert_eq!(text(&app), "ile one\n");
    std::fs::remove_dir_all(&dir).ok();
}

/// The repeat of a dragged selection does not go through the keymap: with
/// `l` and `o` rebound in Visual it still covers the same text.
#[test]
fn a_dragged_selection_repeats_whatever_the_keymap() {
    let mut app = mouse_app();
    app.config.keys.visual.insert("l".into(), "nop".into());
    app.config.keys.visual.insert("o".into(), "nop".into());
    app.config.keys.normal.insert("v".into(), "nop".into());
    app.keymap = Keymap::from_config(&app.config.keys, &app.config.input.leader).0;
    press(&mut app, 12, 4);
    drag(&mut app, 8, 4);
    release(&mut app, 8, 4);
    feed(&mut app, "d");
    assert_eq!(app.buffer.line_text(2), "secondgraph here");
    feed(&mut app, ".");
    assert_eq!(app.buffer.line_text(2), "second here");
}

/// A command that ends in another document than it began in edited
/// nothing: an empty insert session moved there by a click is not the
/// change, and `.` keeps the one before it.
#[test]
fn a_session_that_ends_in_another_document_is_not_the_change() {
    let dir = two_files();
    let mut app = app_with("scratch\n");
    app.open_file(dir.join("one.md"));
    feed(&mut app, "x");
    app.split_pane(true);
    app.open_file(dir.join("two.md"));
    let one_pane = app.layout.ids()[0];
    feed(&mut app, "i");
    app.focus_pane_id(one_pane);
    assert_eq!(shown(&app), "one.md");
    assert!(!app.edited_since_mark(), "a revision of two.md says nothing here");
    esc(&mut app);
    assert_eq!(dot_keys(&app), "x");
    std::fs::remove_dir_all(&dir).ok();
}

/// A window key bound in `[keys.insert]` and refused there (no link under
/// the cursor) is not part of the insert session `.` repeats.
#[test]
fn a_refused_window_key_in_insert_stays_out_of_the_session() {
    let mut cfg = Config::default();
    cfg.keys.insert.insert("<C-g>".to_string(), "follow_link".to_string());
    let mut app = App::new(cfg, None, None).unwrap();
    feed(&mut app, "iab");
    ctrl(&mut app, 'g');
    feed(&mut app, "c");
    esc(&mut app);
    assert_eq!(text(&app), "abc");
    assert_eq!(dot_keys(&app), "iabc<Esc>");
}

/// Every window command names a real command.
#[test]
fn window_commands_are_commands() {
    for name in super::commands::WINDOW_COMMANDS {
        assert!(super::commands::canonical(super::commands::COMMANDS, name) == Some(*name), "{name}");
    }
}

// ------------------------------------------- the final newline is never selected

/// A drag that ends on the empty line past the last newline stops at the
/// last character: `gb` wraps the text, not the newline after it.
#[test]
fn a_drag_onto_the_empty_last_line_stops_at_the_last_character() {
    let mut app = mouse_app();
    press(&mut app, 8, 4);
    drag(&mut app, 0, 5);
    release(&mut app, 0, 5);
    assert_eq!(app.mode, Mode::Visual);
    assert_eq!(app.buffer.cursor, Cursor::new(2, 20), "the last character");
    feed(&mut app, "gb");
    assert_eq!(text(&app), "# One\n\nsecond** paragraph here**\n");

    // The same drag, then `y` and `d`.
    let mut app = mouse_app();
    press(&mut app, 8, 4);
    drag(&mut app, 0, 5);
    feed(&mut app, "y");
    let yanked = app.registers.get(&'"').expect("the unnamed register").text.clone();
    assert_eq!(yanked, " paragraph here");
    let mut app = mouse_app();
    press(&mut app, 8, 4);
    drag(&mut app, 0, 5);
    feed(&mut app, "d");
    assert_eq!(text(&app), "# One\n\nsecond\n");
}

/// A drag that starts there is anchored on the last character too.
#[test]
fn a_drag_from_the_empty_last_line_starts_at_the_last_character() {
    let mut app = mouse_app();
    press(&mut app, 0, 5);
    drag(&mut app, 8, 4);
    release(&mut app, 8, 4);
    assert_eq!(app.mode, Mode::Visual);
    assert_eq!(app.anchor, Some(Cursor::new(2, 20)));
    feed(&mut app, "gl");
    assert_eq!(text(&app), "# One\n\nsecond[ paragraph here]()\n");
}

/// Keys reach the same end: `vG$` stops on the last character, and on a
/// file whose last line is empty, that line's newline (the file's last) is
/// left out of what the selection covers.
#[test]
fn a_keyboard_selection_never_covers_the_final_newline() {
    let mut app = app_with("aa\nbb\n");
    feed(&mut app, "vG$gb");
    assert_eq!(text(&app), "**aa\nbb**\n");

    let mut app = app_with("aa\n\n");
    feed(&mut app, "vG$gb");
    assert_eq!(text(&app), "**aa\n**\n");
    let mut app = app_with("aa\n\n");
    feed(&mut app, "vjd");
    assert_eq!(text(&app), "\n");
    let mut app = app_with("aa\n\n");
    feed(&mut app, "vjy");
    assert_eq!(app.registers.get(&'"').unwrap().text, "aa\n");
    // Only the final newline selected: nothing to act on.
    let mut app = app_with("aa\n\n");
    feed(&mut app, "Gvgb");
    assert_eq!(text(&app), "aa\n\n");
    assert_eq!(app.mode, Mode::Normal);
    let mut app = app_with("aa\n\n");
    feed(&mut app, "Gvd");
    assert_eq!(text(&app), "aa\n\n");
}

/// A file with no trailing newline: the selection runs to its last
/// character, by keys or by a drag past the end.
#[test]
fn a_selection_in_a_file_without_a_trailing_newline() {
    let mut app = app_with("aa\nbb");
    feed(&mut app, "vG$gb");
    assert_eq!(text(&app), "**aa\nbb**");

    // Past the end of the last line; below it, a drag keeps the column
    // it is level with, as in vim.
    let mut app = app_with("# One\n\nsecond paragraph here");
    app.term_size = (60, 12);
    press(&mut app, 8, 4);
    drag(&mut app, 50, 4);
    feed(&mut app, "gb");
    assert_eq!(text(&app), "# One\n\nsecond** paragraph here**");
}

/// An empty file, or one that is a single newline, has nothing to select:
/// the writer verbs and operators leave it as it is.
#[test]
fn a_selection_in_an_empty_file_changes_nothing() {
    for start in ["", "\n"] {
        for keys in ["vgb", "vgl", "vd", "vgi", "v~"] {
            let mut app = app_with(start);
            feed(&mut app, keys);
            esc(&mut app);
            assert_eq!(text(&app), start, "{keys} on {start:?}");
        }
    }
}

// ------------------------------------- an insert session stays in its pane

/// Press the left button on the first line of pane `id`, at its tenth cell.
fn click_pane(app: &mut App, id: crate::render::pane::PaneId) {
    let area = Rect { x: 0, y: 0, width: app.term_size.0, height: app.term_size.1 };
    let col = (0..app.term_size.0)
        .find(|&c| frame::pane_at(app, area, c, 2) == Some(id))
        .expect("the pane is on screen");
    press(app, col + 10, 2);
    release(app, col + 10, 2);
    assert_eq!(app.focus_pane, id, "the click focused the pane");
}

/// one.md in the left pane, two.md in the right, focus on the right.
fn two_panes() -> (App, PathBuf, crate::render::pane::PaneId, crate::render::pane::PaneId) {
    let dir = two_files();
    let mut app = app_with("scratch\n");
    app.term_size = (100, 20);
    app.open_file(dir.join("one.md"));
    app.split_pane(true);
    app.open_file(dir.join("two.md"));
    let ids = app.layout.ids();
    let right = app.focus_pane;
    let left = *ids.iter().find(|&&id| id != right).unwrap();
    (app, dir, left, right)
}

fn doc_text(app: &App, name: &str) -> String {
    let d = app
        .docs
        .iter()
        .find(|d| d.buffer.path.as_ref().is_some_and(|p| p.ends_with(name)))
        .expect("the document is open");
    d.buffer.rope.to_string()
}

/// A click into another pane mid-insert ends the session there, as Esc
/// would, and starts a new one where it landed: each pane's `u` takes back
/// its own typing and nothing else.
#[test]
fn a_click_into_another_pane_mid_insert_starts_a_new_session() {
    let (mut app, dir, left, right) = two_panes();
    feed(&mut app, "iAB");
    click_pane(&mut app, left);
    assert_eq!(app.mode, Mode::Insert, "still inserting, in the pane clicked into");
    assert_eq!(shown(&app), "one.md");
    feed(&mut app, "XY");
    esc(&mut app);
    assert_eq!(doc_text(&app, "two.md"), "ABfile two\n");
    let one = doc_text(&app, "one.md");
    assert!(one.contains("XY"), "{one:?}");

    feed(&mut app, "u");
    assert_eq!(doc_text(&app, "one.md"), "file one\n", "the left pane's typing only");
    assert_eq!(doc_text(&app, "two.md"), "ABfile two\n");
    feed(&mut app, "u");
    assert_eq!(doc_text(&app, "one.md"), "file one\n", "nothing more to undo here");

    click_pane(&mut app, right);
    assert_eq!(app.mode, Mode::Normal);
    // The session there was not left open: Esc never moved its cursor
    // back, and `u` takes back its typing as one step.
    feed(&mut app, "u");
    assert_eq!(doc_text(&app, "two.md"), "file two\n");
    std::fs::remove_dir_all(&dir).ok();
}

/// The session the click ended is the change `.` repeats, complete; the
/// typing in the pane clicked into is a session of its own.
#[test]
fn dot_after_a_click_mid_insert_repeats_the_right_session() {
    // Nothing typed after the click: `.` is the first session.
    let (mut app, dir, left, _) = two_panes();
    feed(&mut app, "iAB");
    click_pane(&mut app, left);
    esc(&mut app);
    assert_eq!(dot_keys(&app), "iAB<Esc>");
    feed(&mut app, "0.");
    assert_eq!(doc_text(&app, "one.md"), "ABfile one\n");
    std::fs::remove_dir_all(&dir).ok();

    // Typed after the click: `.` is that typing, nothing of the first.
    let (mut app, dir, left, _) = two_panes();
    feed(&mut app, "iAB");
    click_pane(&mut app, left);
    feed(&mut app, "XY");
    esc(&mut app);
    assert_eq!(dot_keys(&app), "<insert>XY<Esc>");
    feed(&mut app, "0.");
    assert!(doc_text(&app, "one.md").starts_with("XY"), "{:?}", doc_text(&app, "one.md"));
    assert_eq!(doc_text(&app, "two.md"), "ABfile two\n");
    std::fs::remove_dir_all(&dir).ok();
}

/// Two panes on one document: the click still splits the typing into two
/// sessions, two undo steps.
#[test]
fn a_click_into_another_pane_on_the_same_document_splits_the_undo() {
    let mut app = app_with("one\n");
    app.term_size = (100, 20);
    app.split_pane(true);
    let right = app.focus_pane;
    let left = *app.layout.ids().iter().find(|&&id| id != right).unwrap();
    feed(&mut app, "iAB");
    click_pane(&mut app, left);
    feed(&mut app, "XY");
    esc(&mut app);
    assert!(text(&app).contains("AB") && text(&app).contains("XY"), "{:?}", text(&app));
    feed(&mut app, "u");
    assert_eq!(text(&app), "ABone\n");
    feed(&mut app, "u");
    assert_eq!(text(&app), "one\n");
}

/// A key bound in `[keys.insert]` that switches documents in the pane ends
/// the session the same way: sealed, and the change `.` repeats.
#[test]
fn a_document_switch_mid_insert_seals_the_session() {
    let dir = two_files();
    let mut cfg = Config::default();
    cfg.keys.insert.insert("<C-g>".to_string(), "alternate_buffer".to_string());
    let mut app = App::new(cfg, None, None).unwrap();
    app.open_file(dir.join("one.md"));
    app.open_file(dir.join("two.md"));
    feed(&mut app, "iAB");
    ctrl(&mut app, 'g');
    assert_eq!(shown(&app), "one.md");
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(dot_keys(&app), "iAB<Esc>");
    feed(&mut app, "x");
    ctrl(&mut app, '^');
    assert_eq!(shown(&app), "two.md");
    // The typing is one step, and the `x` in one.md is not in it.
    feed(&mut app, "u");
    assert_eq!(doc_text(&app, "two.md"), "file two\n");
    assert_eq!(doc_text(&app, "one.md"), "ile one\n");
    std::fs::remove_dir_all(&dir).ok();
}


/// An embed of a picture that is not there says so as a picture, and an
/// embed of a missing note keeps calling it a note.
#[test]
fn an_embed_of_a_missing_picture_is_called_a_picture() {
    let (_d, mut app) = vault("intro\n![[pics/shot.png]]\n![[fragments/next]]\n", &[]);
    app.embed_mode = crate::transclude::Mode::Full;
    assert_eq!(embed_rows(&mut app, 1), ["![[pics/shot.png]] — no picture called \"pics/shot.png\""]);
    assert_eq!(embed_rows(&mut app, 2), ["![[fragments/next]] — no note called \"fragments/next\""]);
}

/// `gx` on a wiki link to a missing picture reports it as a picture.
#[test]
fn gx_on_a_missing_picture_names_it_a_picture() {
    let (_d, mut app) = vault("see [[shot.webp]]\n", &[]);
    cursor_on(&mut app, 0, "shot.webp");
    feed(&mut app, "gx");
    assert_eq!(flash_text(&app), "no picture called \"shot.webp\"");
}

/// The `:focus` and `:set focus` words are read whatever their case.
#[test]
fn focus_takes_its_word_in_any_case() {
    let mut app = app_with("one\n");
    for (cmd, want) in [
        (":focus Paragraph", FocusMode::Paragraph),
        (":focus SENTENCE", FocusMode::Sentence),
        (":set focus=Off", FocusMode::Off),
        (":set focus=PARA", FocusMode::Paragraph),
    ] {
        feed(&mut app, cmd);
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        app.sync_after_input();
        assert_eq!(app.focus, want, "{cmd}: {:?}", flash_text(&app));
    }
}

/// `3"<Esc>j` moves one line, and `d"<Esc>j` deletes nothing: `Esc` after
/// the register prefix drops the count and operator with it.
#[test]
fn escape_after_a_register_prefix_drops_the_pending_command() {
    let mut app = app_with("a\nb\nc\nd\ne\n");
    feed(&mut app, "3\"");
    esc(&mut app);
    feed(&mut app, "j");
    assert_eq!(app.buffer.cursor.line, 1, "one line, not three");
    feed(&mut app, "d\"");
    esc(&mut app);
    feed(&mut app, "j");
    assert_eq!(text(&app), "a\nb\nc\nd\ne\n", "nothing deleted");
    assert_eq!(app.buffer.cursor.line, 2);
}

// ── Display math laid out over several rows (#77) ──────────────────────────
//
// Written to hold whatever `render::math::layout` draws: they assert on row
// counts, positions, hidden lines and centring, and read the rows themselves
// back from the cache rather than spelling them out.

/// A paragraph, a closed block of four lines (2..=5), and a paragraph.
const MATH_DOC: &str = "intro\n\n$$\n\\frac{a}{b}\nx + y\n$$\n\nafter\n";

/// Draw `app` into a `w`×`h` terminal and return its rows as text.
fn screen_of(app: &mut App, w: u16, h: u16) -> Vec<String> {
    app.refresh_blocks();
    let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::render::frame::render(f, app)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect()
}

/// The rows the layout of the block opened at `line` draws this frame.
fn math_rows(app: &App, line: usize) -> Vec<(usize, String)> {
    let cache = app.cache.borrow();
    (0..).map_while(|i| cache.math_row(line, i).map(|(p, s)| (p, s.to_string()))).collect()
}

/// The measure a `w`×`h` frame draws at.
fn measure_of(app: &App, w: u16, h: u16) -> usize {
    crate::render::layout::Layout::compute(&app.config.layout, w, h, 0).measure as usize
}

/// A closed block draws its layout on its opening line, and the rest of its
/// lines take no rows; the paragraph after it starts right below the layout.
#[test]
fn a_closed_math_block_lays_out_on_its_opening_line() {
    let mut app = app_with(MATH_DOC);
    screen_of(&mut app, 60, 20);
    let rows = math_rows(&app, 2);
    assert!(!rows.is_empty(), "laid out");
    let cache = app.cache.borrow();
    assert_eq!(cache.line_rows(2), (2, 2 + rows.len() - 1));
    for line in 3..=5 {
        assert_eq!(cache.hidden().shown(line), 2, "line {line} takes no rows");
    }
    assert_eq!(cache.locate(6, 0).0, 2 + rows.len(), "the blank line follows the layout");
    let row = cache.row(3).unwrap();
    assert_eq!((row.line(), row.embedded(), row.editable), (2, Some(1), false));
}

/// A one-line `$$…$$` lays out on itself.
#[test]
fn a_one_line_math_block_lays_out() {
    let mut app = app_with("intro\n\n$$\\sum_{i=1}^{n} i$$\n\nafter\n");
    screen_of(&mut app, 60, 20);
    let rows = math_rows(&app, 2);
    assert!(!rows.is_empty());
    let cache = app.cache.borrow();
    assert_eq!(cache.locate(3, 0).0, 2 + rows.len());
}

/// An unclosed block (bounded by a blank line) and a block too wide for the
/// measure keep today's form: one row per line, nothing hidden.
#[test]
fn unclosed_and_too_wide_blocks_keep_their_per_line_form() {
    let mut app = app_with("intro\n$$\n\\alpha\n\nafter\n");
    screen_of(&mut app, 60, 20);
    assert!(math_rows(&app, 1).is_empty());
    assert_eq!(app.cache.borrow().locate(4, 0).0, 4, "every line one row");

    let wide = (0..30).map(|i| format!("a_{i}")).collect::<Vec<_>>().join(" + ");
    let mut app = app_with(&format!("intro\n$$\n{wide}\n$$\nafter\n"));
    screen_of(&mut app, 40, 20);
    assert!(math_rows(&app, 1).is_empty(), "too wide: no layout");
    let cache = app.cache.borrow();
    assert!(cache.hidden().ranges().is_empty());
    assert!(cache.locate(4, 0).0 > 4, "the wide line wraps as it did");
}

/// The cursor on any line of a block, or a Visual selection touching it,
/// shows all of its source: the active set widens to the whole block.
#[test]
fn a_cursor_or_selection_in_a_block_reveals_all_of_it() {
    for line in 2..=5 {
        let mut app = app_with(MATH_DOC);
        app.buffer.cursor = Cursor::new(line, 0);
        app.sync_after_input();
        assert_eq!((app.active.start, app.active.end), (2, 5), "cursor on {line}");
        screen_of(&mut app, 60, 20);
        assert!(math_rows(&app, 2).is_empty(), "raw while the cursor is on {line}");
        let cache = app.cache.borrow();
        assert_eq!(cache.locate(7, 0).0, 7, "every line takes its row");
    }
    // A selection from above that reaches the opening line.
    let mut app = app_with(MATH_DOC);
    feed(&mut app, "Vjj");
    assert_eq!((app.active.start, app.active.end), (0, 5));
    // One from below that reaches the closing line.
    let mut app = app_with(MATH_DOC);
    app.buffer.cursor = Cursor::new(7, 0);
    feed(&mut app, "vkk");
    assert_eq!((app.active.start, app.active.end), (2, 7));
    screen_of(&mut app, 60, 20);
    assert!(math_rows(&app, 2).is_empty());
    // Read-only mode shows the layout wherever the cursor is.
    let mut app = app_with(MATH_DOC);
    app.buffer.cursor = Cursor::new(3, 0);
    app.set_view(true);
    screen_of(&mut app, 60, 20);
    assert!(!math_rows(&app, 2).is_empty());
}

/// `j` from above lands on the opening line and `k` from below on the
/// closing one; either shows the block's source. A search landing inside
/// does too.
#[test]
fn j_and_k_step_into_a_block_from_either_side() {
    let mut app = app_with(MATH_DOC);
    app.buffer.cursor = Cursor::new(1, 0);
    feed(&mut app, "j");
    assert_eq!(app.buffer.cursor.line, 2);
    assert_eq!((app.active.start, app.active.end), (2, 5));
    feed(&mut app, "jjjj");
    assert_eq!(app.buffer.cursor.line, 6, "j walks through the block's lines");

    let mut app = app_with(MATH_DOC);
    app.buffer.cursor = Cursor::new(6, 0);
    feed(&mut app, "k");
    assert_eq!(app.buffer.cursor.line, 5);
    assert_eq!((app.active.start, app.active.end), (2, 5));

    let mut app = app_with(MATH_DOC);
    cmd(&mut app, "/x + y");
    assert_eq!(app.buffer.cursor.line, 4);
    assert_eq!((app.active.start, app.active.end), (2, 5));
    feed(&mut app, "G");
    assert_eq!(app.active.start, app.active.end, "away again");
}

/// A click on any row of the layout puts the cursor on the opening line.
#[test]
fn a_click_on_a_layout_row_lands_on_the_opening_line() {
    let mut app = app_with(MATH_DOC);
    app.term_size = (60, 20);
    app.buffer.cursor = Cursor::new(7, 0);
    app.sync_after_input();
    let screen = screen_of(&mut app, 60, 20);
    let rows = math_rows(&app, 2);
    let last = rows.last().unwrap().1.trim().to_string();
    let y = screen.iter().position(|r| r.contains(&last)).expect("the layout is on screen");
    let x = screen[y].find(&last).unwrap() as u16;
    press(&mut app, x, y as u16);
    release(&mut app, x, y as u16);
    assert_eq!(app.buffer.cursor, Cursor::new(2, 0));
    assert_eq!((app.active.start, app.active.end), (2, 5), "and shows its source");
}

/// The box is centred in the measure, or in what the opening line's
/// indentation leaves of it, and drawn there.
#[test]
fn a_layout_is_centred_in_the_room_its_indentation_leaves() {
    let width = |s: &str| unicode_width::UnicodeWidthStr::width(s);
    let mut app = app_with(MATH_DOC);
    let screen = screen_of(&mut app, 60, 20);
    let m = measure_of(&app, 60, 20);
    let rows = math_rows(&app, 2);
    let (pad, first) = &rows[0];
    assert_eq!(*pad, (m - width(first)) / 2);
    assert!(rows.iter().all(|(p, r)| p == pad && width(r) == width(first)), "one box");
    let y = screen.iter().position(|r| r.contains(first.trim_end())).unwrap();
    let left = screen[y].chars().take_while(|c| *c == ' ').count();
    let lead = first.chars().take_while(|c| *c == ' ').count();
    assert_eq!(left - lead, (60 - m) / 2 + pad, "drawn where the cache says");

    let mut app = app_with("- item\n\n    $$\n    \\frac{a}{b}\n    $$\n");
    screen_of(&mut app, 60, 20);
    let rows = math_rows(&app, 2);
    let (pad, first) = &rows[0];
    assert_eq!(*pad, 4 + (m - 4 - width(first)) / 2);
}

/// A block inside a closed fold stays folded away with it; opening the fold
/// shows it laid out.
#[test]
fn a_block_in_a_closed_fold_stays_hidden() {
    let mut app = app_with("# A\n$$\nx\n$$\ntext\n# B\nend\n");
    feed(&mut app, "zc");
    screen_of(&mut app, 60, 20);
    {
        let cache = app.cache.borrow();
        assert_eq!(cache.hidden().run(1), Some(1..5));
        assert_eq!(cache.hidden().ranges().len(), 1);
        assert_eq!(cache.locate(5, 0).0, 1);
    }
    assert!(math_rows(&app, 1).is_empty());
    feed(&mut app, "zo");
    screen_of(&mut app, 60, 20);
    assert!(!math_rows(&app, 1).is_empty());
}

/// Line spacing puts no air inside a layout, and the usual spacing after it.
#[test]
fn line_spacing_goes_after_a_layout_not_inside_it() {
    let mut app = app_with("intro\n$$\n\\frac{a}{b}\nx\n$$\nafter\n");
    app.config.layout.line_spacing = 1;
    screen_of(&mut app, 60, 30);
    let n = math_rows(&app, 1).len();
    let cache = app.cache.borrow();
    let start = cache.locate(1, 0).0;
    assert_eq!(start, 2, "intro and its spacing");
    for r in start..start + n {
        assert!(!cache.row(r).unwrap().is_spacer(), "row {r} is the layout");
    }
    assert!(cache.row(start + n).unwrap().is_spacer());
    assert_eq!(cache.locate(5, 0).0, start + n + 1);
}

/// `--render` prints the layout: the rows between the paragraphs are the
/// blank line, the layout's rows and the blank line.
#[test]
fn render_prints_the_layout() {
    let mut app = app_with(MATH_DOC);
    let page = crate::render::print::styled(&mut app, 60);
    let plain = strip_ansi(&page);
    let lines: Vec<&str> = plain.lines().collect();
    let at = |w: &str| lines.iter().position(|l| l.contains(w)).unwrap();
    let rows = math_rows(&app, 2);
    assert_eq!(at("after") - at("intro"), rows.len() + 3);
    for (i, (_, row)) in rows.iter().enumerate() {
        assert!(lines[at("intro") + 2 + i].contains(row.trim_end()), "{plain}");
    }
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        match (esc, c) {
            (false, '\x1b') => esc = true,
            (false, c) => out.push(c),
            (true, c) if c.is_ascii_alphabetic() => esc = false,
            _ => {}
        }
    }
    out
}

/// The layout depends on every line of the block: an edit to a middle line
/// lays it out again once the cursor leaves, and nothing else does.
#[test]
fn editing_a_middle_line_lays_the_block_out_again() {
    let mut app = app_with(MATH_DOC);
    screen_of(&mut app, 60, 20);
    let before = math_rows(&app, 2);
    let layouts = app.cache.borrow().layouts();
    app.buffer.cursor = Cursor::new(4, 0);
    app.sync_after_input();
    feed(&mut app, "A + z");
    screen_of(&mut app, 60, 20);
    esc(&mut app);
    screen_of(&mut app, 60, 20);
    assert!(math_rows(&app, 2).is_empty(), "still raw in Normal on the block");
    feed(&mut app, "G");
    screen_of(&mut app, 60, 20);
    let after = math_rows(&app, 2);
    assert!(!after.is_empty());
    assert_ne!(after, before, "the new text is laid out");
    assert_eq!(app.cache.borrow().layouts(), layouts + 1, "once");
}

/// Moving the cursor around a document of many blocks, in and out of them,
/// parses nothing and lays out nothing again.
#[test]
fn moving_around_many_blocks_lays_out_nothing_again() {
    let doc: String = (0..12)
        .map(|i| format!("para {i}\n\n$$\n\\frac{{{i}}}{{2}}\ny_{i}\n$$\n\n$$x^{i}$$\n\n"))
        .collect();
    let mut app = app_with(&doc);
    screen_of(&mut app, 60, 20);
    let (builds, layouts) = {
        let c = app.cache.borrow();
        (c.builds(), c.layouts())
    };
    assert_eq!(layouts, 24, "every block once");
    for _ in 0..(12 * 9) {
        feed(&mut app, "j");
        screen_of(&mut app, 60, 20);
    }
    feed(&mut app, "gg");
    screen_of(&mut app, 60, 20);
    let c = app.cache.borrow();
    assert_eq!(c.builds(), builds, "no line parsed again");
    assert_eq!(c.layouts(), layouts, "no block laid out again");
}

/// `[markdown] math` off, or concealment off, keeps every block as it was.
#[test]
fn math_or_conceal_off_lays_out_nothing() {
    let mut app = app_with(MATH_DOC);
    app.config.markdown.math = false;
    screen_of(&mut app, 60, 20);
    assert!(math_rows(&app, 2).is_empty());
    assert_eq!(app.cache.borrow().locate(7, 0).0, 7);

    let mut app = app_with(MATH_DOC);
    app.config.layout.conceal = false;
    app.buffer.cursor = Cursor::new(3, 0);
    app.sync_after_input();
    assert_eq!((app.active.start, app.active.end), (3, 3), "nothing to widen for");
    screen_of(&mut app, 60, 20);
    assert!(math_rows(&app, 2).is_empty());
    assert_eq!(app.cache.borrow().locate(7, 0).0, 7);
}

// ── #77 fix round: kinds and the active set kept in step outside input ────

/// A Markdown file `name` holding `text`, in a fresh scratch directory.
fn math_file(name: &str, text: &str) -> PathBuf {
    let dir = two_files();
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

/// The document whose file is `name`.
fn doc_index(app: &App, name: &str) -> usize {
    app.docs
        .iter()
        .position(|d| d.buffer.path.as_ref().is_some_and(|p| p.ends_with(name)))
        .expect("the document is open")
}

/// A file that opens with the cursor on a closed block shows all of its
/// source on the very first frame, before any key: the active set is widened
/// at startup, not only after the first input event.
#[test]
fn a_file_opened_on_a_block_shows_its_source_on_the_first_frame() {
    let path = math_file("first.md", "$$\nx\n$$\nAfter\n");
    let mut app = App::new(Config::default(), Some(path), None).unwrap();
    assert_eq!((app.active.start, app.active.end), (0, 2), "widened at startup");
    let screen = screen_of(&mut app, 60, 20);
    assert!(math_rows(&app, 0).is_empty(), "raw, not laid out");
    let cache = app.cache.borrow();
    assert!(cache.hidden().ranges().is_empty(), "nothing hidden");
    assert_eq!(cache.locate(3, 0).0, 3, "every line takes its row");
    let shown: Vec<&str> = screen.iter().map(|r| r.trim()).filter(|r| !r.is_empty()).collect();
    assert_eq!(shown.iter().filter(|r| **r == "$$").count(), 2, "both delimiters raw: {shown:?}");
}

/// A reload that turns the cursor's line into a closed block widens the
/// active set as the kinds are refreshed for the next frame, with no input
/// event in between.
#[test]
fn a_reload_that_makes_the_cursor_line_a_block_widens_the_active_set() {
    let (mut app, path) = app_on_disk("math-reload", "prose\nmore\n");
    app.buffer.cursor = Cursor::new(0, 0);
    app.sync_after_input();
    write_settled(&path, "$$\nx\n$$\nmore\n");
    assert!(app.check_disk());
    app.refresh_blocks(); // what the event loop does before the frame
    assert_eq!((app.active.start, app.active.end), (0, 2));
    screen_of(&mut app, 60, 20);
    assert!(app.cache.borrow().hidden().ranges().is_empty(), "nothing hidden");
    let _ = std::fs::remove_file(path);
}

/// A document reloaded in a pane that does not have focus gets kinds for its
/// new text before that pane is drawn: a block the reload left unclosed is not
/// laid out on the old kinds, and none of its lines is hidden.
#[test]
fn a_background_reload_refreshes_that_documents_kinds() {
    let a = math_file("a.md", "Intro\n$$\nx\n$$\nEnd\n");
    let b = a.with_file_name("b.md");
    std::fs::write(&b, "other\n").unwrap();
    let mut app = app_with("scratch\n");
    app.term_size = (100, 20);
    app.open_file(a.clone());
    app.split_pane(true);
    app.open_file(b);
    let ai = doc_index(&app, "a.md");
    screen_of(&mut app, 100, 20);
    assert!(!app.docs[ai].cache.borrow().hidden().ranges().is_empty(), "laid out before");

    write_settled(&a, "Intro\n$$\nx\ny\nEnd\n");
    assert!(app.check_disk());
    app.refresh_blocks();
    let d = &app.docs[ai];
    assert_eq!(d.blocks.revision, d.buffer.revision, "the reloaded kinds are fresh");
    let screen = screen_of(&mut app, 100, 20);
    assert!(app.docs[ai].cache.borrow().hidden().ranges().is_empty(), "nothing hidden");
    assert!(screen.iter().any(|r| r.contains('y')), "the new line shows: {screen:?}");
}

/// Kinds built for another revision of the text are never used to lay a
/// block out: the index treats them as no blocks at all.
#[test]
fn stale_kinds_lay_nothing_out() {
    let mut app = app_with("Intro\n$$\nx\n$$\nEnd\n");
    screen_of(&mut app, 60, 20);
    assert!(!app.cache.borrow().hidden().ranges().is_empty());
    // An edit the kinds have not caught up with: the block is unclosed now.
    app.buffer.replace_all("Intro\n$$\nx\ny\nEnd\n");
    let doc = &app.docs[app.current()];
    let mut cache = doc.cache.borrow_mut();
    cache.sync_doc(&app, doc, app.focus_pane, 60);
    assert!(cache.hidden().ranges().is_empty(), "stale kinds: nothing laid out");
}

/// A click on a layout row of a pane without focus, whose own cursor sits in
/// that block, lands on the block's opening line: the click is read against
/// the rows that were on screen, not the raw ones focusing reveals.
#[test]
fn a_click_into_another_pane_reads_the_rows_that_were_on_screen() {
    let a = math_file("a.md", MATH_DOC);
    let b = a.with_file_name("b.md");
    std::fs::write(&b, "other\n").unwrap();
    let mut app = app_with("scratch\n");
    app.term_size = (100, 20);
    app.open_file(a);
    // The cursor this pane keeps: inside the block.
    app.buffer.cursor = Cursor::new(3, 0);
    app.sync_after_input();
    let left = app.focus_pane;
    app.split_pane(true);
    app.open_file(b);
    assert_ne!(app.focus_pane, left);
    let screen = screen_of(&mut app, 100, 20);
    let ai = doc_index(&app, "a.md");
    let rows: Vec<String> = {
        let cache = app.docs[ai].cache.borrow();
        (0..).map_while(|i| cache.math_row(2, i).map(|(_, s)| s.trim().to_string())).collect()
    };
    assert!(rows.len() >= 2, "a layout of several rows: {rows:?}");
    let last = rows.last().unwrap();
    let area = Rect { x: 0, y: 0, width: 100, height: 20 };
    let (x, y) = screen
        .iter()
        .enumerate()
        .find_map(|(y, r)| {
            let x = r.find(last.as_str())?;
            let x = r[..x].chars().count() as u16;
            (frame::pane_at(&app, area, x, y as u16) == Some(left)).then_some((x, y as u16))
        })
        .expect("the layout's last row is in the left pane");
    press(&mut app, x, y);
    release(&mut app, x, y);
    assert_eq!(app.focus_pane, left);
    assert_eq!(app.buffer.cursor, Cursor::new(2, 0), "the opening line");
}

/// The closed blocks are found once per classification and kept with it: an
/// edit drops them, the next lookup finds them again, and cursor moves read
/// the kept list.
#[test]
#[allow(clippy::single_range_in_vec_init)] // block ranges, not a range to expand
fn closed_block_ranges_are_kept_with_the_kinds() {
    let mut app = app_with(MATH_DOC);
    assert!(app.blocks.closed_math().is_none(), "not looked for yet");
    app.buffer.cursor = Cursor::new(3, 0);
    app.sync_after_input();
    assert_eq!(app.blocks.closed_math(), Some(&[2..6][..]), "found by the lookup");
    feed(&mut app, "j");
    assert_eq!(app.blocks.closed_math(), Some(&[2..6][..]));
    feed(&mut app, "Oz");
    esc(&mut app);
    assert_eq!(app.blocks.closed_math(), Some(&[2..7][..]), "found again after the edit");
    // A blank line ends a block: it is no longer closed.
    feed(&mut app, "o");
    esc(&mut app);
    assert!(app.blocks.closed_math().is_none(), "the cursor's blank line is not math");
    feed(&mut app, "k");
    assert_eq!(app.blocks.closed_math(), Some(&[][..]));
}

// ── #77 fix round 2 ─────────────────────────────────────────────────────

/// Turning `[markdown] math` on by saving the watched config, with the
/// cursor on a block's opening line and no key pressed, shows the whole
/// block raw on the next frame: the reload widens the active set over the
/// block it just made, rather than leaving the one-line set of a math-off
/// document to conceal the body and closer until the next input.
#[test]
fn a_config_reload_that_enables_math_widens_the_active_set() {
    let mut cfg = Config::default();
    cfg.markdown.math = false;
    let mut app = app_with_config(MATH_DOC, cfg);
    app.buffer.cursor = Cursor::new(2, 0);
    app.sync_after_input();
    assert_eq!((app.active.start, app.active.end), (2, 2), "math off: one line");

    let conf = two_files().join("shoin.conf");
    std::fs::write(&conf, "[markdown]\nmath = true\n").unwrap();
    app.watcher = ConfigWatcher::new(Some(conf)).unwrap();
    app.reload_config();
    app.refresh_blocks(); // what the event loop does before the frame
    assert!(app.config.markdown.math);
    assert_eq!((app.active.start, app.active.end), (2, 5), "the whole block, raw");
    let rows = screen_of(&mut app, 60, 20);
    assert!(rows.iter().any(|r| r.contains("x + y")), "the body shows raw: {rows:#?}");
}

/// Typing prose away from any math block never looks for the blocks: the
/// active set rejects a line that is not math by its kind, so the ranges
/// dropped by an edit are not rebuilt for every key.
#[test]
#[allow(clippy::single_range_in_vec_init)] // block ranges, not a range to expand
fn typing_prose_does_not_rebuild_the_block_ranges() {
    let mut app = app_with(MATH_DOC);
    app.buffer.cursor = Cursor::new(0, 0);
    feed(&mut app, "ihello world");
    assert!(app.blocks.closed_math().is_none(), "never looked for");
    esc(&mut app);
    feed(&mut app, "j");
    assert!(app.blocks.closed_math().is_none(), "the blank line is not math either");
    feed(&mut app, "j");
    assert_eq!(app.blocks.closed_math(), Some(&[2..6][..]), "a math line looks them up");
    assert_eq!((app.active.start, app.active.end), (2, 5));
}

// ------------------------------------------- read-only past a tall block

/// A 20-row matrix in display math after a line of prose, then `after`
/// lines of prose. No trailing newline.
fn tall_math(after: usize) -> String {
    let rows: Vec<String> = (0..20).map(|i| format!("a_{{{i}}} & b_{{{i}}}")).collect();
    let mut t = format!("intro\n\n$$\n\\begin{{pmatrix}}\n{}\n\\end{{pmatrix}}\n$$", rows.join(" \\\\\n"));
    for i in 0..after {
        t.push_str(&format!("\nafter {i}"));
    }
    t
}

/// The size the read-only tests draw at: the text area is shorter than
/// the 20 rows of the matrix.
const READ_W: u16 = 60;
const READ_H: u16 = 12;

/// `app` read-only and drawn once.
fn reader(app: &mut App) {
    app.set_view(true);
    app.term_size = (READ_W, READ_H);
    screen_of(app, READ_W, READ_H);
}

/// A key, then the frame it leaves.
fn read_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Vec<String> {
    app.on_key(KeyEvent::new(code, mods));
    app.sync_after_input();
    screen_of(app, READ_W, READ_H)
}

/// The row the view would start at if it showed the end: the last row the
/// view's top can reach, with the last line on the bottom row. The empty
/// line after a final newline is not part of it.
fn last_top(app: &App) -> usize {
    let cache = app.cache.borrow();
    let end = cache.line_rows(app.buffer.last_line()).1 + 1;
    end - app.viewport_height()
}

/// Every row a key sequence brings to the screen in read-only mode, from
/// the frame it starts on: `step` pressed until the view stops moving.
fn read_through(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Vec<String> {
    let mut seen = screen_of(app, READ_W, READ_H);
    let mut top = app.scroll_hint();
    for _ in 0..200 {
        seen.extend(read_key(app, code, mods));
        if app.scroll_hint() == top {
            return seen;
        }
        top = app.scroll_hint();
    }
    panic!("{code:?} never stopped");
}

/// Whether `want` is drawn on one of `rows`.
fn drawn(rows: &[String], want: &str) -> bool {
    rows.iter().any(|r| r.contains(want))
}

/// The layout's own rows, as drawn.
fn layout_rows(app: &App, line: usize) -> Vec<String> {
    math_rows(app, line).into_iter().map(|(_, s)| s.trim().to_string()).collect()
}

/// `j` moves the view down one row at a time, through every row of a
/// layout taller than the pane and on to the lines after it, as it does
/// through ordinary lines; `k` comes back the same way.
#[test]
fn read_only_j_and_k_go_row_by_row_through_a_tall_layout() {
    for after in [0, 3] {
        let mut app = app_with(&tall_math(after));
        reader(&mut app);
        let rows = layout_rows(&app, 2);
        assert_eq!(rows.len(), 20, "laid out, taller than the pane");
        let end = last_top(&app);
        for top in 1..=end {
            let screen = read_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            assert_eq!(app.scroll_hint(), top, "after {after}: one row per j\n{screen:#?}");
        }
        let screen = read_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.scroll_hint(), end, "the end stops it");
        assert!(drawn(&screen, rows.last().unwrap()), "the layout's last row\n{screen:#?}");
        if after > 0 {
            assert!(drawn(&screen, &format!("after {}", after - 1)), "the last line\n{screen:#?}");
        }
        for top in (0..end).rev() {
            read_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
            assert_eq!(app.scroll_hint(), top, "after {after}: one row per k");
        }
    }
}

/// Every scrolling key reaches the layout's last row and the line after
/// it going down, and its first rows and the line before it going up.
#[test]
fn read_only_paging_keys_reach_past_a_tall_layout() {
    let ctrl = KeyModifiers::CONTROL;
    let none = KeyModifiers::NONE;
    let pairs = [
        ((KeyCode::Char('j'), none), (KeyCode::Char('k'), none)),
        ((KeyCode::Char('d'), ctrl), (KeyCode::Char('u'), ctrl)),
        ((KeyCode::Char('f'), ctrl), (KeyCode::Char('b'), ctrl)),
        ((KeyCode::Down, none), (KeyCode::Up, none)),
    ];
    for after in [0, 3] {
        for ((down, dm), (up, um)) in pairs {
            let mut app = app_with(&tall_math(after));
            reader(&mut app);
            let rows = layout_rows(&app, 2);
            let seen = read_through(&mut app, down, dm);
            assert_eq!(app.scroll_hint(), last_top(&app), "{down:?} reaches the end");
            for row in &rows {
                assert!(drawn(&seen, row), "{down:?} after {after}: {row} never shown");
            }
            if after > 0 {
                assert!(drawn(&seen, "after 0") && drawn(&seen, &format!("after {}", after - 1)), "{down:?}");
            }
            let seen = read_through(&mut app, up, um);
            assert_eq!(app.scroll_hint(), 0, "{up:?} back to the top");
            for row in &rows {
                assert!(drawn(&seen, row), "{up:?} after {after}: {row} never shown");
            }
            assert!(drawn(&seen, "intro"));
        }
    }
}

/// The wheel scrolls through a tall layout too, three rows a notch.
#[test]
fn read_only_wheel_scrolls_through_a_tall_layout() {
    let mut app = app_with(&tall_math(3));
    reader(&mut app);
    let rows = layout_rows(&app, 2);
    let mut seen = screen_of(&mut app, READ_W, READ_H);
    for _ in 0..20 {
        app.scroll_view(3);
        app.sync_after_input();
        seen.extend(screen_of(&mut app, READ_W, READ_H));
    }
    assert_eq!(app.scroll_hint(), last_top(&app));
    for row in &rows {
        assert!(drawn(&seen, row), "{row} never shown");
    }
    assert!(drawn(&seen, "after 2"));
    app.scroll_view(-3);
    app.sync_after_input();
    screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.scroll_hint(), last_top(&app) - 3);
}

/// `G` shows the end of the file, `gg` the start, wherever the end falls.
#[test]
fn read_only_g_shows_the_end_past_a_tall_layout() {
    for after in [0, 3] {
        let mut app = app_with(&tall_math(after));
        reader(&mut app);
        let rows = layout_rows(&app, 2);
        feed(&mut app, "G");
        let screen = screen_of(&mut app, READ_W, READ_H);
        assert_eq!(app.scroll_hint(), last_top(&app), "after {after}\n{screen:#?}");
        let last = if after > 0 { format!("after {}", after - 1) } else { rows.last().unwrap().clone() };
        assert!(drawn(&screen, &last), "after {after}: {last}\n{screen:#?}");
        // One row back from the end, not the whole layout.
        feed(&mut app, "k");
        screen_of(&mut app, READ_W, READ_H);
        assert_eq!(app.scroll_hint(), last_top(&app) - 1);
        feed(&mut app, "gg");
        let screen = screen_of(&mut app, READ_W, READ_H);
        assert_eq!(app.scroll_hint(), 0);
        assert!(drawn(&screen, "intro"));
    }
}

/// A search still brings its match to the top from inside a layout, and
/// a resize that moves the rows keeps the view on the same line.
#[test]
fn read_only_search_and_resize_from_inside_a_tall_layout() {
    let mut app = app_with(&tall_math(12));
    reader(&mut app);
    feed(&mut app, "jjjjj");
    screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.scroll_hint(), 5, "inside the layout");
    cmd(&mut app, "/after 3");
    let screen = screen_of(&mut app, READ_W, READ_H);
    let top = app.cache.borrow().locate(app.buffer.cursor.line, 0).0;
    assert_eq!(app.scroll_hint(), top, "the match is the top\n{screen:#?}");
    assert!(screen.iter().find(|r| !r.trim().is_empty()).unwrap().contains("after 3"), "{screen:#?}");
    // Narrower: the matrix is still laid out, the cursor's line still on top.
    app.term_size = (40, READ_H);
    screen_of(&mut app, 40, READ_H);
    let top = app.cache.borrow().locate(app.buffer.cursor.line, 0).0;
    assert_eq!(app.scroll_hint(), top);
}

/// An embedded note taller than the pane: the same rule, row by row
/// through the box and on to the lines after it, and `G` to the end.
#[test]
fn read_only_scrolls_through_a_tall_embed() {
    let d = std::env::temp_dir().join(format!("shoin-tall-embed-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let frag: String = (1..=20).map(|i| format!("frag line {i}\n")).collect();
    std::fs::write(d.join("frag.md"), frag).unwrap();
    for host in ["intro\n\n![[frag]]", "intro\n\n![[frag]]\nafter 0\nafter 1\nafter 2\n"] {
        std::fs::write(d.join("host.md"), host).unwrap();
        let mut app = App::new(Config::default(), Some(d.join("host.md")), None).unwrap();
        app.embed_mode = crate::transclude::Mode::Short;
        reader(&mut app);
        let end = last_top(&app);
        let mut seen = Vec::new();
        for top in 1..=end {
            seen.extend(read_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE));
            assert_eq!(app.scroll_hint(), top, "{host:?}: one row per j");
        }
        assert!(drawn(&seen, "frag line 20"), "{host:?}\n{seen:#?}");
        if host.contains("after") {
            assert!(drawn(&seen, "after 2"), "{host:?}");
        }
        feed(&mut app, "gg");
        screen_of(&mut app, READ_W, READ_H);
        assert_eq!(app.scroll_hint(), 0);
        feed(&mut app, "G");
        let screen = screen_of(&mut app, READ_W, READ_H);
        assert_eq!(app.scroll_hint(), end, "{host:?}\n{screen:#?}");
        let seen = read_through(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(app.scroll_hint(), 0);
        assert!(drawn(&seen, "frag line 10"), "{host:?}");
    }
    std::fs::remove_dir_all(&d).ok();
}

/// A wrapped paragraph taller than the pane is scrolled the same way: a
/// row per `j`, so its middle is not skipped.
#[test]
fn read_only_j_goes_row_by_row_through_a_wrapped_paragraph() {
    let words: Vec<String> = (0..150).map(|i| format!("word{i}")).collect();
    let mut app = app_with(&format!("intro\n{}\nafter wrap\n", words.join(" ")));
    reader(&mut app);
    let end = last_top(&app);
    assert!(end > 3);
    let mut seen = Vec::new();
    for top in 1..=end {
        seen.extend(read_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.scroll_hint(), top);
    }
    assert!(drawn(&seen, "word149") && drawn(&seen, "after wrap"));
}

/// Edit mode keeps its line motions: `j` from the line before a tall
/// layout steps onto its opening line, which shows its source.
#[test]
fn edit_mode_j_still_steps_by_line_into_a_tall_layout() {
    let mut app = app_with(&tall_math(3));
    app.term_size = (READ_W, READ_H);
    screen_of(&mut app, READ_W, READ_H);
    feed(&mut app, "jj");
    assert_eq!(app.buffer.cursor.line, 2);
    feed(&mut app, "G");
    assert_eq!(app.buffer.cursor.line, app.buffer.last_line());
}

/// With a final newline, the end is still the last line: `G` puts it on the
/// bottom row, and `j` and `Ctrl-f` there leave the view where it is.
#[test]
fn read_only_end_is_the_last_line_with_a_final_newline() {
    let mut app = app_with(&format!("{}\n", tall_math(3)));
    reader(&mut app);
    let end = last_top(&app);
    assert_eq!(end + 1, app.cache.borrow().total_rows() - app.viewport_height(), "one row for the empty line");
    feed(&mut app, "G");
    let screen = screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.scroll_hint(), end);
    assert!(drawn(&screen, "after 2"), "{screen:#?}");
    for code in [KeyCode::Char('j'), KeyCode::Down] {
        read_key(&mut app, code, KeyModifiers::NONE);
        assert_eq!(app.scroll_hint(), end, "{code:?}");
    }
    read_key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    assert_eq!(app.scroll_hint(), end);
    feed(&mut app, "k");
    screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.scroll_hint(), end - 1, "k moves at once");
}

/// A wrapped line of bold words: every word's `**` is concealed, so a row's
/// first column in the drawn text is not its column in the source.
fn bold_words(n: usize) -> String {
    let words: Vec<String> = (0..n).map(|i| format!("**w{i}**")).collect();
    format!("intro\n{}\nafter\n", words.join(" "))
}

/// The word the view's top row starts with, as drawn.
fn top_word(app: &App) -> String {
    let cache = app.cache.borrow();
    let row = cache.row(app.scroll_hint()).unwrap();
    let entry = cache.entry(row.line()).unwrap();
    let shown: String = entry.cmap.display_text(&entry.source).chars().skip(row.start_col).collect();
    shown.split(' ').next().unwrap().to_string()
}

/// Scrolled onto a later row of a wrapped line with concealed markup, the
/// cursor stands on that row's first visible character in the SOURCE: `i`
/// and `:view` put the caret there, and `yiw` and `*` take its word.
#[test]
fn read_only_cursor_on_a_wrapped_row_is_a_source_column() {
    let text = bold_words(300);
    let source = text.lines().nth(1).unwrap().to_string();
    let onto_row = |app: &mut App| {
        reader(app);
        // Down to the line's second row: the rows before it are the intro's,
        // and line spacing's, if any.
        let second = app.cache.borrow().line_rows(1).0 + 1;
        for _ in 0..second {
            read_key(app, KeyCode::Char('j'), KeyModifiers::NONE);
        }
        assert_eq!(app.scroll_hint(), second);
        assert_eq!(app.cache.borrow().row(second).unwrap().line(), 1);
        let word = top_word(app);
        assert_ne!(word, "w0", "a later row of the line");
        let col = source.find(&format!("**{word}**")).unwrap() + 2;
        (word, col)
    };

    let mut app = app_with(&text);
    let (word, col) = onto_row(&mut app);
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (1, col), "on {word}");
    feed(&mut app, "yiw");
    assert_eq!(app.registers.get(&'"').map(|r| r.text.clone()), Some(word.clone()));
    feed(&mut app, "*");
    assert_eq!(app.search.as_ref().map(|s| s.pattern.clone()), Some(word.clone()));

    let mut app = app_with(&text);
    let (word, col) = onto_row(&mut app);
    feed(&mut app, "ii");
    assert!(matches!(app.mode, Mode::Insert));
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (1, col), "i puts the caret on {word}");

    let mut app = app_with(&text);
    let (word, col) = onto_row(&mut app);
    cmd(&mut app, ":view");
    assert!(!app.view);
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (1, col), ":view leaves it on {word}");
}

/// Widening the pane reflows a paragraph above a tall layout: the view
/// stays on the same row of the layout, not its first.
#[test]
fn read_only_reflow_above_keeps_the_row_in_a_tall_layout() {
    let words: Vec<String> = (0..80).map(|i| format!("word{i}")).collect();
    let text = format!("{}\n{}", words.join(" "), tall_math(12));
    let mut app = app_with(&text);
    app.term_size = (40, READ_H);
    app.set_view(true);
    screen_of(&mut app, 40, READ_H);
    let opener = 3;
    let into = 10;
    let start = app.cache.borrow().line_rows(opener).0;
    for _ in 0..start + into {
        app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.sync_after_input();
        screen_of(&mut app, 40, READ_H);
    }
    assert_eq!(app.scroll_hint(), start + into, "inside the layout");
    let row = layout_rows(&app, opener)[into].clone();
    app.term_size = (120, READ_H);
    let screen = screen_of(&mut app, 120, READ_H);
    let start = app.cache.borrow().line_rows(opener).0;
    assert_eq!(app.scroll_hint(), start + into, "the same row of the layout\n{screen:#?}");
    assert!(screen.iter().find(|r| !r.trim().is_empty()).unwrap().contains(&row), "{row} on top\n{screen:#?}");
    // And a key moves on from there.
    read_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
    app.term_size = (120, READ_H);
    screen_of(&mut app, 120, READ_H);
    assert_eq!(app.scroll_hint(), start + into + 1);
}

/// Holding `j` does not rebuild the row index on input, nor re-parse the
/// document, even with a second, narrower pane on it.
#[test]
fn read_only_motions_reuse_the_focused_row_index() {
    for split in [false, true] {
        let mut app = app_with(&tall_math(30));
        app.term_size = (100, READ_H);
        if split {
            app.split_pane(true);
            ctrl(&mut app, 'w');
            feed(&mut app, ">");
        }
        app.set_view(true);
        screen_of(&mut app, 100, READ_H);
        for _ in 0..5 {
            let (builds, indexes) = {
                let c = app.cache.borrow();
                (c.builds(), c.indexes())
            };
            app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
            app.sync_after_input();
            let c = app.cache.borrow();
            assert_eq!(c.builds(), builds, "split {split}: no re-parse on input");
            assert!(c.indexes() <= indexes + 1, "split {split}: one row index at most, got {}", c.indexes() - indexes);
            drop(c);
            screen_of(&mut app, 100, READ_H);
        }
        assert_eq!(app.scroll_hint(), 5);
    }
}

/// `3G`, and a search whose one match is the opening line, land on the
/// cursor the view keeps for every row of the layout: they still show its
/// first row on top.
#[test]
fn read_only_explicit_jumps_to_the_parked_cursor_show_the_line() {
    let keys: [&dyn Fn(&mut App); 2] = [&|app| feed(app, "3G"), &|app| cmd(app, "/$$ \\begin")];
    for (i, jump) in keys.iter().enumerate() {
        let mut app = app_with(&tall_math(12).replace("$$\n\\begin", "$$ \\begin"));
        reader(&mut app);
        assert_eq!(layout_rows(&app, 2).len(), 20, "laid out");
        feed(&mut app, "jjjjjjj");
        screen_of(&mut app, READ_W, READ_H);
        assert_eq!(app.scroll_hint(), 7);
        let parked = app.buffer.cursor;
        jump(&mut app);
        let screen = screen_of(&mut app, READ_W, READ_H);
        assert_eq!(app.buffer.cursor, parked, "jump {i} lands on the same cursor");
        assert_eq!(app.scroll_hint(), 2, "jump {i}: the layout's first row on top\n{screen:#?}");
    }
}

/// After `G`, a taller pane shows the end with one more row above it, and
/// `k` moves exactly one row from what is drawn.
#[test]
fn read_only_end_is_the_same_for_drawing_and_scrolling() {
    let text: String = (0..40).map(|i| format!("line {i}\n")).collect();
    let mut app = app_with(&text);
    reader(&mut app);
    feed(&mut app, "G");
    screen_of(&mut app, READ_W, READ_H);
    let end = app.scroll_hint();
    app.term_size = (READ_W, READ_H + 1);
    let screen = screen_of(&mut app, READ_W, READ_H + 1);
    assert_eq!(app.scroll_hint(), end - 1, "the last line stays on the bottom row\n{screen:#?}");
    app.on_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
    app.sync_after_input();
    screen_of(&mut app, READ_W, READ_H + 1);
    assert_eq!(app.scroll_hint(), end - 2, "k moves one row");
}

/// A split from inside a tall layout starts the new pane on the same row.
#[test]
fn read_only_split_keeps_the_row_in_a_tall_layout() {
    let mut app = app_with(&tall_math(12));
    reader(&mut app);
    feed(&mut app, "jjjjj");
    screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.scroll_hint(), 5);
    cmd(&mut app, ":vsplit");
    screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.layout.ids().len(), 2);
    assert_eq!(app.scroll_hint(), 5, "the new pane");
}

/// A second `open_unnamed` replaces the block kinds at the same revision and
/// the same settings. The row index has to follow: here the table took no
/// spacing between its rows, and the prose that replaced it does.
#[test]
fn a_second_unnamed_buffer_reindexes_its_rows() {
    let mut app = app_with("");
    app.config.markdown.tables = false;
    app.config.layout.line_spacing = 1;
    app.open_unnamed(Buffer::unnamed(b"| a | b |\n|---|---|\n| 1 | 2 |\n".to_vec()).unwrap());
    let table = screen_of(&mut app, 80, 20);
    let starts: Vec<usize> = (0..3).map(|l| app.cache.borrow().locate(l, 0).0).collect();
    assert_eq!(starts, [0, 1, 2], "no spacing inside a table: {table:#?}");

    app.open_unnamed(Buffer::unnamed(b"alpha\nbravo\ncharlie\n".to_vec()).unwrap());
    let prose = screen_of(&mut app, 80, 20);
    let cache = app.cache.borrow();
    let starts: Vec<usize> = (0..3).map(|l| cache.locate(l, 0).0).collect();
    assert_eq!(starts, [0, 2, 4], "a spacer row after each prose line: {prose:#?}");
    let at = |w: &str| prose.iter().position(|r| r.contains(w)).unwrap_or_else(|| panic!("{w}: {prose:#?}"));
    assert_eq!((at("bravo"), at("charlie")), (at("alpha") + 2, at("alpha") + 4), "{prose:#?}");
}

/// Fifty lines, each indented by its number modulo four, so a line's first
/// non-blank column is not 0.
fn numbered_lines() -> String {
    (1..=50).map(|i| format!("{}line {i}\n", " ".repeat(i % 4))).collect()
}

/// `:N` goes to line N, counted from 1, keeping the column as `NG` does;
/// `:0` and `:1` to the first line, a number past the end and `:$` to the
/// last. Spaces around the number are allowed.
#[test]
fn colon_number_goes_to_that_line() {
    let mut app = app_with(&numbered_lines());
    feed(&mut app, "3l");
    cmd(&mut app, ":30");
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (29, 3));
    assert!(app.flash.as_ref().is_none_or(|f| !matches!(f.kind, FlashKind::Error)), "{:?}", app.flash.as_ref().map(|f| &f.text));
    for (typed, line) in [(":0", 0), (":1", 0), (":999", 49), (":$", 49), (": 7 ", 6), (":30", 29)] {
        cmd(&mut app, typed);
        assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (line, 3), "{typed}");
        assert_eq!(app.mode, Mode::Normal, "{typed}");
    }
    // Far past what a usize holds: still the last line.
    cmd(&mut app, ":99999999999999999999999999");
    assert_eq!(app.buffer.cursor.line, 49);
    // The same place `NG` goes.
    let mut other = app_with(&numbered_lines());
    feed(&mut other, "5l30G");
    let mut app = app_with(&numbered_lines());
    feed(&mut app, "5l");
    cmd(&mut app, ":30");
    assert_eq!(app.buffer.cursor, other.buffer.cursor);
}

/// The lines the column rule was checked on in Neovim (`nvim --clean`,
/// `ts=4 sw=4 et`, as Shoin's defaults): a long indented line, a short
/// one, an empty one, tabs, wide characters.
const STICKY: &str = "    abcdefghijklmno\nab\n\n\tx\tyz\n日本語テキスト\n  indented line here\nlast line of the file\n";

/// Eighty lines: long ones, a short `ab` every seventh and an empty one
/// every tenth, the file the paging and operator cases were checked on.
fn sticky_long() -> String {
    (1..=80)
        .map(|i| match i {
            _ if i % 10 == 0 => "\n".to_string(),
            _ if i % 7 == 0 => "ab\n".to_string(),
            _ => format!("  line {i:02} with some text here\n"),
        })
        .collect()
}

/// A fresh app on `text` with a 30-row window, after `keys`, where the
/// control characters stand for `Ctrl-d` `Ctrl-u` `Ctrl-f` `Ctrl-b` and
/// `Ctrl-r`.
fn sticky_keys(text: &str, keys: &str) -> App {
    let mut app = app_with(text);
    app.term_size = (80, 30);
    for c in keys.chars() {
        match c {
            '\u{4}' => ctrl(&mut app, 'd'),
            '\u{15}' => ctrl(&mut app, 'u'),
            '\u{6}' => ctrl(&mut app, 'f'),
            '\u{2}' => ctrl(&mut app, 'b'),
            '\u{12}' => ctrl(&mut app, 'r'),
            c => feed(&mut app, &c.to_string()),
        }
    }
    app
}

/// Run each `(keys, then a command line, (line, col))` case on a fresh app
/// and list every one that lands elsewhere, so one run shows them all.
fn sticky_misses(text: &str, cases: &[(&str, &str, (usize, usize))]) -> Vec<String> {
    let mut misses = Vec::new();
    for &(keys, typed, want) in cases {
        let mut app = sticky_keys(text, keys);
        if !typed.is_empty() {
            cmd(&mut app, typed);
        }
        let got = (app.buffer.cursor.line, app.buffer.cursor.col);
        if got != want {
            misses.push(format!("{keys:?} {typed}: got {got:?}, Neovim {want:?}"));
        }
    }
    misses
}

/// `G` `gg` `NG` `:N` `:$` `:0` keep the column `j` and `k` keep, as
/// Neovim's default 'nostartofline' does: clamped to a short line, the goal
/// from a longer one coming back, a screen cell across tabs and wide
/// characters. Every expected place is what Neovim gave for the same keys.
#[test]
fn line_jumps_keep_the_column_as_neovim() {
    let misses = sticky_misses(
        STICKY,
        &[
            ("010lG", "", (6, 10)),
            ("7G010lgg", "", (0, 10)),
            ("010l2G", "", (1, 1)),
            ("010l3G", "", (2, 0)),
            ("06l4G", "", (3, 2)),
            ("010l4G", "", (3, 4)),
            ("010l5G", "", (4, 5)),
            // The second tab ends at the stop, cell 8, as in Neovim.
            ("4G03l1G", "", (0, 8)),
            ("4G03l5G", "", (4, 4)),
            ("5G02l1G", "", (0, 4)),
            ("5G02l4G", "", (3, 1)),
            ("010l2G7G", "", (6, 10)),
            ("010lj3Gk", "", (1, 1)),
            ("06l4Gj", "", (4, 3)),
            ("010l", ":7", (6, 10)),
            ("010l", ":$", (6, 10)),
            ("010l", ":0", (0, 10)),
            ("010l", ":2", (1, 1)),
            ("010l2G", ":7", (6, 10)),
            ("7G010l", ":0", (0, 10)),
        ],
    );
    assert!(misses.is_empty(), "{misses:#?}");
}

/// `j`, `k`, `G` and `gg` across a tab in the middle of a line, at tab
/// widths 2, 4 and 8: the tab reaches its stop, so the goal is the cell
/// Neovim draws the char on (`set ts=N`, the same keys). `0f<Tab>` puts
/// the goal on the tab's last cell.
#[test]
fn vertical_motions_count_a_mid_line_tab_to_its_stop_as_neovim() {
    let text = "ab\tcd\nabcdefghij\nx\ty\tz\n";
    let mut misses = Vec::new();
    for (tw, cases) in [
        (2, [("0fcj", (1, 4)), ("0f\tj", (1, 3)), ("j0fhk", (0, 4)), ("j0fhG", (2, 4)), ("G0fzgg", (0, 3))]),
        (4, [("0fcj", (1, 4)), ("0f\tj", (1, 3)), ("j0fhk", (0, 4)), ("j0fhG", (2, 3)), ("G0fzgg", (0, 4))]),
        (8, [("0fcj", (1, 8)), ("0f\tj", (1, 7)), ("j0fhk", (0, 2)), ("j0fhG", (2, 1)), ("G0fzgg", (0, 4))]),
    ] {
        for (keys, want) in cases {
            let mut app = app_with(text);
            app.config.editor.tab_width = tw;
            app.refresh_blocks();
            for c in keys.chars() {
                app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                app.sync_after_input();
            }
            let got = (app.buffer.cursor.line, app.buffer.cursor.col);
            if got != want {
                misses.push(format!("{keys:?} at {tw}: got {got:?}, Neovim {want:?}"));
            }
        }
    }
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// `H` `M` `L` and the paging keys keep the column too, and so does a
/// linewise `d`, `>` or `<`, which then make the column they land on the
/// goal. Lines from Neovim where the window size does not matter.
#[test]
fn screen_paging_and_linewise_operators_keep_the_column_as_neovim() {
    let text = sticky_long();
    let mut misses = sticky_misses(
        &text,
        &[
            ("015l\u{4}\u{15}", "", (0, 15)),
            ("015ldd", "", (0, 15)),
            ("05j15ldd", "", (5, 1)),
            ("05j15lddj", "", (6, 1)),
            ("05j15ljdd", "", (6, 15)),
            ("05j15ljddj", "", (7, 15)),
            ("031j15ldG", "", (30, 15)),
            ("05j15ljVd", "", (6, 15)),
            ("05j15ljVdj", "", (7, 15)),
            ("05j15ljdj", "", (6, 15)),
            ("05j15ljdjk", "", (5, 15)),
            ("015l>>", "", (0, 15)),
            ("015l<<", "", (0, 15)),
            ("05j15lj>>", "", (6, 5)),
            ("05j15lj<<", "", (6, 1)),
            ("05j15lj>>j", "", (7, 5)),
            ("015lVj>", "", (0, 15)),
            ("015lVj<", "", (0, 15)),
            ("05j15lj>k", "", (5, 15)),
            ("05j15ljVk>", "", (5, 15)),
            ("05j15ljvk>", "", (5, 15)),
        ],
    );
    // `H` `M` `L` and the paging keys land on a line the window size and
    // `scroll_off` pick: only the column is Neovim's, 15 unless that line
    // is `ab` or empty. `06j15l` stands on `ab`, so its goal is 1.
    for keys in ["015lH", "05j15ljH", "06j15lH", "015lM", "015lL", "05j15ljL", "05j15lj\u{4}", "015l\u{6}", "015l\u{6}\u{2}", "05j15lj\u{6}\u{2}"] {
        let app = sticky_keys(&text, keys);
        let line = app.buffer.cursor.line;
        let want = match line + 1 {
            n if n % 10 == 0 => 0,
            _ if keys.starts_with("06j") => 1,
            n if n % 7 == 0 => 1,
            _ => 15,
        };
        if app.buffer.cursor.col != want {
            misses.push(format!("{keys:?}: col {} on line {line}, Neovim {want}", app.buffer.cursor.col));
        }
    }
    // A goal from a long line survives `dd` onto another long one, and is
    // the column it landed on after a clamp.
    misses.extend(sticky_misses("ab\n    long line number one here\nshort\n", &[("j15lkddj", "", (1, 4))]));
    assert!(misses.is_empty(), "{misses:#?}");
}

/// A line number with something after it that is no command, or a command
/// that takes no range, is not a command. Offsets and ranges are
/// (`range_tests`).
#[test]
fn colon_number_with_more_after_it_is_not_a_command() {
    for typed in ["3x", "$x", "1,5q", "3 4 e"] {
        let mut app = app_with(&numbered_lines());
        feed(&mut app, "5G");
        cmd(&mut app, &format!(":{typed}"));
        assert_eq!(app.buffer.cursor.line, 4, ":{typed} moved the cursor");
        let flash = app.flash.as_ref().expect(typed);
        assert!(matches!(flash.kind, FlashKind::Error), ":{typed}");
        let said = flash.text.as_deref().unwrap_or("");
        assert!(said.starts_with("not a command"), ":{typed}: {said}");
    }
}

/// `:N` onto a display block's opening line shows its source, as `NG` does.
#[test]
fn colon_number_onto_a_math_block_reveals_it() {
    let mut app = app_with(MATH_DOC);
    cmd(&mut app, ":4");
    assert_eq!(app.buffer.cursor.line, 3);
    assert_eq!((app.active.start, app.active.end), (2, 5));
    cmd(&mut app, ":$");
    assert_eq!(app.active.start, app.active.end, "away again");
}

/// Read-only: `:N` puts its line on top, as `NG` does, even from inside a
/// tall layout whose kept row puts the cursor on that very line.
#[test]
fn read_only_colon_number_puts_its_line_on_top() {
    let mut app = app_with(&tall_math(12));
    reader(&mut app);
    feed(&mut app, "jjjjj");
    screen_of(&mut app, READ_W, READ_H);
    assert_eq!((app.scroll_hint(), app.buffer.cursor.line), (5, 2), "inside the layout, parked on its opener");
    cmd(&mut app, ":3");
    let screen = screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.buffer.cursor.line, 2);
    let top = app.cache.borrow().locate(2, 0).0;
    assert_eq!(app.scroll_hint(), top, "the layout's first row on top\n{screen:#?}");
    let first = layout_rows(&app, 2)[0].clone();
    assert!(screen.iter().find(|r| !r.trim().is_empty()).unwrap().contains(&first), "{screen:#?}");
    // An ordinary line: `after 3` is line 30.
    cmd(&mut app, ":30");
    let screen = screen_of(&mut app, READ_W, READ_H);
    let top = app.cache.borrow().locate(29, 0).0;
    assert_eq!(app.scroll_hint(), top, "{screen:#?}");
    assert!(screen.iter().find(|r| !r.trim().is_empty()).unwrap().contains("after 3"), "{screen:#?}");
    assert!(app.view, "still read-only");
    // `:$` is `G`: the end of the text, the last line on the bottom row.
    cmd(&mut app, ":$");
    screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.scroll_hint(), last_top(&app));
    let mut other = app_with(&tall_math(12));
    reader(&mut other);
    feed(&mut other, "G");
    screen_of(&mut other, READ_W, READ_H);
    assert_eq!(app.buffer.cursor, other.buffer.cursor, "parked where `G` parks it");
}

/// Read-only: `NG` and `:N` show their line from its first wrapped row,
/// as before #176. The sticky column is edit mode's: carried here, the
/// column the view parked the cursor on in the line it left would put a
/// later row of the target line on top.
#[test]
fn read_only_line_jumps_start_at_the_first_wrapped_row() {
    let words = |p: &str| (0..60).map(|i| format!("{p}{i:02}")).collect::<Vec<_>>().join(" ");
    let rest: String = (0..30).map(|i| format!("after {i}\n")).collect();
    let text = format!("{}\n{}\n{rest}", words("a"), words("b"));
    for keys in ["2G", ":2"] {
        let mut app = app_with(&text);
        reader(&mut app);
        feed(&mut app, "jjj");
        screen_of(&mut app, READ_W, READ_H);
        assert!(app.buffer.cursor.col > 0, "{keys}: parked inside the first line");
        if let Some(c) = keys.strip_prefix(':') {
            cmd(&mut app, &format!(":{c}"));
        } else {
            feed(&mut app, keys);
        }
        let screen = screen_of(&mut app, READ_W, READ_H);
        assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (1, 0), "{keys}");
        let top = app.cache.borrow().locate(1, 0).0;
        assert_eq!(app.scroll_hint(), top, "{keys}\n{screen:#?}");
        assert!(screen.iter().find(|r| !r.trim().is_empty()).unwrap().contains("b00"), "{keys}\n{screen:#?}");
    }
}

/// Redo puts the cursor back where the linewise edit left it, not at the
/// start of the range it removed or the end of the indent it added, and
/// undo where the command began: Neovim's answers, each key string run
/// with `u` and then `Ctrl-r` after it.
#[test]
fn undo_and_redo_of_linewise_edits_land_as_neovim() {
    let text = "abcdefghijklmnop\n".repeat(4);
    let mut cases = Vec::new();
    for (keys, undo, redo) in [
        ("08ldd", (0, 8), (0, 8)),
        ("08l2>>", (0, 8), (0, 8)),
        ("08l>>", (0, 8), (0, 8)),
        ("08ldj", (0, 8), (0, 8)),
        ("08ljVkd", (1, 8), (0, 8)),
        ("08ljdG", (1, 8), (0, 8)),
    ] {
        // Undo of `Vkd` is where the command began, line 2: Neovim goes to
        // the top line of the change, 1:8, and that predates the kept column.
        cases.push((format!("{keys}u"), undo));
        cases.push((format!("{keys}u\u{12}"), redo));
    }
    // The goal comes back with the redo: `j` keeps column 8.
    cases.push(("08lddu\u{12}j".to_string(), (1, 8)));
    let cases: Vec<_> = cases.iter().map(|(k, w)| (k.as_str(), "", *w)).collect();
    let misses = sticky_misses(&text, &cases);
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// A goal set with the cursor on a tab is the tab's LAST cell, where
/// Neovim draws the Normal-mode cursor: after `dd` lands on one, after `0`
/// or `h` moves onto one. (`l0`, because a `0` already at column 0 moves
/// nothing in Shoin and keeps the old goal.) Every case is Neovim's answer,
/// the mid-line tab's too: `ab\tx`'s tab ends at its stop, cell 3.
#[test]
fn a_goal_set_on_a_tab_is_its_last_cell_as_neovim() {
    let leading = "abcdefghijk\n\tx\nabcdefghijk\n";
    let mid = "abcdefghijk\nab\tx\nabcdefghijk\n";
    let two = "abcdefghijk\n\t\tx\nabcdefghijk\n";
    let mut misses = Vec::new();
    for (text, cases) in [
        (
            leading,
            &[
                ("02lddj", (1, 3)),
                ("jl0j", (2, 3)),
                ("jl0k", (0, 3)),
                ("jl0dd", (1, 3)),
                ("jfxhj", (2, 3)),
                ("jfxhG", (2, 3)),
                ("jfxhgg", (0, 3)),
                ("jl0vj", (2, 3)),
                ("jfxhjk", (1, 0)),
            ][..],
        ),
        (
            mid,
            &[
                ("02lddj", (1, 3)),
                ("02lddk", (0, 2)),
                ("jfxhj", (2, 3)),
                ("jfxhgg", (0, 3)),
                ("jfxhjk", (1, 2)),
                ("j0j", (2, 0)),
            ][..],
        ),
        (two, &[("jl0j", (2, 3)), ("j0lj", (2, 7)), ("05lddj", (1, 7))][..]),
    ] {
        let cases: Vec<_> = cases.iter().map(|&(k, w)| (k, "", w)).collect();
        misses.extend(sticky_misses(text, &cases));
    }
    assert!(misses.is_empty(), "{}", misses.join("\n"));
    // Insert mode's cursor stands before the tab, and so does its goal:
    // Neovim's `i<Down>` from in front of `\tx` keeps column 0.
    let mut app = sticky_keys(leading, "j0i");
    app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.sync_after_input();
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (2, 0));
}

/// A line jump, `j`/`k` or a linewise delete clamped to a line that ends in
/// a combining mark lands on its base character, as Neovim's do: the mark
/// is drawn in the base's cell. One mark, two, an enclosing one and a
/// zero-width joiner, each after `ab`.
#[test]
fn clamping_never_lands_on_a_combining_mark() {
    let mut misses = Vec::new();
    for mark in ["\u{301}", "\u{301}\u{302}", "\u{20dd}", "\u{200d}"] {
        let text = format!("abcdefghijk\nab{mark}\nabcdefghijk\n");
        let cases = [
            ("09ldd", "", (0, 1)),
            ("09lj", "", (1, 1)),
            ("09ljk", "", (0, 9)),
            ("G09lk", "", (1, 1)),
            ("09lddj", "", (1, 1)),
            ("09l2G", "", (1, 1)),
            ("09l", ":2", (1, 1)),
        ];
        misses.extend(sticky_misses(&text, &cases).into_iter().map(|m| format!("{mark:?} {m}")));
    }
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// Keys, where they leave the cursor, and where a `j` after them goes.
type Landing<'a> = (&'a str, (usize, usize), (usize, usize));

/// Run each `(keys, (line, col), then after one more j)` case on a fresh
/// app and list every one that lands elsewhere: where the keys leave the
/// cursor, and where a `j` after them goes, which shows the goal they left.
fn landing_misses(text: &str, cases: &[Landing]) -> Vec<String> {
    let mut misses = Vec::new();
    for &(keys, want, then) in cases {
        let mut app = sticky_keys(text, keys);
        let got = (app.buffer.cursor.line, app.buffer.cursor.col);
        feed(&mut app, "j");
        let next = (app.buffer.cursor.line, app.buffer.cursor.col);
        if (got, next) != (want, then) {
            misses.push(format!("{keys:?}: got {got:?} then j {next:?}, Neovim {want:?} then {then:?}"));
        }
    }
    misses
}

/// Lines with a first non-blank past column 0, a short one, a line of
/// blanks, an empty one, a leading tab and wide characters.
const CASE_LINES: &str = "Abc Def\n   Indented Line Here With Words\nab\n   \n\nXYZ long line here with some words\n\tTabbed Line Here\n日本 Wide Line Here\nlast Line here\n";

/// A linewise yank leaves the cursor where its range starts, as Neovim's
/// does: where `k` or `gg` put it, at column 0 for `yip` or a Visual
/// selection made downwards, where it was for `yy` `yj` `yG`; and the goal
/// is that column now, so a `j` after `yy` on `ab` forgets the longer
/// line's 15. Every case is Neovim's answer for the same keys.
#[test]
fn linewise_yank_lands_where_its_range_starts_as_neovim() {
    let mut misses = landing_misses(
        &sticky_long(),
        &[
            ("05j15ljyk", (5, 15), (6, 1)),
            ("05j15ljy2k", (4, 15), (5, 15)),
            ("05j15ljygg", (0, 15), (1, 15)),
            ("05j15ljVky", (5, 15), (6, 1)),
            ("05j15ljV2ky", (4, 15), (5, 15)),
            ("05j15ljvkY", (5, 15), (6, 1)),
            ("05j15ljvjY", (6, 0), (7, 0)),
            ("05j15ljVjy", (6, 0), (7, 0)),
            ("05j15ljyj", (6, 1), (7, 1)),
            ("05j15ljyy", (6, 1), (7, 1)),
            ("05j15ljyG", (6, 1), (7, 1)),
            ("05j15lj3yy", (6, 1), (7, 1)),
            ("05j15ljY", (6, 1), (7, 1)),
            ("015l7jyk", (6, 1), (7, 1)),
            ("015l6jyk", (5, 15), (6, 1)),
            ("015l9jyk", (8, 15), (9, 0)),
            ("015ljyk", (0, 15), (1, 15)),
            ("015lyk", (0, 15), (1, 15)),
            ("015lygg", (0, 15), (1, 15)),
            ("015lVy", (0, 0), (1, 0)),
            ("04j15l$yk", (3, 28), (4, 28)),
            ("05j15ljyip", (0, 0), (1, 0)),
        ],
    );
    misses.extend(landing_misses(
        CASE_LINES,
        &[
            ("6j5lyk", (5, 8), (6, 5)),
            ("6j5lykk", (4, 0), (5, 8)),
            ("G5lyk", (7, 3), (8, 5)),
            ("G5lykk", (6, 2), (7, 3)),
            ("6jyk", (5, 0), (6, 0)),
            ("7j3lyk", (6, 2), (7, 3)),
            ("7j3lykk", (5, 5), (6, 2)),
            ("6j5lyy", (6, 5), (7, 6)),
            ("ggyk", (0, 0), (1, 0)),
            ("3j5lyk", (2, 1), (3, 1)),
            ("4j5lyk", (3, 0), (4, 0)),
        ],
    ));
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// `guu` `gUU` and their `gugu` `gUgU` spellings land on the line's first
/// non-blank (its last blank on a line of nothing else) unless the cursor
/// was before it, `3guu` and `gUj` where the cursor was, `gUk` and `gUgg`
/// where the motion put it, `Vgu` `VU` `V~` at column 0 unless the cursor
/// is above the anchor: wherever the range starts, as Neovim's.
#[test]
fn linewise_case_changes_land_where_their_range_starts_as_neovim() {
    let mut misses = landing_misses(
        CASE_LINES,
        &[
            ("j20lguu", (1, 3), (2, 1)),
            ("j20lgUU", (1, 3), (2, 1)),
            ("j20lgugu", (1, 3), (2, 1)),
            ("j20lgUgU", (1, 3), (2, 1)),
            ("j20l3guu", (1, 20), (2, 1)),
            ("j20l2guu", (1, 20), (2, 1)),
            ("jj1l2guu", (2, 1), (3, 1)),
            ("jjjgUU", (3, 0), (4, 0)),
            ("jjj1lgUU", (3, 1), (4, 0)),
            ("jjj2lgUU", (3, 2), (4, 0)),
            ("jjjjgUU", (4, 0), (5, 0)),
            ("jjjjj5lgUU", (5, 0), (6, 0)),
            ("6j5lgUU", (6, 1), (7, 2)),
            ("7j5lgUU", (7, 0), (8, 0)),
            ("7j5lguu", (7, 0), (8, 0)),
            ("ggguu", (0, 0), (1, 0)),
            ("gg5lguu", (0, 0), (1, 0)),
            ("G5lgUk", (7, 3), (8, 5)),
            ("j20lgUk", (0, 6), (1, 6)),
            ("jj1lgUk", (1, 1), (2, 1)),
            ("j20lgUj", (1, 20), (2, 1)),
            ("j20lgUG", (1, 20), (2, 1)),
            ("j20lgUgg", (0, 6), (1, 6)),
            ("j20lgUip", (0, 0), (1, 0)),
            ("j20lVgu", (1, 0), (2, 0)),
            ("j20lVU", (1, 0), (2, 0)),
            ("j20lVjU", (1, 0), (2, 0)),
            ("j20lVju", (1, 0), (2, 0)),
            ("j20lV~", (1, 0), (2, 0)),
            ("jj1lVkU", (1, 1), (2, 1)),
            ("G5lVkU", (7, 3), (8, 5)),
            ("G5lVku", (7, 3), (8, 5)),
            ("G5lVk~", (7, 3), (8, 5)),
        ],
    );
    misses.extend(landing_misses(
        &sticky_long(),
        &[
            ("05j15ljguu", (6, 0), (7, 0)),
            ("05j15lj3guu", (6, 1), (7, 1)),
            ("05j15ljgUk", (5, 15), (6, 1)),
            ("05j15lj2gUk", (4, 15), (5, 15)),
            ("05j15ljgUj", (6, 1), (7, 1)),
            ("05j15ljVku", (5, 15), (6, 1)),
            ("05j15ljVk~", (5, 15), (6, 1)),
        ],
    ));
    assert!(misses.is_empty(), "{}", misses.join("\n"));
    // The lines did change case.
    let app = sticky_keys(CASE_LINES, "j20lgUU");
    assert_eq!(app.buffer.line_text(1), "   INDENTED LINE HERE WITH WORDS");
    let app = sticky_keys(CASE_LINES, "G5lVk~");
    assert_eq!(app.buffer.line_text(7), "日本 wIDE lINE hERE");
}

/// Undo and redo of a linewise case change both put the cursor where the
/// change left it, as Neovim's, from wherever it went since.
#[test]
fn undo_and_redo_of_a_linewise_case_change_land_as_neovim() {
    let cases: Vec<(String, (usize, usize))> = [
        ("j20lguu", (1, 3)),
        ("j20lgUUG", (1, 3)),
        ("j20l3gUU", (1, 20)),
        ("j20lgUk", (0, 6)),
        ("G5lgUk", (7, 3)),
        ("j20lVjU", (1, 0)),
        ("G5lVkU", (7, 3)),
        ("G5lVk~", (7, 3)),
        ("j20lV~", (1, 0)),
        ("7j5lguu", (7, 0)),
    ]
    .iter()
    .flat_map(|&(k, w)| [(format!("{k}u"), w), (format!("{k}u\u{12}"), w)])
    .collect();
    let cases: Vec<_> = cases.iter().map(|(k, w)| (k.as_str(), "", *w)).collect();
    let misses = sticky_misses(CASE_LINES, &cases);
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// `2gUU` on the last line has no second line to take, so Neovim beeps and
/// changes nothing: the cursor stays where it was. A count that runs past
/// the end from higher up takes the lines there are and stays put, as
/// `3guu` does; undo and redo come back to it.
#[test]
fn a_counted_case_change_past_the_end_as_neovim() {
    let text = "first line here\n  second line\n   last words\n";
    let mut misses = landing_misses(
        text,
        &[
            ("G05l2gUU", (2, 5), (2, 5)),
            ("G05l5gUU", (2, 5), (2, 5)),
            ("G05l2gugu", (2, 5), (2, 5)),
            ("G05lgUU", (2, 3), (2, 3)),
            ("j05l3guu", (1, 5), (2, 5)),
            ("j05l3gUU", (1, 5), (2, 5)),
            ("gg05l9gUU", (0, 5), (1, 5)),
        ],
    );
    misses.extend(sticky_misses(
        text,
        &[
            // `2gUU` made no step: `u` undoes the `gUU` before it.
            ("gg5lgUUG05l2gUUgg$u", "", (0, 0)),
            ("j05l3gUUG$u", "", (1, 5)),
            ("j05l3gUUG$uG$\u{12}", "", (1, 5)),
            ("gg05l9gUUG$u", "", (0, 5)),
            ("gg05l9gUUG$uG$\u{12}", "", (0, 5)),
        ],
    ));
    assert!(misses.is_empty(), "{}", misses.join("\n"));
    assert_eq!(sticky_keys(text, "G05l2gUU").buffer.line_text(2), "   last words");
    let app = sticky_keys(text, "gg5lgUUG05l2gUUgg$u");
    assert_eq!((app.buffer.line_text(0), app.buffer.line_text(2)), ("first line here".into(), "   last words".into()));
    assert_eq!(sticky_keys(text, "j05l3gUU").buffer.line_text(2), "   LAST WORDS");
}

/// A case change that changes a line's length (`İ` lowercases to `i` and a
/// combining dot here, `ß` uppercases to `SS`, `ﬁ` to `FI`) lands on the
/// character Neovim's does. Neovim maps `İ` to `i` and `ß` to `ẞ` and
/// keeps the byte column, so `0lguj` on `İİİabc` ends on the third `i`:
/// here the start of that `i`, never its dot. Undo goes back to where the
/// range started in the old text, redo to the landing in the new.
#[test]
fn a_case_change_that_changes_the_length_lands_as_neovim() {
    let text = "İİİabc\nxyz\nİİİabcdef\n";
    let mut misses = landing_misses(
        text,
        &[
            ("0lguj", (0, 4), (1, 2)),
            ("03lguj", (0, 8), (1, 2)),
            ("05lguj", (0, 8), (1, 2)),
            ("j3lguk", (0, 7), (1, 2)),
            ("j2lguk", (0, 7), (1, 2)),
            ("j3lVku", (0, 7), (1, 2)),
            ("j3lVk~", (0, 7), (1, 2)),
            ("0lVjgu", (0, 0), (1, 0)),
        ],
    );
    let text2 = "aßbcde\nxyz\naẞbcde\naﬁbcde\nlast\n";
    misses.extend(landing_misses(
        text2,
        &[
            ("03lgUj", (0, 3), (1, 2)),
            ("05lgUj", (0, 5), (1, 2)),
            ("j3lgUk", (0, 1), (1, 1)),
            ("j4lgUk", (0, 1), (1, 1)),
            ("jj3lguj", (2, 4), (3, 4)),
            ("jj4lguk", (1, 2), (2, 2)),
            ("jjj3lgUj", (3, 4), (4, 3)),
            ("jjj4lgUj", (3, 5), (4, 3)),
            ("jjj3lguj", (3, 3), (4, 3)),
            ("G3lgUk", (3, 4), (4, 3)),
            ("G3lVkU", (3, 4), (4, 3)),
            ("G3lVk~", (3, 4), (4, 3)),
            ("4lVjU", (0, 0), (1, 0)),
        ],
    ));
    let i = "İabc\nxyz\n";
    misses.extend(sticky_misses(
        i,
        &[
            ("0lguj", "", (0, 3)),
            ("0lgujG$u", "", (0, 1)),
            ("0lgujG$uG$\u{12}", "", (0, 3)),
            ("j3lgukG$u", "", (0, 2)),
            ("j3lgukG$uG$\u{12}", "", (0, 4)),
        ],
    ));
    misses.extend(sticky_misses(
        "aßbc\nxyz\n",
        &[
            ("03lgUjG$u", "", (0, 3)),
            ("03lgUjG$uG$\u{12}", "", (0, 3)),
            ("j3lgUkG$u", "", (0, 2)),
            ("j3lgUkG$uG$\u{12}", "", (0, 1)),
        ],
    ));
    misses.extend(sticky_misses(
        "İİİabc\nxyz\n",
        &[("03lgujG$u", "", (0, 3)), ("03lgujG$uG$\u{12}", "", (0, 8))],
    ));
    assert!(misses.is_empty(), "{}", misses.join("\n"));
    // The text is Shoin's full case mapping, as before; `x` takes a letter.
    let app = sticky_keys(i, "0lgujx");
    assert_eq!(app.buffer.line_text(0), "i\u{307}ac");
    assert_eq!(sticky_keys(text2, "03lgUj").buffer.line_text(0), "ASSBCDE");
    assert_eq!(sticky_keys(text2, "G3lgUk").buffer.line_text(3), "AFIBCDE");
}

/// `guu`'s first non-blank skips only spaces and tabs, as Neovim's: a
/// no-break space or an ideographic space is not blank there, though `^`
/// and the indent commands still skip it.
#[test]
fn a_doubled_case_change_skips_only_spaces_and_tabs() {
    let text = "\u{a0}ABC\nxyz long\n  \u{a0} ABC\nxyz long\n  \u{a0}\nxyz long\n\u{3000}ABC\nxyz\n";
    let misses = landing_misses(
        text,
        &[
            ("02lguu", (0, 0), (1, 0)),
            ("02lgUU", (0, 0), (1, 0)),
            ("02lgugu", (0, 0), (1, 0)),
            ("2j05lguu", (2, 2), (3, 2)),
            ("2j0lguu", (2, 1), (3, 1)),
            ("2j03lgUU", (2, 2), (3, 2)),
            ("4j02lguu", (4, 2), (5, 2)),
            ("4j0lguu", (4, 1), (5, 1)),
            ("6j02lguu", (6, 0), (7, 0)),
        ],
    );
    assert!(misses.is_empty(), "{}", misses.join("\n"));
    assert_eq!(sticky_keys(text, "02lguu").buffer.line_text(0), "\u{a0}abc");
}

/// A linewise yank or case change whose range starts on a closed fold
/// lands on the fold's line at column 0, as Neovim's (`zf2j` there, `zc`
/// here); a Visual one does not.
#[test]
fn a_linewise_yank_from_a_closed_fold_lands_at_its_start() {
    let text = "# Top line here with words\nlong line number one here ok\n## Folded heading here\nbody line one is here\nbody line two is here\n## Next heading line here\nlong line after the fold ok\nlast line here\n";
    let p = "jjzcGk015l";
    let cases = [
        ("", (6, 15), (7, 13)),
        ("k", (5, 15), (6, 15)),
        ("kk", (2, 15), (5, 15)),
        ("kyk", (2, 0), (5, 0)),
        ("kgUk", (2, 0), (5, 0)),
        ("kkyy", (2, 0), (5, 0)),
        ("kkyj", (2, 0), (5, 0)),
        ("kkgUU", (2, 0), (5, 0)),
        ("kkY", (2, 0), (5, 0)),
        ("kk2yy", (2, 0), (5, 0)),
        ("kkVy", (2, 0), (5, 0)),
        ("kVky", (2, 15), (5, 15)),
        ("kkVjy", (2, 0), (5, 0)),
        ("kkkyj", (1, 15), (2, 15)),
        ("kkkgUj", (1, 15), (2, 15)),
        ("y2k", (2, 0), (5, 0)),
    ];
    let keys: Vec<String> = cases.iter().map(|(k, _, _)| format!("{p}{k}")).collect();
    let cases: Vec<_> = cases.iter().zip(&keys).map(|(&(_, w, t), k)| (k.as_str(), w, t)).collect();
    let misses = landing_misses(text, &cases);
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}

/// Concealed markup counts by its source: `yk` onto a line with `**` and a
/// link keeps the source column, as Neovim's (which conceals nothing).
#[test]
fn a_linewise_yank_onto_concealed_markup_keeps_the_source_column() {
    let text = "Some **bold** and *italic* words in this line\nanother long line of plain text here\nshort [link](http://x.y/z) here\n";
    let misses = landing_misses(
        text,
        &[
            ("j20lyk", (0, 20), (1, 20)),
            ("j20lgUk", (0, 20), (1, 20)),
            ("G20lyk", (1, 20), (2, 20)),
            ("020lyj", (0, 20), (1, 20)),
        ],
    );
    assert!(misses.is_empty(), "{}", misses.join("\n"));
}
/// Read-only yanks too: `yk` takes the line above with the cursor's and
/// lands on it, `yj` stays, both at the column read-only keeps (0).
#[test]
fn read_only_linewise_yank_lands_where_its_range_starts() {
    let text = sticky_long();
    let mut app = app_with(&text);
    reader(&mut app);
    feed(&mut app, "jjjjj");
    screen_of(&mut app, READ_W, READ_H);
    let line = app.buffer.cursor.line;
    assert!(line > 1, "moved down");
    let lines = |a: usize, b: usize| (a..=b).map(|l| format!("{}\n", app_line(&text, l))).collect::<String>();
    feed(&mut app, "yk");
    screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.registers.get(&'"').map(|r| (r.text.clone(), r.linewise)), Some((lines(line - 1, line), true)));
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (line - 1, 0));
    feed(&mut app, "yj");
    screen_of(&mut app, READ_W, READ_H);
    assert_eq!(app.registers.get(&'"').map(|r| r.text.clone()), Some(lines(line - 1, line)));
    assert_eq!((app.buffer.cursor.line, app.buffer.cursor.col), (line - 1, 0));
    assert!(app.view, "still read-only");
}

fn app_line(text: &str, l: usize) -> &str {
    text.lines().nth(l).unwrap()
}

#[cfg(unix)]
#[test]
fn writing_through_own_file_aliases_is_a_save() {
    let mut app = app_with("one\n");
    let dir = two_files();
    let notes = dir.join("notes.md");
    app.run_command(&format!("w {}", notes.display()));
    let alias = dir.join("alias.md");
    let sym = dir.join("sym.md");
    std::fs::hard_link(&notes, &alias).unwrap();
    std::os::unix::fs::symlink("notes.md", &sym).unwrap();

    for (command, destination, text) in [
        ("w", &alias, "two"),
        ("w", &sym, "three"),
        ("saveas", &alias, "four"),
        ("saveas", &sym, "five"),
    ] {
        feed(&mut app, &format!("cc{text}"));
        esc(&mut app);
        app.run_command(&format!("{command} {}", destination.display()));
        assert!(!app.buffer.modified, "{}", flash_text(&app));
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), format!("{text}\n"));
        app.run_command("w");
        assert!(flash_text(&app).contains("written"), "{}", flash_text(&app));
    }
    assert!(sym.is_symlink());
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(unix)]
#[test]
fn saveas_refuses_a_hard_link_to_another_open_file() {
    let dir = two_files();
    let a = dir.join("one.md");
    let b = dir.join("two.md");
    let alias = dir.join("alias.md");
    std::fs::hard_link(&b, &alias).unwrap();
    let mut app = app_with("");
    assert!(app.open_file(b.clone()));
    assert!(app.open_file(a.clone()));
    let before = std::fs::read(&b).unwrap();
    app.run_command(&format!("saveas! {}", alias.display()));
    assert_eq!(app.buffer.path.as_deref(), Some(a.as_path()));
    assert!(flash_text(&app).contains("another buffer"));
    assert_eq!(std::fs::read(&b).unwrap(), before);
    std::fs::remove_dir_all(dir).unwrap();
}
