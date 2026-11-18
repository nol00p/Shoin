//! Application state and the event loop. SPEC.md §9.
//!
//! Build-order steps 1-3. Key handling here is direct: the operator-pending
//! machine (`input::pending`) and the configurable keymap (`input::keymap`)
//! take over in steps 7 and 9 respectively, at which point `on_key` shrinks to
//! a dispatch into `Action`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::cursor::SetCursorStyle;
use ratatui::crossterm::event::{
    self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::Rect;
use ratatui::DefaultTerminal;

use crate::clipboard::Clipboard;
use crate::config::{Config, ConfigWatcher};
use crate::help::Help;
use crate::finder::{self, Finder};
use crate::tree::{Activate, FileTree};
use crate::input::action::{Action, Operator, Root, Target};
use crate::input::bindings::{Command, Verb};
use crate::input::keymap::{Keymap, Lookup};
use crate::input::mode::{Mode, Prompt, PromptKind};
use crate::input::pending::{Key, Pending, Resolution, Table};
use crate::render::cache::RenderCache;
use crate::render::conceal::ActiveSet;
use crate::render::align::TextAlign;
use crate::render::focus::{FocusMode, FocusRegion};
use crate::render::numbers::NumberMode;
use crate::render::pane::{Dir, Node, Pane, PaneId};
use crate::render::frame;
use crate::render::markdown::block::{leading_ws_cols, BlockCache, BlockKind, Marker};
use crate::render::markdown::inline::{self, Inline};
use crate::render::theme::Theme;
use crate::fs::ops;
use crate::export::Format;
use crate::text::buffer::Buffer;
use crate::text::cursor::Cursor;
use crate::text::motion::{self, Motion};
use crate::text::list::{self, Lists, RunError};
use crate::text::object;
use crate::text::table::{self, TableAlign};
use crate::transclude::{compile, link};

const TICK: Duration = Duration::from_millis(100);

/// How long one iteration may spend absorbing input before it has to draw.
///
/// A frame costs O(lines): the render cache verifies every entry against its
/// source line whenever the buffer revision moves, and the row index is rebuilt
/// across the whole document. That is the right trade for ONE edit — but it is
/// paid per edit, and input does not arrive one event at a time. A paste is
/// thousands of key events already sitting in the queue, and drawing between
/// each of them made a 10 KB paste into a 5 000-line document a minute of dead
/// terminal. Draining what has already arrived and drawing the result once
/// turns O(events × lines) back into O(lines).
///
/// The budget is what keeps a very large paste from going dark: whatever is
/// still queued when it runs out waits for the next iteration, so the screen
/// keeps up through an arbitrarily long one.
const BATCH_BUDGET: Duration = Duration::from_millis(16);

/// A hard ceiling on one batch, alongside the budget rather than instead of it.
/// The budget is wall-clock and so cannot be reasoned about; this is the bound
/// that says a batch ENDS, whatever the clock did — a suspended process whose
/// `Instant` barely moved still has to give the screen back.
const BATCH_MAX: usize = 4096;

/// How often the open documents' mtimes are checked for `[editor] autoreload`.
///
/// One `stat` per named document per second. The check is also run the moment
/// the terminal regains focus, which is when it actually matters — you looked
/// away, something else wrote the file, you looked back. This poll is what
/// covers the rest: a terminal that does not report focus, and a file written
/// by something in the same terminal that never took focus away.
const DISK_POLL: Duration = Duration::from_secs(1);

/// How long a file must have been left alone before it is read.
///
/// A writer that is not atomic — a `>` redirect, a slow `scp`, `git checkout`
/// of a large file — can be observed MID-WRITE: truncated, or not yet valid
/// UTF-8. Invalid UTF-8 is refused by `fs::open::load` and simply retried, but
/// a truncated read that happens to be valid UTF-8 would load as real content.
/// Requiring the mtime to be this old first means we only ever read a file that
/// has settled, which costs up to one `DISK_POLL` of latency and removes the
/// whole class.
const DISK_SETTLE: Duration = Duration::from_millis(250);

/// Cells a `<C-w>>` / `<C-w>+` press moves a pane edge. Wider than vim's single
/// cell: a centered text measure is worth adjusting in visible steps.
const WIDTH_STEP: i32 = 4;
const HEIGHT_STEP: i32 = 2;

/// Past this many entries a deletion count stops being informative, and
/// walking further to find the real number costs more than it tells.
const DELETE_COUNT_CAP: usize = 999;

/// Narrowest `:set measure` will accept. `Layout::compute` clamps to the
/// terminal anyway, so this is about refusing a value that is not a measure at
/// all rather than about safety.
const MIN_MEASURE: u16 = 8;


pub struct Flash {
    /// `None` means "show the default status content", not "show nothing".
    pub text: Option<String>,
    pub kind: FlashKind,
    pub expires_at: Instant,
}

pub enum FlashKind {
    Info,
    Error,
}

/// One open document: everything that belongs to the FILE rather than to the
/// editor. `App` derefs to the current one, so every `self.buffer` in this file
/// reads the document you are actually looking at.
pub struct BufferState {
    pub buffer: Buffer,

    /// Per-line block classification, parallel to the buffer. Rebuilt when the
    /// buffer revision changes; `block::invalidate_from` makes that
    /// incremental. SPEC.md §5.2.
    pub blocks: BlockCache,

    /// Per-line parse + wrap cache. Behind a `RefCell` because rendering takes
    /// `&App` — the render path stays read-only in every sense that matters,
    /// and its one piece of memory does not have to travel up through every
    /// caller of `frame::render`.
    ///
    /// Per DOCUMENT, so switching buffers does not throw away the parse of the
    /// one you just left.
    pub cache: RefCell<RenderCache>,
}

impl BufferState {
    fn new(buffer: Buffer, math: bool) -> BufferState {
        BufferState {
            blocks: BlockCache::build(&buffer, math),
            buffer,
            cache: RefCell::new(RenderCache::default()),
        }
    }

    /// The name shown in the buffer list and the switcher.
    pub fn name(&self) -> String {
        self.buffer.display_name()
    }
}

pub struct App {
    /// Every open document, in the order they were opened. `App` derefs to the
    /// one the FOCUSED PANE is showing.
    pub docs: Vec<BufferState>,

    /// The document `<C-^>` goes back to — the one the focused pane was showing
    /// before its current one.
    ///
    /// An INDEX, not a path, because an unnamed buffer has neither; the index
    /// is fixed up in `close_buffer` the same way the panes' are, which is the
    /// one place `docs` ever shrinks.
    pub alternate: Option<usize>,

    /// The window layout: a tree of splits whose leaves are panes, each a view
    /// onto a document. One leaf until you split.
    ///
    /// The FILE TREE is deliberately not a leaf of this tree: it is chrome, not
    /// a view onto a document, so it takes its width from the frame before the
    /// panes divide what is left. Making it a leaf would mean every split
    /// computation had to special-case a node that holds no buffer.
    pub layout: Node,
    /// Which pane has the cursor.
    pub focus_pane: PaneId,
    /// Hands out pane ids; never reused, so focus cannot land on a stale one.
    next_pane_id: PaneId,

    pub config: Config,
    pub mode: Mode,

    /// Resolved colors. Built-in default until config theming lands (step 9).
    pub theme: Theme,

    /// Lines rendered raw this frame. Recomputed on every cursor move; a CHANGE
    /// in it dirties the conceal cache even with no edit. SPEC.md §9.
    pub active: ActiveSet,

    pub flash: Option<Flash>,
    pub quit: bool,

    /// Last cursor shape pushed to the terminal, so we only re-emit the escape
    /// on an actual mode change rather than every frame.
    cursor_shape: Option<CursorShape>,

    /// Whether a non-default cursor color is currently set (Command mode paints
    /// the caret blue). Tracked so we only emit the escape on change.
    cursor_colored: bool,

    // --- input (SPEC.md §7.2) ---
    //
    // The whole Normal/Visual grammar — counts, operators, objects, registers,
    // multi-key sequences — lives in `input::pending`, behind the keymap. `App`
    // only ever sees a resolved `Command`, which is what keeps the key handling
    // from growing here every time a panel or a mode is added.
    pending: Pending,
    /// What the start screen draws — resolved from `[splash] art` at startup
    /// and on reload, never per frame.
    pub splash_art: crate::render::splash::Chosen,
    /// Whether discovery found a config at all. Only the start screen reads it,
    /// to offer `--init-config` to a reader who has none — see
    /// `config::init` for why the editor does not just write one.
    pub configured: bool,
    /// Read-only mode (`shoin -R`, `:view`): every line concealed, no caret,
    /// edits refused, `q` quits. See [`App::set_view`].
    pub view: bool,
    /// An edit was just refused in read-only mode, so an `i` now means "let
    /// me edit" rather than being refused again.
    edit_offered: bool,
    /// How much of an `![[…]]` line expands in place. Starts at
    /// `transclude.embed`; `:embed [mode]` changes it.
    pub embed_mode: crate::transclude::Mode,
    /// The mode a bare `:embed` turns back on, so toggling off and on again
    /// returns to `rec` rather than dropping to `short`.
    last_embed_mode: crate::transclude::Mode,
    /// Bumped whenever a note on disk may have changed under an embed: a
    /// write, a file created, moved or deleted, a reload, or arriving at a
    /// document. An embed's expansion is read from DISK, not from the line, so
    /// the render cache cannot see such a change in the text it keys on; it
    /// compares this instead and re-reads its `![[…]]` lines when it moves.
    pub disk_epoch: u64,
    /// How much of `input.escape_alias` has just been typed, and when the last
    /// of it arrived. Insert mode only; see `feed_escape_alias`.
    escape_run: usize,
    escape_since: Option<Instant>,
    /// The last `f`/`F`/`t`/`T` target, for `;` (repeat) and `,` (reverse):
    /// `(char, forward, till)`.
    last_find: Option<(char, bool, bool)>,

    /// The registers, by name: `"` unnamed, `0` the yank register, `1`-`9` the
    /// delete ring, `a`-`z` named. SPEC.md §7.2.
    registers: HashMap<char, Register>,
    /// Register named by a `"x` prefix, consumed by the next store or paste.
    pending_register: Option<char>,
    /// What `"+` and `"*` reach — and every unnamed yank and paste when
    /// `[editor] clipboard` is on. Never the real one under `cargo test`.
    clipboard: Box<dyn Clipboard>,
    /// Visual-mode selection anchor; `None` outside Visual/VisualLine.
    pub anchor: Option<Cursor>,
    /// Where the left button went down, while a drag out of it is still live
    /// and started in a text pane. `None` the rest of the time — including for
    /// a press in the file tree, which is how tree drags stay inert.
    drag_from: Option<Cursor>,
    /// Whether leaving Visual should resume the insert session it came out of
    /// rather than land in Normal — vim's `-- (insert) VISUAL --`, reached by
    /// dragging the mouse while inserting. Only ever true WHILE in Visual:
    /// `enter_visual` clears it and `end_visual` consumes it, so it cannot go
    /// stale and drop a later `v` into Insert.
    visual_resumes_insert: bool,
    /// The pane and document a Visual selection was made in. A selection
    /// belongs to them: once focus is anywhere else (another pane, another
    /// document, the file tree), `sync_after_input` ends it, so `d` can never
    /// act on text nobody selected.
    visual_owner: Option<(PaneId, usize)>,

    /// Live config-file watcher; `None` when running on built-in defaults.
    watcher: Option<ConfigWatcher>,

    /// Focus mode and its cached bright region. SPEC.md §6.
    pub focus: FocusMode,
    pub focus_region: Option<FocusRegion>,

    /// The line-number gutter. Starts at `layout.numbers`; `:number [mode]`
    /// changes it.
    pub numbers: NumberMode,
    /// The mode a bare `:number` turns back on, so toggling off and on again
    /// returns to whichever mode was last chosen rather than defaulting to
    /// absolute.
    last_numbers: NumberMode,

    /// How each row sits within the (always-centered, per `layout.align`)
    /// writing column. Starts at `layout.text_align`; `:align [mode]` changes
    /// it live, and cycles left → center → right → justified with no
    /// argument.
    pub text_align: TextAlign,

    /// The last executed search; drives `n`/`N` and match highlighting.
    pub search: Option<Search>,

    // --- `.` repeat (SPEC.md §7.3) ---
    /// Input of the last change, replayed by `.`.
    dot: Vec<Replay>,
    /// Input of the command currently in flight.
    recording: Vec<Replay>,
    /// True while `on_key` runs. A key that takes focus away from a Visual
    /// selection is dealt with by `on_key` itself, after the key; only when
    /// no key is under way (a mouse click) does `sync_after_input` end it.
    in_key: bool,
    /// Buffer revision when the in-flight command began, to tell if it edited.
    dot_rev: u64,
    /// True while replaying `dot`, so the replay is not itself recorded.
    replaying: bool,
    /// A counted `3i`/`2o`: how many copies, whether each copy is a new
    /// line, and the keys typed so far. `leave_insert` replays them
    /// `count - 1` more times, as vim does.
    insert_repeat: Option<InsertRepeat>,
    /// True while `repeat_insert` replays: the escape alias stays out of it.
    replaying_insert: bool,
    /// The line `cc` left its indent on, so an `Esc` with nothing typed takes
    /// the unused indent back (vim's 'autoindent').
    cc_indent_line: Option<usize>,

    /// User `[keys.*]` bindings overlaid on the built-in grammar.
    keymap: Keymap,

    /// Which terminal graphics protocol this session can use, read once at
    /// startup — the environment does not change under a running editor.
    pub image_protocol: crate::image::Protocol,
    /// Where the last frame put its pictures. Filled while the frame is built
    /// (hence the cell: rendering only has `&App`) and drained by the painter
    /// straight after.
    pub images: std::cell::RefCell<Vec<crate::render::Placement>>,
    /// What the painter last actually sent, and the terminal size it sent it
    /// at. A picture already on screen in the right place must not be sent
    /// again — that is the difference between typing and typing through
    /// treacle.
    painted: std::cell::RefCell<(Vec<crate::render::Placement>, (u16, u16))>,
    /// The spotlight box's rectangle, when one is open.
    ///
    /// ratatui draws the overlay over the document, but a picture is painted
    /// AFTER ratatui and a kitty image sits above the text besides — so an
    /// overlay would be behind the very thing it was summoned over. Recorded
    /// while the frame is built, and used to withhold any picture it covers.
    pub overlay: std::cell::Cell<Option<ratatui::layout::Rect>>,

    /// `<C-w>` typed inside the file tree, which runs its own key handling.
    tree_window_prefix: bool,
    /// The count on the `<C-w>` command in flight, so `5<C-w>>` moves five
    /// steps. Only meaningful for the length of `window_command`.
    window_count: usize,

    /// Last known terminal size, for mapping mouse clicks to buffer positions.
    term_size: (u16, u16),

    /// The `:help` overlay, when open. Intercepts input and rendering.
    pub help: Option<Help>,

    /// The file-tree pane, when open (left side). SPEC.md IDEAS — Neo-tree style.
    pub tree: Option<FileTree>,

    /// The `:diff` conflict view, when open. An overlay in the shape of `help`
    /// and `finder`: it draws the whole surface itself and owns all input.
    pub diff: Option<crate::diff::DiffView>,

    /// The fuzzy file finder, when open. An overlay, not a pane: it takes all
    /// input while it is up and owns no part of the editor's coordinate space.
    pub finder: Option<Finder>,

    /// When the last character was typed in Insert mode, for undo coalescing.
    last_insert: Option<Instant>,

    /// Chrome hidden by `--zen` or `:zen`, with the settings it overrode. Held
    /// on `App` rather than in `Config` because a hot reload re-reads the file
    /// and would otherwise forget a choice the user made at the command line.
    zen: Option<ZenState>,

    /// Whether the screen is out of date. SPEC.md §9: draw only when something
    /// changed. Every state change in this program originates from an event, so
    /// `step` raises this for whatever it handled and the loop lowers it after
    /// painting — an idle editor does no work between ticks.
    needs_redraw: bool,

    /// When the open documents' mtimes are next checked. Unlike `autosave_at`
    /// this is never `None`: the poll is cheap enough to leave running, and
    /// `check_disk` is the one place that asks whether the setting is on.
    disk_check_at: Instant,

    /// When the next autosave is due, or `None` when `[editor] autosave` is off.
    ///
    /// A DEADLINE rather than an elapsed count, so the loop's question is one
    /// `Instant` comparison it can afford every tick. Re-armed after every
    /// write, the manual ones included: a timer that fires two seconds after
    /// `:w` writes a file nobody changed since.
    autosave_at: Option<Instant>,
}

/// `App` derefs to the document you are editing. That is what lets every verb
/// in this file say `self.buffer` and mean "the current one" — the alternative
/// was threading an index through 290 call sites for no gain in clarity.
impl std::ops::Deref for App {
    type Target = BufferState;

    fn deref(&self) -> &BufferState {
        &self.docs[self.current()]
    }
}

impl std::ops::DerefMut for App {
    fn deref_mut(&mut self) -> &mut BufferState {
        let i = self.current();
        &mut self.docs[i]
    }
}

/// The chrome settings zen mode is hiding, so leaving it can put them back.
#[derive(Clone, Copy)]
struct ZenState {
    status: bool,
    scroll_hint: bool,
}

/// The unnamed register. `linewise` distinguishes `dd`/`yy` (whole lines) from
/// charwise deletes/yanks, so paste knows whether to open a new line.
#[derive(Clone)]
struct Register {
    text: String,
    linewise: bool,
}

/// One piece of recorded input for `.`. A bracketed paste is its own entry
/// rather than the keys it would have taken to type: replayed as keys it would
/// go back through auto-indent, list continuation and the escape alias, and
/// come out different from what was pasted.
#[derive(Clone)]
enum Replay {
    Key(KeyEvent),
    Paste(String),
}

/// The last executed search, for `n`/`N` repeats and match highlighting.
#[derive(Clone)]
pub struct Search {
    pub pattern: String,
    /// The original direction (`/` vs `?`); `n` follows it, `N` reverses it.
    pub reverse: bool,
}

// Search runs per keypress (`/`, `n`, `N`), so it reads the rope a line at a
// time with `str::find` rather than copying the document into a `Vec<char>`
// and comparing at every position. Positions are CHAR indexes into the rope;
// bytes are only ever an offset inside one line's text.

/// The byte offset of char `col` in `s`, or `s.len()` past its end.
fn byte_of(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map_or(s.len(), |(b, _)| b)
}

/// First char index at or after `from` where `needle` starts in `hay`.
fn str_find(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let at = byte_of(hay, from);
    let found = hay[at..].find(needle)?;
    Some(from + hay[at..at + found].chars().count())
}

/// Last char index at or before `before` where `needle` starts in `hay`. The
/// match may run past `before`, so the text searched ends a needle's length
/// after it: no match starting later fits in that.
fn str_rfind(hay: &str, needle: &str, before: usize) -> Option<usize> {
    let end = byte_of(hay, before.saturating_add(needle.chars().count()));
    let found = hay[..end].rfind(needle)?;
    Some(hay[..found].chars().count())
}

/// One line's text: borrowed when the rope holds it in one chunk, as it does
/// for all but the rare line that straddles two.
fn line_str(rope: &ropey::Rope, line: usize) -> std::borrow::Cow<'_, str> {
    let slice = rope.line(line);
    match slice.as_str() {
        Some(s) => std::borrow::Cow::Borrowed(s),
        None => std::borrow::Cow::Owned(slice.to_string()),
    }
}

/// First char index at or after `from` where `needle` occurs in `rope`.
fn rope_find(rope: &ropey::Rope, needle: &str, from: usize) -> Option<usize> {
    if needle.is_empty() || from >= rope.len_chars() {
        return None;
    }
    // Only `\n` ends a rope line here (ropey is built without `unicode_lines`
    // and `cr_lines`), so a needle without one lies inside a single line. One
    // with it — it would have to be pasted into the search line — is looked
    // for in the whole text.
    if needle.contains('\n') {
        return str_find(&rope.to_string(), needle, from);
    }
    let first = rope.char_to_line(from);
    (first..rope.len_lines()).find_map(|line| {
        let base = rope.line_to_char(line);
        str_find(&line_str(rope, line), needle, from.max(base) - base).map(|i| base + i)
    })
}

/// Last char index at or before `before` where `needle` occurs in `rope`.
fn rope_rfind(rope: &ropey::Rope, needle: &str, before: usize) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    let before = before.min(rope.len_chars());
    if needle.contains('\n') {
        return str_rfind(&rope.to_string(), needle, before);
    }
    let last = rope.char_to_line(before);
    (0..=last).rev().find_map(|line| {
        let base = rope.line_to_char(line);
        let limit = if line == last { before - base } else { usize::MAX };
        str_rfind(&line_str(rope, line), needle, limit).map(|i| base + i)
    })
}

/// An emphasis span the bold and italic toggles can strip, in char indexes
/// into the rope (`inline::InlineSpan` counts from the start of its line).
struct EmphasisSpan {
    kind: Inline,
    outer: Range<usize>,
    inner: Range<usize>,
}

/// Where the link under the cursor points.
///
/// Three cases because there are three ways to get there, not three syntaxes:
/// a name the vault resolves, a path the document spells out, and an address
/// only the desktop can reach. `[[note]]`, `![[note]]` and `[text](note.md)`
/// all land in the first two.
enum Dest {
    /// A `[[…]]` target — a NAME, which `transclude::link` turns into a file.
    Note(link::Link),
    /// A path written out in a `[text](path)`, relative to the edited file.
    Path { path: PathBuf, section: link::Section },
    /// A URL. Nothing in the editor can open it.
    Url(String),
}

/// A recorded change with its count replaced by `n`: `"a3dw` → `"a5dw` for
/// `5.`, `d2w` → `5dw`, `A;…` → `5A;…`. A count is the digits after an
/// optional register, the first of them not `0` (a leading `0` is a
/// motion), and the digits straight after an operator. A Visual change or a
/// paste is replayed as it was: a count there would land on the `v`.
fn with_count(keys: &[Replay], n: usize) -> Vec<Replay> {
    let plain = |r: &Replay| match r {
        Replay::Key(k) if k.modifiers.difference(KeyModifiers::SHIFT).is_empty() => match k.code {
            KeyCode::Char(c) => Some(c),
            _ => None,
        },
        _ => None,
    };
    let at = if keys.first().and_then(plain) == Some('"') { 2.min(keys.len()) } else { 0 };
    // Where a run of count digits starting at `from` ends.
    let digits_end = |from: usize| {
        let mut end = from;
        while let Some(c) = keys.get(end).and_then(plain) {
            let digit = if end == from { ('1'..='9').contains(&c) } else { c.is_ascii_digit() };
            if !digit {
                break;
            }
            end += 1;
        }
        end
    };
    let end = digits_end(at);
    match keys.get(end) {
        Some(Replay::Paste(_)) | None => return keys.to_vec(),
        Some(k) if matches!(plain(k), Some('v' | 'V')) => return keys.to_vec(),
        _ => {}
    }
    let mut out: Vec<Replay> = keys[..at].to_vec();
    let n = n.to_string();
    out.extend(n.chars().map(|c| Replay::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))));
    // An operator's own count goes too: `d2w` then `3.` is three words.
    let op_len = match (keys.get(end).and_then(plain), keys.get(end + 1).and_then(plain)) {
        (Some('d' | 'c' | 'y' | '<' | '>'), _) => 1,
        (Some('g'), Some('u' | 'U')) => 2,
        _ => 0,
    };
    out.extend(keys[end..end + op_len].iter().cloned());
    out.extend(keys[digits_end(end + op_len)..].iter().cloned());
    out
}

/// See `App::insert_repeat`.
#[derive(Clone, Debug)]
struct InsertRepeat {
    count: usize,
    line: bool,
    keys: Vec<KeyEvent>,
    /// Where the session began (a char index) and the document's length
    /// then: what the first copy produced is the span from `start` as long
    /// as the document grew, and each later copy goes after the one before,
    /// past any closer auto-pair left behind the cursor.
    start: usize,
    len_before: usize,
}

/// The most keys a counted insert replays in all: past it the count is a
/// typo, as `PASTE_LIMIT` reads a counted paste.
const INSERT_REPEAT_LIMIT: usize = 1 << 20;

/// What a key did to a one-line text field: the `:` line, the `/` `?`
/// search line, or a panel's prompt.
#[derive(Debug, PartialEq, Eq)]
enum FieldKey {
    /// `Esc`, `Ctrl-C`, or a backspace with nothing left to delete.
    Cancel,
    /// `Enter`.
    Submit,
    /// The text changed, or the key was ignored: stay in the field.
    Stay,
}

/// The keys every one-line field shares, applied to `text`. A control or
/// Alt chord is never typed as its letter: `Ctrl-U` clears, `Ctrl-W` (and
/// `Alt-`/`Ctrl-Backspace`) deletes the last word, `Ctrl-H` is a backspace,
/// and the rest are ignored, as Insert mode ignores them. The finder's query
/// takes the same keys (`on_key_finder`).
fn field_key(key: KeyEvent, text: &mut String) -> FieldKey {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    // A terminal reporting alternate keys sends Ctrl-Shift-W as `W` with
    // Control: the same chord as Ctrl-W here.
    let code = match key.code {
        KeyCode::Char(c) if ctrl => KeyCode::Char(c.to_ascii_lowercase()),
        code => code,
    };
    match code {
        KeyCode::Esc => FieldKey::Cancel,
        KeyCode::Backspace if ctrl || alt => {
            delete_last_word(text);
            FieldKey::Stay
        }
        KeyCode::Char('c') | KeyCode::Char('[') if ctrl => FieldKey::Cancel,
        KeyCode::Enter => FieldKey::Submit,
        KeyCode::Backspace => backspace_field(text),
        KeyCode::Char('h') if ctrl => backspace_field(text),
        KeyCode::Char('u') if ctrl => {
            text.clear();
            FieldKey::Stay
        }
        KeyCode::Char('w') if ctrl => {
            delete_last_word(text);
            FieldKey::Stay
        }
        KeyCode::Char(c) if !ctrl && !alt => {
            text.push(c);
            FieldKey::Stay
        }
        _ => FieldKey::Stay,
    }
}

/// Backspace on an empty field closes it, as in vim's `:` line.
fn backspace_field(text: &mut String) -> FieldKey {
    match text.pop() {
        Some(_) => FieldKey::Stay,
        None => FieldKey::Cancel,
    }
}

/// `Ctrl-W` in a text field, as vim's command line has it: the spaces before
/// the end, then a run of word characters or a run of other symbols.
fn delete_last_word(text: &mut String) {
    let trimmed = text.trim_end_matches(char::is_whitespace).len();
    text.truncate(trimmed);
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let Some(last) = text.chars().next_back() else { return };
    let keep = if word(last) {
        text.trim_end_matches(word).len()
    } else {
        text.trim_end_matches(|c: char| !word(c) && !c.is_whitespace()).len()
    };
    text.truncate(keep);
}

/// Destinations `gx` will hand to the desktop, and the only ones `[text](…)`
/// reads as a URL rather than as a path.
///
/// An allowlist rather than a general scheme parser, because the ambiguous
/// cases all resolve the wrong way: `C:\notes\x.md` and `2:30 plan.md` both
/// look like schemes, and a Windows drive letter reaching the system opener
/// would be a surprise with no way to ask for the file instead.
const URL_SCHEMES: &[&str] = &["http://", "https://", "mailto:"];

fn is_url(s: &str) -> bool {
    URL_SCHEMES
        .iter()
        .any(|p| s.get(..p.len()).is_some_and(|head| head.eq_ignore_ascii_case(p)))
}

/// The most text a counted `p` may put in (16 MB): a register times its
/// count. Past it the count is a typo, not a request.
const PASTE_LIMIT: usize = 16 << 20;

/// Extensions a dead link may CREATE. A missing note is a note to write; a
/// missing `report.pdf` is a missing file, and touching an empty one in its
/// place would answer a question nobody asked.
const CREATABLE: &[&str] = &["md", "markdown", "txt"];

/// Common file types that are not notes. In a wiki link's NAME these still
/// read as an extension, so `[[report.pdf]]` is a missing PDF (refused), not
/// a note called `report.pdf` (which would create `report.pdf.md`). Kept to
/// documents, data, media and archives: a name like `[[Dr.Who]]` must not
/// stop being a note because its tail happens to be a file type.
const FILE_TYPES: &[&str] = &[
    "pdf", "doc", "docx", "odt", "rtf", "epub", "xls", "xlsx", "ods", "csv", "ppt",
    "pptx", "odp", "json", "xml", "html", "htm", "zip", "gz", "tgz", "tar", "7z", "rar",
    "mp3", "wav", "flac", "ogg", "m4a", "mp4", "mov", "mkv", "avi", "webm",
];

/// The extension a dead link's target names, lowercased, or `None` when its
/// last dot is part of the name.
///
/// A wiki link's target is a NAME (`name` true): only a linkable extension (a
/// note's or a picture's, the ones `link::resolve` reads) or a common file
/// type (`FILE_TYPES`) is an extension, so `[[2024.01.16 meeting]]` and
/// `[[Dr.Who]]` are notes to write, exactly the files the link resolves to
/// once they exist, while `[[report.pdf]]` is a PDF. A markdown link's target
/// is a PATH, and any extension it has is the file's type.
fn named_ext(path: &Path, name: bool) -> Option<String> {
    let e = path.extension()?.to_string_lossy().to_lowercase();
    let known = CREATABLE.contains(&e.as_str())
        || crate::image::IMAGE_EXTENSIONS.contains(&e.as_str())
        || FILE_TYPES.contains(&e.as_str());
    (known || !name).then_some(e)
}

/// A link target with no extension means a note — the same default
/// `link::candidates` resolves with, so what `gf` creates is what the link
/// finds next time. `.md` goes on the WHOLE name: `2024.01.16 meeting` is
/// `2024.01.16 meeting.md`, not `2024.01.md`.
fn with_default_ext(path: PathBuf, name: bool) -> PathBuf {
    if named_ext(&path, name).is_some() {
        return path;
    }
    match path.file_name() {
        Some(name) => {
            let mut name = name.to_os_string();
            name.push(".md");
            path.with_file_name(name)
        }
        None => path,
    }
}

fn creatable(path: &Path, name: bool) -> bool {
    match named_ext(path, name) {
        None => true,
        Some(e) => CREATABLE.contains(&e.as_str()),
    }
}

/// A typed path with `~` or `~/…` at its start read as `$HOME`, as a shell
/// reads it, for `:e`, `:w`, `:saveas`, the export prompt and the tree's
/// prompts. `~user` and a `~` anywhere else are left as typed, and so is
/// everything when there is no `$HOME`.
fn expand_home(typed: &str) -> PathBuf {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty());
    match (typed.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(home).join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(typed),
    }
}

/// A buffer path in the one form every buffer keeps: absolute (joined onto
/// `base`, the working directory, when relative) with `.` dropped and `..`
/// resolved lexically. Nothing has to exist — a new file has a path too.
///
/// The one exception is `..` after a symlink, where the text and the
/// filesystem disagree (`link/..` is the parent of where `link` points, not
/// the folder `link` sits in): there the path so far is resolved first, so
/// the file opened is the one the shell would open.
///
/// One form is what makes `n.md`, `./n.md` and the tree's `/home/…/n.md`
/// one buffer, and what lets a tree rename find the buffer it moves.
fn clean_path(base: &Path, path: &Path) -> PathBuf {
    use std::path::Component;
    let joined = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            // `/..` is `/`: `pop` leaves the root where it is.
            Component::ParentDir => {
                let link = std::fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink());
                if let Some(real) = link.then(|| std::fs::canonicalize(&out).ok()).flatten() {
                    out = real;
                }
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// How a section reads in a message: as the reader wrote it after the `#`.
fn section_label(section: &link::Section) -> String {
    match section {
        link::Section::All => String::new(),
        link::Section::Heading(h) => h.clone(),
        link::Section::Block(b) => format!("^{b}"),
    }
}

/// The terminal caret shape per mode. A visible modal cue: block in Normal, bar
/// while inserting or typing a command, underline in Visual by default — each
/// overridable via `[cursor]`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
}

impl CursorShape {
    /// `None` for a shape this does not know. Every call site falls back to a
    /// block; `config::validate` is what makes the fallback visible.
    pub fn parse(name: &str) -> Option<CursorShape> {
        Some(match name.trim() {
            "block" => CursorShape::Block,
            "bar" | "line" | "beam" => CursorShape::Bar,
            "underline" | "under" => CursorShape::Underline,
            _ => return None,
        })
    }

    fn to_terminal(self, blink: bool) -> SetCursorStyle {
        match (self, blink) {
            (CursorShape::Block, true) => SetCursorStyle::BlinkingBlock,
            (CursorShape::Block, false) => SetCursorStyle::SteadyBlock,
            (CursorShape::Bar, true) => SetCursorStyle::BlinkingBar,
            (CursorShape::Bar, false) => SetCursorStyle::SteadyBar,
            (CursorShape::Underline, true) => SetCursorStyle::BlinkingUnderScore,
            (CursorShape::Underline, false) => SetCursorStyle::SteadyUnderScore,
        }
    }
}

/// The command-mode caret color, from the theme's accent (a blue), with a sane
/// fallback for non-RGB palettes.
fn cursor_blue(theme: &Theme) -> (u8, u8, u8) {
    match theme.link {
        crate::render::theme::Color::Rgb(r, g, b) => (r, g, b),
        _ => (0x4c, 0x8b, 0xff),
    }
}

/// Set (`OSC 12`) or reset (`OSC 112`) the terminal cursor color. Not a
/// crossterm command, so emitted as a raw escape and flushed.
fn set_cursor_color(rgb: Option<(u8, u8, u8)>) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = match rgb {
        Some((r, g, b)) => write!(out, "\x1b]12;#{r:02x}{g:02x}{b:02x}\x07"),
        None => write!(out, "\x1b]112\x07"),
    };
    let _ = out.flush();
}

// The `impl App` blocks, by area (split from one file, 2026-11).
mod paint;
mod mouse;
mod disk;
mod paste;
mod help;
mod tree;
mod links;
mod panes;
mod buffers;
mod fuzzy;
mod verbs;
mod writer;
mod lists;
mod registers;
mod visual;
mod insert;
mod prompts;
mod search;
mod commands;

impl App {
    pub fn new(
        config: Config,
        file: Option<PathBuf>,
        config_explicit: Option<PathBuf>,
    ) -> Result<Self> {
        // Absolute and clean from the start, like every path a buffer keeps
        // (see `clean_path`): `shoin n.md` must be the buffer the tree opens.
        let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let buffer = match file {
            Some(path) => {
                Buffer::open(clean_path(&here, &path), &config.markdown.plain_text_extensions)?
            }
            None => Buffer::empty(),
        };
        let config_focus = config.layout.focus.clone();
        let config_numbers = config.layout.numbers.clone();
        let config_text_align = config.layout.text_align.clone();
        // Read before `config` moves into the struct below.
        let autosave = crate::fs::save::Autosave::from_config(&config.editor)
            .interval()
            .map(|every| Instant::now() + every);
        // Bad individual bindings are skipped (not fatal); surface the first as a
        // startup flash so a typo'd action name is visible, not silent.
        let (keymap, key_warnings) = Keymap::from_config(&config.keys, &config.input.leader);
        // The directory the config came from, so `[splash] art = "art.txt"`
        // means "beside my .conf files".
        let found = crate::config::discover(config_explicit.as_deref());
        let configured = !found.is_empty();
        let config_dir = found
            .first()
            .and_then(|f| f.parent().map(|p| p.to_path_buf()));
        let (splash_art, splash_warning) =
            crate::render::splash::choose(&config, config_dir.as_deref());

        // Order: a misspelt setting first, then a bad binding, then the art.
        // Only one fits on the status line, and a setting the editor could not
        // read is the one most likely to be misread as "the feature is broken".
        let first_warning = crate::config::validate(&config)
            .first()
            .map(|w| format!("config: {w}"))
            .or_else(|| key_warnings.first().map(|w| format!("config: {w}")))
            .or(splash_warning);
        let startup_flash = first_warning.map(|w| Flash {
            text: Some(w),
            kind: FlashKind::Error,
            expires_at: Instant::now() + Duration::from_millis(config.status.flash_ms.max(3000)),
        });
        let clipboard = crate::clipboard::session(config.editor.clipboard_background);
        // A failed watch is not fatal — the editor just runs without hot reload.
        // Never under `cargo test`: every test app would watch the developer's
        // real config directory, and hundreds of them at once run out of
        // inotify instances (EMFILE) and fail the watcher's own tests.
        let watcher = if cfg!(test) {
            None
        } else {
            ConfigWatcher::new(config_explicit).ok().flatten()
        };
        Ok(App {
            active: ActiveSet {
                start: buffer.cursor.line,
                end: buffer.cursor.line,
            },
            theme: Theme::from_config(&config.theme).unwrap_or_default(),
            docs: vec![BufferState::new(buffer, config.markdown.math)],
            alternate: None,
            layout: Node::leaf(1, 0),
            focus_pane: 1,
            next_pane_id: 2,
            splash_art,
            configured,
            view: false,
            edit_offered: false,
            embed_mode: crate::transclude::Mode::parse(&config.transclude.embed)
                .unwrap_or_default(),
            last_embed_mode: crate::transclude::Mode::Short,
            disk_epoch: 0,
            config,
            mode: Mode::Normal,
            flash: startup_flash,
            quit: false,
            cursor_shape: None,
            cursor_colored: false,
            pending: Pending::default(),
            escape_run: 0,
            escape_since: None,
            last_find: None,
            registers: HashMap::new(),
            pending_register: None,
            clipboard,
            anchor: None,
            drag_from: None,
            visual_resumes_insert: false,
            visual_owner: None,
            watcher,
            focus: FocusMode::parse(&config_focus).unwrap_or(FocusMode::Off),
            focus_region: None,
            numbers: NumberMode::parse(&config_numbers).unwrap_or_default(),
            last_numbers: NumberMode::Absolute,
            text_align: TextAlign::parse(&config_text_align).unwrap_or_default(),
            search: None,
            dot: Vec::new(),
            recording: Vec::new(),
            in_key: false,
            dot_rev: 0,
            replaying: false,
            insert_repeat: None,
            replaying_insert: false,
            cc_indent_line: None,
            keymap,
            image_protocol: crate::image::detect(),
            images: std::cell::RefCell::new(Vec::new()),
            painted: std::cell::RefCell::new((Vec::new(), (0, 0))),
            overlay: std::cell::Cell::new(None),
            tree_window_prefix: false,
            window_count: 1,
            term_size: (0, 0),
            help: None,
            tree: None,
            diff: None,
            finder: None,
            last_insert: None,
            zen: None,
            needs_redraw: true,
            disk_check_at: Instant::now() + DISK_POLL,
            autosave_at: autosave,
        })
    }

    pub fn run_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<crate::recover::Ended> {
        if let Ok(size) = terminal.size() {
            self.term_size = (size.width, size.height);
        }
        while !self.quit {
            // A signal is ending the editor: its thread owns the terminal now.
            if crate::recover::stopping() {
                return Ok(crate::recover::Ended::Stopped);
            }
            // Keep the crash-recovery copy of the unsaved buffers current, and
            // say so when an open file has a recovery file beside it.
            if let Some(text) = crate::recover::tick(self.docs.iter().map(|d| &d.buffer)) {
                self.notify(text, FlashKind::Error);
                self.needs_redraw = true;
            }
            // Before the draw, so their status lines reach the same frame.
            // Disk before autosave: a clean buffer that just took the file's
            // changes has nothing to write, and a conflicted one should be
            // flagged before the timer tries and is refused.
            if self.disk_tick() {
                self.needs_redraw = true;
            }
            if self.autosave_tick() {
                self.needs_redraw = true;
            }
            // Between frames: a background copy that fell back to OSC 52
            // writes its escape here, never in the middle of a draw.
            self.clipboard.poll();
            self.refresh_blocks();
            self.refresh_focus();
            let Some(frame) = crate::recover::frame() else { return Ok(crate::recover::Ended::Stopped) };
            self.sync_cursor_shape();
            if self.needs_redraw {
                let blanks = {
                    let snapshot = &*self;
                    let frame = terminal.draw(|f| frame::render(f, snapshot))?;
                    self.unpaint_moved_pictures(frame.buffer)
                };
                let places = self.visible_pictures();
                if !blanks.is_empty() {
                    use std::io::Write;
                    let mut stdout = std::io::stdout();
                    let _ = stdout.write_all(blanks.as_bytes());
                    let _ = stdout.flush();
                    // Those pixels are gone, so whatever is still placed must
                    // be painted again.
                    self.painted.borrow_mut().0.clear();
                }
                // Only after a redraw, which is the whole economy of it: the
                // payload goes down the wire once per frame that changed, not
                // once per frame. ratatui has just repainted the cells the
                // pictures sat on, so they have to go back.
                self.paint_images(places);
                self.needs_redraw = false;
            }
            drop(frame);
            self.step()?;
        }
        // Hand the caret back to the terminal's own default on the way out.
        if let Some(_frame) = crate::recover::frame() {
            let _ = execute!(std::io::stdout(), SetCursorStyle::DefaultUserShape);
            set_cursor_color(None);
        }
        Ok(crate::recover::Ended::Quit)
    }

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
    fn unpaint_moved_pictures(&self, buf: &ratatui::buffer::Buffer) -> String {
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
    fn visible_pictures(&self) -> Vec<crate::render::Placement> {
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
    fn paint_images(&self, places: Vec<crate::render::Placement>) {
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
    fn sync_cursor_shape(&mut self) {
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

    /// Keep the block cache in step with the buffer.
    ///
    /// Incremental: edits record the lines they touched (`Buffer::take_dirty`),
    /// so the rescan resumes from the first one's cached carry state and stops
    /// once past the last one the carry re-converges. Only when no
    /// line was reported (a wholesale buffer swap, or a cache that never
    /// matched the document's length) does it fall back to a full rebuild.
    pub(crate) fn refresh_blocks(&mut self) {
        // `[markdown] math` decides what a `$$` line opens; a cache built
        // under the other value is wrong everywhere, not just at an edit.
        if self.blocks.math != self.config.markdown.math {
            let math = self.config.markdown.math;
            let i = self.current();
            let doc = &mut self.docs[i];
            doc.blocks = BlockCache::build(&doc.buffer, math);
            doc.buffer.clear_dirty();
            return;
        }
        if self.blocks.revision == self.buffer.revision {
            self.buffer.clear_dirty();
            return;
        }
        match self.buffer.take_dirty() {
            // `invalidate_from` takes the EDITED lines — it resumes a line
            // earlier itself — and falls back to a full build when it has no
            // valid carry state to resume from (the head of the file, or a
            // shorter cache).
            Some((line, last)) => {
                // Split the borrow: `blocks` and `buffer` are two fields of one
                // document, and the deref that reaches them takes all of `self`.
                let i = self.current();
                let doc = &mut self.docs[i];
                doc.blocks.invalidate_from(&doc.buffer, line, last);
            }
            None => {
                let i = self.current();
                let doc = &mut self.docs[i];
                doc.blocks = BlockCache::build(&doc.buffer, self.config.markdown.math);
            }
        }
    }

    /// Recompute the focus region only when the cursor leaves it (or the buffer
    /// changed) — not every frame. SPEC.md §6.
    fn refresh_focus(&mut self) {
        if self.focus == FocusMode::Off {
            self.focus_region = None;
            return;
        }
        let valid = self
            .focus_region
            .as_ref()
            .is_some_and(|r| r.still_valid(&self.buffer));
        if !valid {
            self.focus_region = FocusRegion::compute(&self.buffer, self.focus);
        }
    }

    fn set_focus(&mut self, mode: FocusMode) {
        self.focus = mode;
        self.focus_region = None;
        self.notify(format!("focus: {}", mode.label()), FlashKind::Info);
    }

    /// Re-read the watched config and hot-swap it. A config that fails to parse
    /// is rejected with an error flash; the running editor is untouched, so a
    /// typo mid-edit never costs you the buffer. SPEC.md §8.
    fn reload_config(&mut self) {
        let candidate = match &self.watcher {
            Some(w) => w.reload(),
            None => {
                self.notify("no config file to reload", FlashKind::Info);
                return;
            }
        };
        match candidate {
            Ok(new) => {
                let (keymap, warnings) = Keymap::from_config(&new.keys, &new.input.leader);
                self.theme = Theme::from_config(&new.theme).unwrap_or_default();
                self.keymap = keymap;
                let was_mouse = self.config.input.mouse;
                let was_math = self.config.markdown.math;
                self.config = new;
                // Every open document, not only the current one: a split pane
                // shows another document's block kinds too.
                if self.config.markdown.math != was_math {
                    let math = self.config.markdown.math;
                    for doc in &mut self.docs {
                        doc.blocks = BlockCache::build(&doc.buffer, math);
                    }
                }
                self.clipboard.set_background(self.config.editor.clipboard_background);
                // A reload re-reads the file, which knows nothing about `--zen`
                // or a `:zen` toggle — put those back on top of it.
                self.reapply_zen();
                if self.config.input.mouse != was_mouse {
                    self.set_mouse_capture(self.config.input.mouse);
                }
                // Re-emit the caret for a possibly-changed [cursor].
                self.cursor_shape = None;
                // [editor] autosave may have just been turned on, off, or
                // re-timed; the deadline in hand was computed from the old one.
                self.arm_autosave();
                // A reload is also how someone iterates on their own art, so
                // re-resolve it here rather than only at startup.
                let dir = self.watcher.as_ref().and_then(|w| w.config_dir());
                let (art, art_warning) =
                    crate::render::splash::choose(&self.config, dir.as_deref());
                self.splash_art = art;
                if let Some(w) = art_warning {
                    return self.notify(w, FlashKind::Error);
                }
                let settings = crate::config::validate(&self.config);
                match settings.first().or_else(|| warnings.first()) {
                    Some(w) => self.notify(format!("config reloaded — {w}"), FlashKind::Error),
                    None => self.notify("config reloaded", FlashKind::Info),
                }
            }
            Err(e) => self.notify(format!("config error: {e}"), FlashKind::Error),
        }
    }

    /// One iteration: wait for input, then absorb everything that has already
    /// arrived with it before handing back to be drawn.
    ///
    /// The draining is the point. A key event is cheap — an edit on a rope and
    /// a cursor clamp — but the frame that follows it is O(lines), so what
    /// costs is drawing BETWEEN two events that were already both in the queue.
    /// Nobody can read those intermediate frames anyway: they are on screen for
    /// as long as it takes to compute the next one. See `BATCH_BUDGET`.
    fn step(&mut self) -> Result<()> {
        if self.watcher.as_ref().is_some_and(|w| w.changed()) {
            self.reload_config();
            self.needs_redraw = true;
        }
        if !event::poll(TICK)? {
            // A flash that timed out is the one change no event announces.
            if self.expire_flash() {
                self.needs_redraw = true;
            }
            return Ok(());
        }
        // The first event is in hand; everything after it counts only if it has
        // arrived too. ZERO, not TICK — waiting here would hold a finished
        // frame back for input that may never come.
        let mut first = Some(event::read()?);
        self.absorb_batch(&mut || match first.take() {
            Some(event) => Ok(Some(event)),
            None => match event::poll(Duration::ZERO)? {
                true => Ok(Some(event::read()?)),
                false => Ok(None),
            },
        })?;
        Ok(())
    }

    /// Apply events from `next` until it has none ready, the budget runs out,
    /// or the editor is quitting. Returns how many were applied.
    ///
    /// Split out from `step` so the batching rule can be held to in a test:
    /// here the source is crossterm's queue, and in the tests it is a `Vec`.
    fn absorb_batch(
        &mut self,
        next: &mut impl FnMut() -> Result<Option<CtEvent>>,
    ) -> Result<usize> {
        let deadline = Instant::now() + BATCH_BUDGET;
        let mut absorbed = 0usize;
        while let Some(event) = next()? {
            self.absorb(event);
            absorbed += 1;
            // `quit` first: whatever follows `:q` in the queue was typed at a
            // buffer that is on its way out, and belongs to the shell now.
            if self.quit {
                // Claimed now, so a signal from here on does not dump the
                // text a `:q!` just discarded.
                crate::recover::quit();
                break;
            }
            if absorbed >= BATCH_MAX || Instant::now() >= deadline {
                break;
            }
        }
        Ok(absorbed)
    }

    /// Apply one terminal event to the editor's state, without drawing.
    fn absorb(&mut self, event: CtEvent) {
        match event {
            CtEvent::Key(key) if key.kind == KeyEventKind::Press => {
                self.on_key(crate::input::legacy::normalize(key));
                self.sync_after_input();
                self.needs_redraw = true;
            }
            CtEvent::Mouse(m) => {
                self.on_mouse(m);
                self.sync_after_input();
                self.needs_redraw = true;
            }
            CtEvent::Paste(text) => {
                self.on_paste(text);
                self.sync_after_input();
                self.needs_redraw = true;
            }
            CtEvent::Resize(w, h) => {
                self.term_size = (w, h);
                self.needs_redraw = true;
            }
            // Coming back to the terminal is exactly when a file is most likely
            // to have changed under you, so this check jumps the poll's queue
            // rather than waiting up to `DISK_POLL` for it.
            CtEvent::FocusGained => {
                // A binding, not a match guard: `check_disk` MUTATES, and a
                // guard that returns false would have run it and then skipped
                // the arm. Clippy suggests the guard; it is wrong here.
                let changed = self.check_disk();
                self.needs_redraw |= changed;
            }
            _ => {}
        }
    }

    /// Enable or disable terminal mouse capture at runtime. Not under
    /// `cargo test`, where stdout is the test runner's terminal, not ours.
    fn set_mouse_capture(&self, on: bool) {
        use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        if cfg!(test) {
            return;
        }
        let Some(_frame) = crate::recover::frame() else { return };
        let _ = if on {
            execute!(std::io::stdout(), EnableMouseCapture)
        } else {
            execute!(std::io::stdout(), DisableMouseCapture)
        };
    }

    /// The whole terminal, for mapping a mouse position through the layout.
    fn mouse_area(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: self.term_size.0,
            height: self.term_size.1,
        }
    }

    /// Left-click positions the cursor and drag-selects from it; the wheel
    /// scrolls the page, as `Ctrl-e` / `Ctrl-y` do.
    fn on_mouse(&mut self, m: MouseEvent) {
        if !self.config.input.mouse {
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // A press ends whatever the previous one started, wherever it
                // lands — including in the tree, which arms no drag.
                self.drag_from = None;
                let tw = if self.tree.is_some() {
                    crate::tree::WIDTH.min(self.term_size.0 / 2)
                } else {
                    0
                };
                if self.tree.is_some() && m.column < tw {
                    self.tree_click(m.row);
                    return;
                }
                let area = self.mouse_area();
                // A click focuses the pane it landed in first — otherwise it
                // would be mapped through the geometry of a different one.
                if let Some(id) = frame::pane_at(self, area, m.column, m.row) {
                    self.focus_pane_id(id);
                    if let Some(t) = self.tree.as_mut() {
                        t.focused = false;
                    }
                }
                if let Some(cursor) = frame::locate_click(self, area, m.column, m.row) {
                    self.buffer.cursor = cursor;
                    // A click abandons any half-typed command.
                    self.pending.reset();
                    // ...and drops any selection, rather than extending it to
                    // where you clicked. Vim does the same: the click puts the
                    // cursor down, and the drag that may follow starts there.
                    if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
                        self.leave_visual();
                    }
                    // Normal and Insert both arm a drag, with the document in
                    // front. Under an overlay the text being dragged over is
                    // not the text on screen, so nothing is armed there.
                    let overlaid =
                        self.help.is_some() || self.diff.is_some() || self.finder.is_some();
                    if matches!(self.mode, Mode::Normal | Mode::Insert) && !overlaid {
                        self.drag_from = Some(cursor);
                    }
                }
            }
            // Dragging out of that press is Visual mode, charwise, anchored
            // where the button went down.
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(from) = self.drag_from else {
                    return;
                };
                // The press armed this in Normal or Insert, but a key since
                // may have moved on — into `:`, or a tree prompt. Those own
                // the input.
                if !matches!(self.mode, Mode::Normal | Mode::Insert | Mode::Visual) {
                    return;
                }
                let area = self.mouse_area();
                let Some(cursor) = frame::locate_drag(self, area, m.column, m.row) else {
                    return;
                };
                // Entering only once the drag has actually reached another
                // cell keeps a shaky hand — a Drag event on the pressed cell —
                // from turning every click into a one-character selection.
                if cursor == from && self.mode != Mode::Visual {
                    return;
                }
                if self.mode != Mode::Visual {
                    // Dragged out of an insert session, the selection remembers
                    // to go back into it — vim's `-- (insert) VISUAL --`. The
                    // insert group is left open deliberately: the operator and
                    // the typing that resumes after it undo as one step, the
                    // same shape `cw` already has.
                    let resumes_insert = self.mode == Mode::Insert;
                    self.enter_visual(Mode::Visual);
                    self.visual_resumes_insert = resumes_insert;
                    // Naming the mode once, on the way in. The caret carries it
                    // from there; nothing stays on screen.
                    if resumes_insert {
                        self.notify("-- (insert) visual --", FlashKind::Info);
                    }
                }
                self.anchor = Some(from);
                self.buffer.cursor = cursor;
            }
            // The selection outlives the button: releasing leaves Visual mode
            // up, so `y`, `d` and `<leader>b` all have something to act on.
            MouseEventKind::Up(MouseButton::Left) => self.drag_from = None,
            MouseEventKind::ScrollDown => self.scroll_view(3),
            MouseEventKind::ScrollUp => self.scroll_view(-3),
            _ => {}
        }
    }

    /// Check the open documents' mtimes if the poll is due.
    fn disk_tick(&mut self) -> bool {
        if Instant::now() < self.disk_check_at {
            return false;
        }
        self.disk_check_at = Instant::now() + DISK_POLL;
        self.check_disk()
    }

    /// Take back what changed on disk, for every document that can have it.
    ///
    /// The two cases are decided by ONE question — does this buffer hold
    /// unsaved work:
    ///
    /// - **Clean:** the file wins, and the buffer is reloaded. Nothing of the
    ///   reader's is lost by definition, and `Buffer::reload` makes it one undo
    ///   step, so even an unwanted reload is `u` away.
    /// - **Modified:** nothing is touched. The two have diverged and only the
    ///   reader can say which wins, so the buffer is flagged and the marker
    ///   goes on the status line. `:w!` keeps theirs, `:revert!` takes the
    ///   file's. This is the one thing an editor must never decide alone.
    ///
    /// A file that has been DELETED is not a change: `changed_externally`
    /// answers `false` when it cannot stat the path, so the buffer keeps its
    /// text and `:w` will recreate the file. Reloading emptiness over a
    /// document because something removed it would be the worst of the
    /// available behaviours.
    fn check_disk(&mut self) -> bool {
        if !self.config.editor.autoreload {
            return false;
        }
        let exts = self.config.markdown.plain_text_extensions.clone();
        let now = std::time::SystemTime::now();
        let mut reloaded: Vec<String> = Vec::new();
        let mut conflicted: Vec<String> = Vec::new();
        let mut failed: Option<String> = None;

        // A document being compared is frozen: the view holds SNAPSHOTS, and
        // the comparison a reader is deciding about must not move under them
        // because a poll noticed a third write.
        let under_review = self.diff.as_ref().map(|d| d.doc);
        for (i, doc) in self.docs.iter_mut().enumerate() {
            if Some(i) == under_review {
                continue;
            }
            let Some(path) = doc.buffer.path.clone() else {
                continue;
            };
            if !crate::fs::save::changed_externally(&path, doc.buffer.disk_mtime) {
                continue;
            }
            // Let a half-written file finish. An `Err` here means the clock
            // disagrees with the filesystem (mtime in the future, which NFS
            // does) — act rather than wait forever on a comparison that will
            // never come true.
            //
            // Read BEFORE the load below, so a failed load is tied to the
            // version it failed on, not to one written a moment after.
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            let settled = mtime
                .map(|m| now.duration_since(m).map(|age| age >= DISK_SETTLE).unwrap_or(true))
                .unwrap_or(true);
            if !settled {
                continue;
            }

            if doc.buffer.modified {
                // Announce a conflict ONCE. The status marker is what persists;
                // re-flashing every second would bury everything else.
                if !doc.buffer.conflict {
                    doc.buffer.conflict = true;
                    conflicted.push(doc.buffer.display_name());
                }
                continue;
            }

            match doc.buffer.reload(&exts) {
                Ok(true) => {
                    doc.buffer.reload_failed = None;
                    reloaded.push(doc.buffer.display_name());
                }
                // The mtime moved but the bytes did not. Absorbed in silence:
                // there is nothing to tell the reader about a `touch`.
                Ok(false) => doc.buffer.reload_failed = None,
                // A file that cannot be loaded (not UTF-8, unreadable) leaves
                // the text and the old mtime, so the next poll tries again and
                // a fixed file — rewritten, or `chmod`ed — still reloads. But
                // each broken version is announced ONCE: re-flashing it every
                // second would bury everything else.
                Err(e) => {
                    if doc.buffer.reload_failed != mtime {
                        doc.buffer.reload_failed = mtime;
                        failed.get_or_insert_with(|| e.to_string());
                    }
                }
            }
        }

        if !reloaded.is_empty() {
            self.disk_changed();
        }
        // A conflict outranks a reload in the report: one of them needs a
        // decision and the other is already finished.
        let conflict = match conflicted.len() {
            0 => None,
            1 => Some(format!("{} changed on disk — :revert! or :w!", conflicted[0])),
            n => Some(format!("{n} files changed on disk — see :ls")),
        };
        // Both are announced only once, so neither may hide the other.
        let failed = failed.map(|why| format!("reload: {why} — the text here is kept"));
        match (conflict, failed, reloaded.len()) {
            (Some(c), Some(f), _) => self.notify(format!("{c} · {f}"), FlashKind::Error),
            (Some(msg), None, _) | (None, Some(msg), _) => self.notify(msg, FlashKind::Error),
            (None, None, 0) => return false,
            (None, None, 1) => self.notify(format!("reloaded {}", reloaded[0]), FlashKind::Info),
            (None, None, n) => self.notify(format!("reloaded {n} files"), FlashKind::Info),
        }
        true
    }

    /// `:diff` — open the conflict view: this buffer beside the file.
    ///
    /// Useful without a conflict too (it reports when the two match), because a
    /// command that only works in the one state you are already alarmed about
    /// is a command nobody has practised.
    fn open_diff(&mut self) {
        let Some(path) = self.buffer.path.clone() else {
            return self.notify("no file to compare against", FlashKind::Error);
        };
        if !path.exists() {
            let name = self.buffer.display_name();
            return self.notify(format!("{name} is not on disk yet"), FlashKind::Error);
        }
        let exts = self.config.markdown.plain_text_extensions.clone();
        let loaded = match crate::fs::open::load(&path, &exts) {
            Ok(l) => l,
            Err(e) => return self.notify(format!("{e}"), FlashKind::Error),
        };

        let name = self.buffer.display_name();
        let mine = crate::diff::lines_of(&self.buffer.rope.to_string());
        let theirs = crate::diff::lines_of(&loaded.rope.to_string());
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        let mut view =
            crate::diff::DiffView::new(self.current(), name.clone(), mine, theirs, mtime);
        if view.align.is_identical() {
            return self.notify(format!("{name} matches the file on disk"), FlashKind::Info);
        }
        // Open ON the first difference rather than at the top: the reader asked
        // to see what changed, and in a long note that is rarely line 1.
        if let Some(first) = view.align.hunks.first().map(|h| h.start) {
            view.reveal(first, crate::diff::DiffView::body_height(self.term_size.1));
        }
        // The view draws the flash inside it, so one left from before it
        // opened (a failed save, say) would read as being about the merge.
        self.flash = None;
        if view.align.coarse {
            self.notify("the two versions share little — showing one block", FlashKind::Info);
        }
        self.diff = Some(view);
        self.needs_redraw = true;
    }

    /// `w` in the diff view — write the merge the selections add up to.
    ///
    /// Commits the WHOLE document, not the difference under the cursor: every
    /// hunk always has a chosen side, so the view is a complete text at all
    /// times and this is the one place it becomes real. That is what makes `q`
    /// a true abort and the result a single undo step.
    ///
    /// It re-checks the file first. `check_disk` freezes a document while its
    /// diff is open, so nothing WE do can move it — but nothing stops a third
    /// party writing the file while the reader deliberates, and forcing past
    /// that would clobber an edit this view never showed them.
    fn commit_diff(&mut self) {
        let Some(view) = self.diff.as_ref() else { return };
        let doc = view.doc;
        let name = view.name.clone();
        let merged = view.merged();
        let snapshot = view.snapshot_mtime;

        let Some(path) = self.docs.get(doc).and_then(|d| d.buffer.path.clone()) else {
            self.diff = None;
            return self.notify("the file is gone — nothing to write", FlashKind::Error);
        };
        if crate::fs::save::changed_externally(&path, snapshot) {
            self.diff = None;
            return self.notify(
                format!("{name} changed again — :diff to see it"),
                FlashKind::Error,
            );
        }

        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        let doc = &mut self.docs[doc];
        // The merge has to land in the buffer before it is written. A buffer
        // that refuses it (read-only mode, a read-only range) must not be
        // written either: that would put the UNMERGED text over the file.
        //
        // Read-only mode keeps the view OPEN, every choice in it, and says how
        // to go on from there: `W` leaves read-only and writes those choices.
        // Closing would throw the reader's per-difference work away over a
        // mode one key can change. A read-only RANGE is not the reader's to
        // change, so that refusal closes the view as before: nothing in it
        // could ever be written.
        if !doc.buffer.replace_all(&merged) {
            self.needs_redraw = true;
            if doc.buffer.read_only {
                let msg = "read-only — nothing written; W leaves read-only and writes";
                return self.notify(msg, FlashKind::Error);
            }
            self.diff = None;
            let msg = format!("part of {name} refuses edits — nothing written");
            return self.notify(msg, FlashKind::Error);
        }
        self.diff = None;
        // Forced, and safe to force: the guard exists to catch a change nobody
        // has looked at, and the check above just proved there is none.
        match doc.buffer.save(policy, true) {
            Ok(()) => {
                self.claim_save_revisions();
                self.disk_changed();
                self.notify(format!("{name} merged and written"), FlashKind::Info)
            }
            Err(e) => self.notify(format!("{e}"), FlashKind::Error),
        }
        self.sync_after_input();
    }

    /// Keys inside the diff view. It has drawn over everything, so this is the
    /// whole grammar while it is up — deliberately small, and none of it edits.
    fn on_key_diff(&mut self, key: KeyEvent) {
        let height = crate::diff::DiffView::body_height(self.term_size.1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(view) = self.diff.as_mut() else { return };

        let max = view.rows().saturating_sub(height);
        match key.code {
            // Single keys, not vim's `]c` / `[c`: this view has its own grammar
            // and nothing to be compatible with, so nothing needs holding.
            KeyCode::Char('n') | KeyCode::Char('p') => {
                let forward = matches!(key.code, KeyCode::Char('n'));
                if let Some(row) = view.step_hunk(forward) {
                    view.reveal(row, height);
                }
            }
            KeyCode::Char('j') | KeyCode::Down => view.scroll = (view.scroll + 1).min(max),
            KeyCode::Char('k') | KeyCode::Up => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Char('d') if ctrl => view.scroll = (view.scroll + height / 2).min(max),
            KeyCode::Char('u') if ctrl => view.scroll = view.scroll.saturating_sub(height / 2),
            KeyCode::Char('g') | KeyCode::Home => view.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => view.scroll = max,
            KeyCode::Char('q') | KeyCode::Esc => {
                self.diff = None;
            }
            // `<Tab>` rather than `<Space>`: space is the LEADER key everywhere
            // else in the editor, and a key that starts a command sequence in
            // every other context should not silently decide which version of
            // your work survives in this one. Tab is a two-way switch by nature
            // and has no Normal-mode meaning to unlearn.
            KeyCode::Tab | KeyCode::BackTab => {
                if let Some(side) = view.cycle_current() {
                    let n = view.hunk + 1;
                    let total = view.align.hunks.len();
                    self.notify(
                        format!("difference {n}/{total}: keep {}", side.label()),
                        FlashKind::Info,
                    );
                }
            }
            // The bulk answers. They SET every difference rather than resolving
            // outright: with per-difference choices in play, a key that both
            // selects and commits would silently discard whatever the reader
            // had already toggled.
            KeyCode::Char('l') => {
                view.set_all(crate::diff::Side::Live);
                self.notify("every difference: keep live · w to commit", FlashKind::Info);
            }
            KeyCode::Char('f') => {
                view.set_all(crate::diff::Side::File);
                self.notify("every difference: keep file · w to commit", FlashKind::Info);
            }
            KeyCode::Char('b') => {
                view.set_all(crate::diff::Side::Both);
                self.notify("every difference: keep both · w to commit", FlashKind::Info);
            }
            KeyCode::Char('w') => return self.commit_diff(),
            // The way on from a `w` refused in read-only mode, named in that
            // refusal: what `:view` would do, without leaving the view and
            // the choices in it. Elsewhere it is `w`.
            KeyCode::Char('W') => {
                if self.view {
                    self.set_view(false);
                }
                return self.commit_diff();
            }
            _ => {}
        }
        self.needs_redraw = true;
    }

    /// `:revert` — take what is on disk, discarding whatever is unsaved.
    ///
    /// The manual form of the same reload, and the "take theirs" answer to a
    /// conflict. It needs the `!` for the same reason `:q` does: on a modified
    /// buffer it throws work away, and that has to be typed deliberately.
    fn revert(&mut self, force: bool) {
        if self.buffer.path.is_none() {
            return self.notify("no file to revert to", FlashKind::Error);
        }
        if self.buffer.modified && !force {
            return self.notify(
                "unsaved changes — :revert! to discard them",
                FlashKind::Error,
            );
        }
        let exts = self.config.markdown.plain_text_extensions.clone();
        // Bound, not deref: a deref borrows all of `App`.
        let i = self.current();
        let doc = &mut self.docs[i];
        let result = doc.buffer.reload(&exts);
        let name = doc.buffer.display_name();
        match result {
            Ok(true) => {
                self.disk_changed();
                self.notify(format!("reverted {name}"), FlashKind::Info)
            }
            Ok(false) => self.notify(format!("{name} already matches disk"), FlashKind::Info),
            Err(e) => self.notify(format!("{e}"), FlashKind::Error),
        }
    }

    /// Arm — or disarm — the autosave deadline from the config as it stands.
    ///
    /// Called at every point the answer can have changed: a write, a `:set`, a
    /// hot reload. `Autosave::interval` returning `None` for "off" is what lets
    /// this be one line rather than a branch.
    fn arm_autosave(&mut self) {
        self.autosave_at = crate::fs::save::Autosave::from_config(&self.config.editor)
            .interval()
            .map(|every| Instant::now() + every);
    }

    /// Write if the deadline has passed. Reports whether anything needs drawing.
    ///
    /// The deadline is re-armed BEFORE the write and whatever the write does, so
    /// a file that cannot be saved — one something else has touched — is
    /// reported once every interval and not once every tick.
    fn autosave_tick(&mut self) -> bool {
        match self.autosave_at {
            Some(due) if Instant::now() >= due => {}
            _ => return false,
        }
        self.arm_autosave();
        self.autosave_now()
    }

    /// Save every modified buffer that has a filename, and say so.
    ///
    /// Three deliberate limits, because an unattended write has none of the
    /// escape hatches `:w` does:
    ///
    /// - **Never `force`.** The external-modification guard is the whole reason
    ///   `:w` can refuse, and a background write is exactly when nobody is
    ///   watching for the refusal. A file someone else has written since we read
    ///   it is LEFT ALONE and named on the status line; `:w!` remains the only
    ///   way past that, because overruling it is a decision.
    /// - **A buffer with no path is skipped**, silently. `save` would fail with
    ///   "no filename", and a timer that flashes an error every three minutes at
    ///   a scratch buffer is worse than one that says nothing.
    /// - **Every document, not just the visible one.** A modified buffer behind
    ///   a split or in another pane is precisely what an autosave is for; `:w`
    ///   only ever means the one in front of you.
    ///
    /// Like `:w`, this seals the undo step (`Buffer::write_to` calls
    /// `history.split()`), so an undo after an autosave stops at the point it
    /// fired. That is the same boundary a manual save leaves and the same one
    /// `undo_coalesce_ms` draws anyway.
    fn autosave_now(&mut self) -> bool {
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        let mut written: Vec<String> = Vec::new();
        let mut refused: Option<String> = None;
        let (current, typing) = (self.current(), self.mode.allows_past_end());
        for (i, doc) in self.docs.iter_mut().enumerate() {
            if !doc.buffer.modified || doc.buffer.path.is_none() {
                continue;
            }
            let policy =
                crate::fs::save::SavePolicy { insert_mode: typing && i == current, ..policy };
            // A conflicted buffer would be refused by the guard every single
            // interval. The status marker already says the file diverged, and
            // saying it again on a timer buries whatever else the line had.
            // (Only reachable with `autoreload` on — nothing else sets this.)
            if doc.buffer.conflict {
                continue;
            }
            match doc.buffer.save(policy, false) {
                Ok(()) => written.push(doc.buffer.display_name()),
                // The FIRST refusal: one line of status, and the rest of the
                // documents still get their turn.
                Err(e) if refused.is_none() => refused = Some(e.to_string()),
                Err(_) => {}
            }
        }
        if !written.is_empty() {
            self.claim_save_revisions();
            self.disk_changed();
            // A trim may have shortened the Visual anchor's line (the cursor
            // is clamped by the buffer; other panes clamp theirs on focus).
            if let Some(a) = self.anchor.as_mut() {
                a.line = a.line.min(self.docs[current].buffer.last_line());
                a.col = a.col.min(self.docs[current].buffer.line_len(a.line));
            }
        }
        match (refused, written.len()) {
            (Some(why), _) => self.notify(format!("autosave: {why}"), FlashKind::Error),
            (None, 0) => return false,
            (None, 1) => self.notify(format!("autosaved {}", written[0]), FlashKind::Info),
            (None, n) => self.notify(format!("autosaved {n} files"), FlashKind::Info),
        }
        true
    }

    /// A save that trimmed the text edited the buffer, but that edit is not
    /// the reader's command: move the "did this command edit" mark past it, so
    /// `.` and the table alignment on leaving Insert see only the reader's.
    fn claim_save_revisions(&mut self) {
        let current = self.current();
        for (i, doc) in self.docs.iter_mut().enumerate() {
            let spent = std::mem::take(&mut doc.buffer.save_revisions);
            if i == current {
                self.dot_rev += spent;
            }
        }
    }

    /// Drop a flash whose time is up, reporting whether it actually cleared —
    /// the status line needs a repaint when it does.
    fn expire_flash(&mut self) -> bool {
        if self.flash.as_ref().is_some_and(|f| Instant::now() >= f.expires_at) {
            self.flash = None;
            return true;
        }
        false
    }

    fn notify(&mut self, text: impl Into<String>, kind: FlashKind) {
        self.flash = Some(Flash {
            text: Some(text.into()),
            kind,
            expires_at: Instant::now() + Duration::from_millis(self.config.status.flash_ms),
        });
    }

    /// Show the default status content (file, position, words) briefly.
    fn touch_status(&mut self) {
        self.flash = Some(Flash {
            text: None,
            kind: FlashKind::Info,
            expires_at: Instant::now() + Duration::from_millis(self.config.status.flash_ms),
        });
    }

    /// Clamp the cursor and recompute the active set after any input.
    fn sync_after_input(&mut self) {
        // Insert mode may sit on the empty line after a trailing newline (an
        // Enter at the end of the text lands there); nothing else may.
        let past_end = self.mode.allows_past_end();
        let last = if past_end {
            self.buffer.line_count().saturating_sub(1)
        } else {
            self.buffer.last_line()
        };
        if self.buffer.cursor.line > last {
            self.buffer.cursor.line = last;
        }
        let len = self.buffer.line_len(self.buffer.cursor.line);
        self.buffer.cursor.clamp(len, past_end);
        if self.view {
            self.clamp_reading_cursor();
        }
        // The selection exists in Visual only, and only where it was made.
        // Every way out — Esc, an operator, `:`, `/`, Insert, another pane or
        // document, the file tree — lands here, so this is where its state is
        // dropped, rather than at each exit. Moving focus away ends Visual in
        // Normal: the insert session it may have come from belongs to the
        // pane left behind.
        let here = (self.focus_pane, self.current());
        let tree = self.tree.as_ref().is_some_and(|t| t.focused);
        let away = tree || self.visual_owner != Some(here);
        if matches!(self.mode, Mode::Visual | Mode::VisualLine) && away && !self.in_key {
            // A mouse click: no key will see it end, so it ends here, and
            // its keys are not a change `.` repeats.
            self.mode = Mode::Normal;
            self.pending.reset();
            self.recording.clear();
            self.dot_rev = self.buffer.revision;
        }
        if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            // The anchor stays inside the text, as the cursor does. A drag
            // that began on the empty line past the last newline keeps that
            // newline: the anchor goes to the end of the last line, past its
            // last character, as the newline itself.
            let last = self.buffer.last_line();
            let end = Cursor::new(last, self.buffer.line_len(last));
            if self.anchor.is_some_and(|a| a.line > last) {
                self.anchor = Some(end);
            }
        } else {
            self.anchor = None;
            self.visual_resumes_insert = false;
            self.visual_owner = None;
        }
        self.active = self.active_set(self.anchor);
        // The focused pane remembers where it is, so leaving and coming back
        // lands in the same place even if another pane moved the document's
        // live cursor meanwhile.
        let cursor = self.buffer.cursor;
        let id = self.focus_pane;
        if let Some(pane) = self.layout.pane_mut(id) {
            pane.cursor = cursor;
        }
    }

    fn move_by(&mut self, m: Motion, count: usize) {
        let page = self.viewport_height();
        if let Some(res) = motion::resolve(&self.buffer, m, count, page, false) {
            self.buffer.cursor = res.target;
        }
    }

    /// Scroll the focused pane `delta` rows, down when positive. Typewriter
    /// and read-only mode tie the view to the cursor, so there the cursor is
    /// what moves; elsewhere the page moves and the cursor only follows when
    /// it would leave the screen (`frame::scrolled_view`).
    fn scroll_view(&mut self, delta: isize) {
        if self.view || self.config.layout.typewriter {
            let m = if delta > 0 { Motion::Down } else { Motion::Up };
            self.move_by(m, delta.unsigned_abs());
            if self.view {
                self.clamp_reading_cursor();
            }
            return;
        }
        let area = Rect { x: 0, y: 0, width: self.term_size.0, height: self.term_size.1 };
        let Some((top, cursor)) = frame::scrolled_view(self, area, delta) else { return };
        if let Some(pane) = self.focused_pane() {
            pane.scroll.set(top);
        }
        self.buffer.cursor = cursor;
    }

    /// The current text-area height in rows, for paging motions.
    fn viewport_height(&self) -> usize {
        let reserved = if self.config.status.enabled { 1 } else { 0 };
        let lay = crate::render::layout::Layout::compute(
            &self.config.layout,
            self.term_size.0,
            self.term_size.1,
            reserved,
        );
        (lay.height as usize).max(1)
    }

    /// A clean Normal state with no half-typed command — a command boundary,
    /// which is what dot-recording keys off.
    fn is_clean(&self) -> bool {
        matches!(self.mode, Mode::Normal) && self.pending.is_clean()
    }

    /// Execute a config-bound `Action` by delegating to the built-in verbs.
    /// Run one resolved command: the action, with the count and register the
    /// grammar gathered for it. This is the ONLY place an action is executed —
    /// config bindings, the built-in grammar and `:` commands all land here,
    /// which is what keeps undo grouping and dot-repeat tractable (SPEC §7.2).
    fn run(&mut self, cmd: Command) {
        if self.view && cmd.action.edits() {
            // Refused once with a way out, then the way out: `i` after the
            // message is the reader who found the editor and wants it.
            if !(self.edit_offered && cmd.action == Action::Insert) {
                self.edit_offered = true;
                self.notify("read-only — press i to edit", FlashKind::Info);
                return;
            }
            self.set_view(false);
        }
        self.edit_offered = false;
        // The register the `"x` prefix named; `store_register`/`take_register`
        // consume it, and anything that does not use it must not keep it.
        self.pending_register = cmd.register;
        let count = cmd.count.max(1);

        if matches!(self.mode, Mode::Visual | Mode::VisualLine) && self.visual_key(&cmd.action, count) {
            self.pending_register = None;
            return;
        }

        match cmd.action {
            Action::Move(Motion::RepeatFind { reverse }) => {
                if let Some((m, n)) = self.find_again(reverse, count) {
                    self.move_by(m, n);
                }
            }
            Action::Move(m) => {
                self.remember_find(m);
                let m = self.resolve_screen_motion(m);
                self.move_by(m, count);
            }
            Action::Operator { op, target } => {
                if let Target::Motion(m) = target {
                    self.remember_find(m);
                }
                self.operate(op, target, count)
            }
            Action::SelectObject { key, around } => self.select_object(key, around),
            Action::SwapSelectionEnds => {
                if let Some(anchor) = self.anchor.replace(self.buffer.cursor) {
                    self.buffer.cursor = anchor;
                }
            }
            Action::ScrollView { down } => {
                let rows = count as isize;
                self.scroll_view(if down { rows } else { -rows });
            }
            Action::DeleteChar => self.delete_chars_under(count),
            Action::DeleteCharBack => self.delete_chars_before(count),
            Action::DeleteToEol => self.change_or_delete_to_eol(Operator::Delete),
            Action::ChangeToEol => self.change_or_delete_to_eol(Operator::Change),
            Action::YankLine => {
                let line = self.buffer.cursor.line;
                let last = self.buffer.last_line();
                self.operate_linewise(Operator::Yank, line, line.saturating_add(count.saturating_sub(1)).min(last));
            }
            // `3rx` replaces three characters, so this needs the count and
            // belongs here rather than in `apply_action`.
            Action::ReplaceChar(c) => self.replace_chars(c, count),
            Action::JoinLines => self.join_lines(count),
            Action::ToggleCase => self.toggle_case(count),
            Action::ListItemMove { down } => self.move_list_item(down, count),
            Action::MoveLines { down } => self.move_lines(down, count),
            Action::SearchNext { reverse } => {
                let backward = match self.search.as_ref() {
                    Some(s) => s.reverse != reverse,
                    None => reverse,
                };
                self.search_move(backward);
            }
            Action::PasteAfter => self.paste(true, count),
            Action::PasteBefore => self.paste(false, count),
            Action::SearchWordUnderCursor => self.search_word_under_cursor(),
            Action::AppendParagraph => {
                self.move_by(Motion::ParagraphForward, 1);
                self.enter_insert();
            }
            Action::Window(c) => {
                self.window_count = count;
                self.window_command(c);
                self.window_count = 1;
            }
            // Toggle symmetry: the binding that opens a split closes it again.
            // `<C-w>v` is the unconditional form, as in vim.
            Action::ToggleSplit { vertical } => {
                if self.layout.count() > 1 {
                    self.close_pane();
                } else {
                    self.split_pane(vertical);
                }
            }
            Action::ClosePane => {
                if !self.close_pane() {
                    self.notify("last pane — :q to quit", FlashKind::Info);
                }
            }
            Action::OnlyPane => self.only_pane(),
            // `3.` replaces the count the change was made with (vim's rule).
            Action::Repeat => self.repeat_dot_with((count > 1).then_some(count)),
            action @ (Action::Insert
            | Action::Append
            | Action::InsertLineStart
            | Action::AppendLineEnd
            | Action::OpenBelow
            | Action::OpenAbove) => {
                let line = matches!(action, Action::OpenBelow | Action::OpenAbove);
                self.apply_action(action);
                if count > 1 && self.mode == Mode::Insert {
                    let start = self.buffer.char_index(self.buffer.cursor);
                    let len_before = self.buffer.rope.len_chars();
                    self.insert_repeat =
                        Some(InsertRepeat { count, line, keys: Vec::new(), start, len_before });
                }
            }
            action => self.apply_action(action),
        }
        self.pending_register = None;
    }

    /// `;` and `,` repeat the last `f`/`t`, so every one of them is recorded as
    /// it runs — including the `dt,` form, which vim repeats too.
    fn remember_find(&mut self, m: Motion) {
        if let Motion::FindChar { target, forward, till } = m {
            self.last_find = Some((target, forward, till));
        }
    }

    /// `H`/`M`/`L` name a line by where it sits on SCREEN, so they can only be
    /// resolved here, where the viewport is known. Everything else passes
    /// through untouched.
    fn resolve_screen_motion(&self, m: Motion) -> Motion {
        let want = match m {
            Motion::ScreenTop | Motion::ScreenMiddle | Motion::ScreenBottom => m,
            other => return other,
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: self.term_size.0,
            height: self.term_size.1,
        };
        let Some((first, last)) = frame::visible_line_range(self, area) else {
            return Motion::GotoLine(self.buffer.cursor.line);
        };
        let off = (self.config.editor.scroll_off as usize).min((last - first) / 2);
        Motion::GotoLine(match want {
            Motion::ScreenTop => first + off,
            Motion::ScreenMiddle => (first + last) / 2,
            _ => last - off,
        })
    }

    /// Apply an operator to whatever the grammar resolved as its range.
    fn operate(&mut self, op: Operator, target: Target, count: usize) {
        match target {
            // `d;`: the last find, with the count `;` itself would use.
            Target::Motion(Motion::RepeatFind { reverse }) => {
                if let Some((m, n)) = self.find_again(reverse, count) {
                    self.apply_operator(op, m, n);
                }
            }
            Target::Motion(m) => {
                let m = self.resolve_screen_motion(m);
                self.apply_operator(op, m, count);
            }
            Target::Object { key, around } => self.operate_object(op, key, around, count),
            Target::Line => {
                let l1 = self.buffer.cursor.line;
                let last = self.buffer.last_line();
                self.operate_linewise(op, l1, l1.saturating_add(count.saturating_sub(1)).min(last));
            }
            Target::Selection => self.visual_operate(op),
        }
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::Save | Action::SaveStayInsert => {
                self.write(None, false);
            }
            Action::WriteQuit => {
                self.write(None, false);
                self.try_quit(false);
            }
            Action::Quit { force } => self.try_quit(force),
            Action::Insert => self.enter_insert(),
            Action::Append => {
                let len = self.buffer.line_len(self.buffer.cursor.line);
                if len > 0 {
                    let col = (self.buffer.cursor.col + 1).min(len);
                    self.buffer.cursor.set_col(col);
                }
                self.enter_insert();
            }
            Action::InsertLineStart => {
                self.move_by(Motion::LineFirstNonBlank, 1);
                self.enter_insert();
            }
            Action::AppendLineEnd => {
                let len = self.buffer.line_len(self.buffer.cursor.line);
                self.buffer.cursor.set_col(len);
                self.enter_insert();
            }
            Action::ListShift { outdent } => self.shift_list_item(outdent),
            Action::FixList => self.fix_list(),
            Action::OpenBelow => self.open_line(false),
            Action::OpenAbove => self.open_line(true),
            Action::NormalMode => self.leave_visual(),
            Action::Visual => self.enter_visual(Mode::Visual),
            Action::VisualLine => self.enter_visual(Mode::VisualLine),
            Action::Command => self.mode = Mode::Command(String::new()),
            Action::SearchForward => {
                self.mode = Mode::Search { query: String::new(), reverse: false }
            }
            Action::SearchBackward => {
                self.mode = Mode::Search { query: String::new(), reverse: true }
            }
            Action::Undo => self.undo_or_warn(),
            Action::Redo => self.redo_or_warn(),
            Action::Repeat => self.repeat_dot(),
            Action::PasteAfter => self.paste(true, 1),
            Action::PasteBefore => self.paste(false, 1),
            Action::DeleteChar => self.delete_chars_under(1),
            Action::ToggleBold => self.writer_toggle("**"),
            Action::ToggleItalic => self.writer_toggle("*"),
            Action::ToggleHighlight => self.writer_toggle("=="),
            Action::ToggleCode => self.writer_toggle("`"),
            Action::ToggleTask => self.toggle_task(),
            Action::AlignTable => self.align_table(),
            Action::InsertLink => self.wrap_link(),
            Action::SetHeading(n) => self.set_heading(n),
            Action::ClearHeading => self.set_heading(0),
            Action::CycleFocus => {
                let next = self.focus.next();
                self.set_focus(next);
            }
            Action::ToggleTypewriter => {
                self.config.layout.typewriter = !self.config.layout.typewriter;
            }
            Action::ToggleConceal => {
                self.config.layout.conceal = !self.config.layout.conceal;
            }
            Action::ToggleNumbers => self.set_numbers(""),
            Action::CycleAlign => self.set_align(""),
            Action::FileTree { root } => {
                let root = self.root_dir(root);
                self.toggle_tree(root);
            }
            // Not toggle symmetry: once open, the overlay owns all input
            // (`on_key_help`), so a repeat of this binding never reaches here
            // to close it — q / Esc does that, same as `:help` opened it.
            // From the focused tree, help opens at the tree's own keys.
            Action::Help => {
                let in_tree = self.tree.as_ref().is_some_and(|t| t.focused);
                self.open_help(if in_tree { "tree" } else { "shortcuts" });
            }
            Action::FollowLink => self.follow_link(),
            Action::OpenExternal => self.open_external(),
            Action::AlternateBuffer => self.alternate_buffer(),
            Action::FindBuffer => self.open_buffer_switcher(),
            Action::FindFile { root } => {
                let root = self.root_dir(root);
                self.open_finder(root);
            }
            Action::ToggleZen => self.set_zen(self.zen.is_none()),
            Action::ToggleView => self.set_view(!self.view),
            // Recognized, deliberately nothing — `"" = "nop"` unbinds a key.
            Action::Nop => {}
            // Handled in `run`, where the count they take is still in hand.
            // Enumerated rather than caught by `_`, because a catch-all here is
            // exactly how `r{c}` came to be emitted by the grammar and reach no
            // handler at all: it compiled, and did nothing.
            Action::Move { .. }
            | Action::Operator { .. }
            | Action::SelectObject { .. }
            | Action::SwapSelectionEnds
            | Action::DeleteCharBack
            | Action::DeleteToEol
            | Action::ChangeToEol
            | Action::YankLine
            | Action::SearchNext { .. }
            | Action::SearchWordUnderCursor
            | Action::ScrollView { .. }
            | Action::AppendParagraph
            | Action::Window(_)
            | Action::ToggleSplit { .. }
            | Action::ClosePane
            | Action::OnlyPane
            | Action::ReplaceChar(_)
            | Action::JoinLines
            | Action::ToggleCase
            | Action::ListItemMove { .. }
            | Action::MoveLines { .. } => {}
        }
    }

    pub(crate) fn on_key(&mut self, key: KeyEvent) {
        // Saved and put back rather than cleared: `.` replays keys through
        // here from inside a key.
        let outer = std::mem::replace(&mut self.in_key, true);
        self.handle_key(key);
        self.in_key = outer;
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // The help overlay owns all input while it is open.
        if self.help.is_some() {
            self.on_key_help(key);
            return;
        }
        // The diff view owns all input while it is open, for the same reason
        // help does: it has drawn over everything and its keys are its own.
        if self.diff.is_some() {
            self.on_key_diff(key);
            return;
        }
        // The finder overlay owns all input while it is open — it sits above
        // both panes, so it is checked before the tree.
        if self.finder.is_some() {
            self.on_key_finder(key);
            return;
        }
        // A prompt the tree opened outranks the tree itself: it is a text
        // field, so `a` while it is up is a letter, not another `a` command.
        if let Mode::Prompt(p) = self.mode.clone() {
            self.on_key_prompt(key, p);
            return;
        }
        // The file tree owns input while it has focus.
        if self.tree.as_ref().is_some_and(|t| t.focused) {
            self.on_key_tree(key);
            return;
        }

        // `.` repeats the last change; intercept it before recording so it does
        // not overwrite `dot` with itself.
        let is_dot = matches!(key.code, KeyCode::Char('.'))
            && !key.modifiers.contains(KeyModifiers::CONTROL);
        if !self.replaying && self.is_clean() && is_dot {
            self.repeat_dot();
            return;
        }

        // Dot recording: a new command starts from a clean Normal state and runs
        // until we return to one; if it changed the buffer, it becomes `dot`.
        if !self.replaying {
            if self.is_clean() {
                self.recording.clear();
                self.dot_rev = self.buffer.revision;
            }
            self.recording.push(Replay::Key(key));
        }

        let mode = self.mode.clone();
        // The pane and document a selection under way belongs to, to see
        // after the key whether the key took focus away from it.
        let selection = match mode {
            Mode::Visual | Mode::VisualLine => self.visual_owner,
            _ => None,
        };

        // Open one undo step per Normal/Visual command. An Insert session
        // continues the group its entering command opened, so the whole
        // `i…Esc` (or `cw…Esc`) collapses into a single undo.
        let opens_group = matches!(mode, Mode::Normal | Mode::Visual | Mode::VisualLine);
        if opens_group {
            // Anchor undo to the cursor at a command BOUNDARY only. A Visual
            // selection and an operator-pending motion are continuations of the
            // command that started them, and have already moved the cursor.
            let anchor = self.is_clean().then_some(self.buffer.cursor);
            self.buffer.history.begin_group(anchor);
        }

        match mode {
            Mode::Normal => self.on_key_normal(key),
            Mode::Insert => self.on_key_insert(key),
            Mode::Visual | Mode::VisualLine => self.on_key_visual(key),
            Mode::Command(buf) => self.on_key_command(key, buf),
            Mode::Search { query, reverse } => self.on_key_search(key, query, reverse),
            Mode::Prompt(p) => self.on_key_prompt(key, p),
        }

        // A key that took focus away from a selection (`<C-w>q`, `<C-w>v`,
        // `<C-^>`, the tree) ends it. Such a session is not a change `.`
        // repeats: its keys, the pane key among them, are dropped, and `.`
        // keeps the change before it. Moving again is `Alt-j` again.
        let tree = self.tree.as_ref().is_some_and(|t| t.focused);
        if selection.is_some_and(|owner| tree || owner != (self.focus_pane, self.current())) {
            if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
                self.mode = Mode::Normal;
            }
            self.pending.reset();
            if !self.replaying {
                self.recording.clear();
                self.dot_rev = self.buffer.revision;
            }
        }

        // Seal the step unless we are still inserting.
        if !matches!(self.mode, Mode::Insert) {
            self.buffer.history.end_group();
        }

        // Command finished (back to a clean boundary): if it edited, remember it.
        if !self.replaying && self.is_clean() && self.buffer.revision != self.dot_rev {
            self.dot = std::mem::take(&mut self.recording);
        }
    }

    // ----------------------------------------------------------------- paste

    /// A bracketed paste: the terminal's own paste (Cmd+V, Ctrl+Shift+V),
    /// arriving as one piece of text instead of as keys.
    ///
    /// Into the buffer it goes in exactly as it was copied, in Normal mode as
    /// much as Insert: no auto-indent, no list continuation, no auto-pair and
    /// no escape alias, any one of which would change it on the way in. Normal
    /// mode inserts at the cursor and stays in Normal, as Neovim does — the
    /// keys-as-commands reading is the accident bracketed paste exists to
    /// prevent. A one-line field takes the first line only, since a newline
    /// there would be Enter.
    fn on_paste(&mut self, text: String) {
        // A paste is not typing a count can repeat.
        self.insert_repeat = None;
        let text = crate::clipboard::normalize(&text);
        let first_line = || text.split('\n').next().unwrap_or("").to_string();
        if self.help.is_some() || self.diff.is_some() {
            return;
        }
        if let Some(finder) = self.finder.as_mut() {
            finder.push_str(&first_line());
            return;
        }
        match self.mode.clone() {
            Mode::Command(mut buf) => {
                buf.push_str(&first_line());
                self.mode = Mode::Command(buf);
                return;
            }
            Mode::Search { mut query, reverse } => {
                query.push_str(&first_line());
                self.mode = Mode::Search { query, reverse };
                return;
            }
            // A yes/no question is answered by one deliberate key, never by
            // whatever happened to be on the clipboard.
            Mode::Prompt(mut p) => {
                if !p.kind.is_confirm() {
                    p.input.push_str(&first_line());
                    self.mode = Mode::Prompt(p);
                }
                return;
            }
            _ => {}
        }
        if self.tree.as_ref().is_some_and(|t| t.focused) || text.is_empty() {
            return;
        }
        // A paste into the text is an edit like any other, and goes through
        // the same refusal (and the same `i` after it).
        if self.view {
            self.edit_offered = true;
            self.notify("read-only — press i to edit", FlashKind::Info);
            return;
        }
        // Over a selection, the selection ends first; a drag made while
        // inserting goes back to the insert session it came out of.
        if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            self.leave_visual();
        }
        // A half-typed command is dropped, not finished by the next key: a `d`
        // left waiting under a paste would take whatever is typed after it.
        if matches!(self.mode, Mode::Normal) {
            self.pending.reset();
        }
        // `.` repeats a paste the way it repeats anything else: from Normal
        // it is a change of its own, inside an insert session it is part of
        // the session's change.
        if !self.replaying {
            if self.is_clean() {
                self.recording.clear();
                self.dot_rev = self.buffer.revision;
            }
            self.recording.push(Replay::Paste(text.clone()));
        }
        let inserting = matches!(self.mode, Mode::Insert);
        if inserting {
            // A step of its own inside the session, so `u` takes back the
            // paste and not the sentence typed before it.
            self.coalesce_insert(true);
            self.escape_run = 0;
            self.escape_since = None;
        } else {
            let anchor = self.buffer.cursor;
            self.buffer.history.begin_group(Some(anchor));
        }
        self.insert_pasted(&text, inserting);
        if inserting {
            self.buffer.history.split();
        } else {
            self.buffer.history.end_group();
        }
        if !self.replaying && self.is_clean() && self.buffer.revision != self.dot_rev {
            self.dot = std::mem::take(&mut self.recording);
        }
    }

    /// Put pasted text in at the cursor. The cursor ends after it while
    /// inserting, and on its last character in Normal mode, as `P` leaves it.
    fn insert_pasted(&mut self, text: &str, inserting: bool) {
        let at = self.buffer.cursor;
        if !self.buffer.insert_str(at, text) {
            return;
        }
        let end = self.buffer.char_index(at) + text.chars().count();
        self.buffer.cursor = if inserting {
            self.idx_to_cursor(end)
        } else {
            // On the last character — and Normal mode cannot rest on a line
            // break, so a paste ending in one leaves the cursor before it.
            let c = self.idx_to_cursor(end - 1);
            let col = c.col.min(self.buffer.line_len(c.line).saturating_sub(1));
            Cursor::new(c.line, col)
        };
    }

    // ------------------------------------------------------------------ help

    fn open_help(&mut self, topic: &str) {
        self.help = Some(Help::open(topic));
    }

    /// Scroll, search or close the help overlay.
    fn on_key_help(&mut self, key: KeyEvent) {
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

    // ------------------------------------------------------------- file tree

    fn cwd(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    /// A path in the form a buffer keeps it: absolute and lexically clean
    /// (`clean_path`), so it can be matched against the tree's and the
    /// finder's paths, which always are.
    fn absolute(&self, p: &Path) -> PathBuf {
        clean_path(&self.cwd(), p)
    }

    /// A path the reader TYPED (`:e`, `:w`, `:saveas`, `:export`): `~/`
    /// expanded, then made absolute and clean like every buffer path.
    fn typed_path(&self, typed: &str) -> PathBuf {
        self.absolute(&expand_home(typed))
    }

    /// A path as a message names it: relative to the working directory when
    /// it is beneath it, whole otherwise.
    fn shown(&self, p: &Path) -> String {
        p.strip_prefix(self.cwd()).unwrap_or(p).display().to_string()
    }

    /// The finder's near view: the edited file's own directory, or the working
    /// directory while the buffer is still unnamed.
    fn file_dir(&self) -> PathBuf {
        // `Path::parent` of a bare filename is the EMPTY path, not the cwd, and
        // an empty directory reads as no directory at all — hence `absolute`
        // first, and the filter after it.
        self.buffer
            .path
            .as_ref()
            .map(|p| self.absolute(p))
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| self.cwd())
    }

    /// `$HOME`: the tree's permanent root, and the finder's wide net. Falls
    /// back to the working directory in an environment that has no home.
    fn home(&self) -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| self.cwd())
    }

    /// `Root::File` is what the plain `file_tree` / `find_file` ask for, so
    /// it is the one `tree.root` can redirect — to the launch folder, for a
    /// vault whose notes live in subfolders. An unknown word keeps the file's
    /// folder (and is named by the config check).
    fn root_dir(&self, root: Root) -> PathBuf {
        let root = match root {
            Root::File => Root::parse(&self.config.tree.root).unwrap_or(Root::File),
            other => other,
        };
        match root {
            Root::File => self.file_dir(),
            Root::Home => self.home(),
            Root::Cwd => self.cwd(),
        }
    }

    /// `<leader>fe`/`fE`: closed → open+focus; open but unfocused → focus;
    /// focused → close.
    ///
    /// `fe` roots at the edited file's own folder — the near view, and what a
    /// writer means by "here" — while `fE` roots at `$HOME`. Neither is a
    /// project root: Shoin edits prose, and a folder of notes is not a
    /// checkout. `-` and `+` move the root from wherever it landed.
    ///
    /// The edited file is selected when the listing contains it, so the tree
    /// opens pointing at where you already are rather than at its first row.
    fn toggle_tree(&mut self, root: PathBuf) {
        // The already-open cases first, so the `FileTree` below is built with
        // `self` unborrowed and can read the buffer's path.
        match &mut self.tree {
            Some(t) if !t.focused => {
                t.focused = true;
                return;
            }
            Some(_) => {
                self.tree = None;
                return;
            }
            None => {}
        }
        let mut tree = FileTree::open(root);
        tree.set_hidden(self.config.tree.show_hidden);
        if let Some(path) = self.buffer.path.as_ref().map(|p| self.absolute(p)) {
            tree.select_path(&path);
        }
        self.tree = Some(tree);
    }

    /// `Ctrl-w` window commands, vim's own vocabulary: `v`/`s` split, `q`/`c`
    /// close, `o` keeps only this pane, `h`/`j`/`k`/`l` move by direction and
    /// `w` cycles. The file-tree sidebar joins in at the left edge: moving left
    /// from the leftmost pane focuses it, and any move out of it comes back.
    fn window_command(&mut self, key: char) {
        // The count belongs to the command, and `run` has already spent it on
        // nothing else, so a `<C-w>` verb is free to use it.
        let tree_focused = self.tree.as_ref().is_some_and(|t| t.focused);
        if tree_focused {
            if matches!(key, 'l' | 'w' | 'j' | 'k') {
                if let Some(t) = self.tree.as_mut() {
                    t.focused = false;
                }
            }
            return;
        }
        match key {
            'v' => self.split_pane(true),
            's' => self.split_pane(false),
            'q' | 'c' => {
                if !self.close_pane() {
                    self.notify("last pane — :q to quit", FlashKind::Info);
                }
            }
            'o' => self.only_pane(),
            'w' => self.cycle_pane(),
            // Sizing. Wider/narrower move by columns, taller/shorter by rows —
            // a text measure is worth moving in more than one column at a time,
            // and a count multiplies the step (`5<C-w>>`).
            '>' | '<' | '+' | '-' => {
                let vertical = matches!(key, '>' | '<');
                let step = if vertical { WIDTH_STEP } else { HEIGHT_STEP };
                let sign = if matches!(key, '>' | '+') { 1 } else { -1 };
                let delta = sign * step * self.window_count.max(1) as i32;
                let area = self.pane_area();
                if !self.layout.resize(area, self.focus_pane, vertical, delta) {
                    self.notify("no room that way", FlashKind::Info);
                }
            }
            '=' => {
                self.layout.equalize();
                self.touch_status();
            }
            'h' | 'j' | 'k' | 'l' => {
                let dir = Dir::from_key(key).unwrap();
                let area = self.pane_area();
                match self.layout.neighbor(area, self.focus_pane, dir) {
                    Some(id) => self.focus_pane_id(id),
                    // Nothing that way: the tree is off the left edge.
                    None if dir == Dir::Left => {
                        if let Some(t) = self.tree.as_mut() {
                            t.focused = true;
                        }
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    fn on_key_tree(&mut self, key: KeyEvent) {
        // The `Ctrl-w` window prefix works from the tree too (e.g. `Ctrl-w l`
        // back to the editor). The tree runs its own key handling, so it keeps
        // its own one-key prefix rather than reaching into the grammar.
        if std::mem::take(&mut self.tree_window_prefix) {
            // Arrows fold to the vim letters, exactly as `bindings::after_prefix`
            // does for the editor — `<C-w><Right>` must work from here too.
            let c = match key.code {
                KeyCode::Left => Some('h'),
                KeyCode::Down => Some('j'),
                KeyCode::Up => Some('k'),
                KeyCode::Right => Some('l'),
                KeyCode::Char(c) => Some(c),
                _ => None,
            };
            if let Some(c) = c {
                self.window_command(c);
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('w') {
            self.tree_window_prefix = true;
            return;
        }
        // Leader sequences reach the keymap from the tree too, so the leader
        // keys mean the same everywhere: `<leader>hl` opens help (at the
        // tree's keys), `<leader>fe` closes the tree, `<leader>ff` finds. Only
        // a key that starts or continues one is passed on; the tree's own
        // single keys stay the tree's.
        if let Some(k) = Key::from_event(key) {
            let starts = matches!(self.keymap.lookup("normal", &[k]), Lookup::Prefix);
            if starts || !self.pending.partial.is_empty() {
                self.feed_pending(key, Table::Normal);
                return;
            }
        }

        let mut close = false;
        let mut to_open = None;
        let mut prompt = None;
        let mut flash = None;
        let mut remember_hidden = None;
        if let Some(tree) = self.tree.as_mut() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => close = true,
                KeyCode::Char('j') | KeyCode::Down => tree.down(),
                KeyCode::Char('k') | KeyCode::Up => tree.up(),
                KeyCode::Char('h') | KeyCode::Left => tree.collapse_or_parent(),
                KeyCode::Char('g') | KeyCode::Home => tree.select_first(),
                KeyCode::Char('G') | KeyCode::End => tree.select_last(),
                KeyCode::Char('R') => tree.refresh(),
                // A directory with no dotfiles does not change on screen, so
                // the toggle says which way it went rather than looking dead.
                KeyCode::Char('H') => {
                    let on = tree.toggle_hidden();
                    remember_hidden = Some(on);
                    flash = Some(if on { "hidden files shown" } else { "hidden files hidden" });
                }
                // The root itself moves: `-` climbs out, `=` descends into the
                // selected directory. Without them the root is a wall, since `h`
                // stops dead at depth 0. Unshifted neighbours on the keyboard,
                // because they are pressed in pairs while reading around.
                KeyCode::Char('-') => {
                    tree.root_up();
                }
                KeyCode::Char('=') => {
                    tree.root_into();
                }
                KeyCode::Char('l') | KeyCode::Char('o') | KeyCode::Enter | KeyCode::Right => {
                    if let Some(Activate::Open(path)) = tree.activate() {
                        to_open = Some(path);
                    }
                }
                // File operations. Each opens a prompt rather than acting at
                // once — even `d`, which asks before it removes anything.
                KeyCode::Char('a') => prompt = Some(PromptKind::Create),
                KeyCode::Char('r') => prompt = Some(PromptKind::Rename),
                KeyCode::Char('m') => prompt = Some(PromptKind::Move),
                KeyCode::Char('d') => prompt = Some(PromptKind::Delete { entries: 0 }),
                _ => {}
            }
        }
        // `H` writes through to the live config, the way `:set conceal` and the
        // typewriter toggle do — which is what makes it outlast the panel.
        if let Some(on) = remember_hidden {
            self.config.tree.show_hidden = on;
        }
        if let Some(msg) = flash {
            self.notify(msg, FlashKind::Info);
        }
        if let Some(kind) = prompt {
            return self.tree_prompt(kind);
        }
        if close {
            self.tree = None;
        }
        if let Some(path) = to_open {
            if self.open_file(path) {
                if self.config.tree.close_on_open {
                    self.tree = None;
                } else if let Some(t) = self.tree.as_mut() {
                    t.focused = false; // hand focus back to the editor
                }
            }
        }
    }

    /// Replace the current buffer with `path`. Refuses if there are unsaved
    /// changes. Returns whether it opened.
    /// A left-click inside the tree pane: focus it and select the clicked row.
    fn tree_click(&mut self, row: u16) {
        // The tree's height as drawn: the window less the bottom row.
        let h = self.term_size.1.saturating_sub(crate::render::frame::bottom_rows(self)) as usize;
        if let Some(tree) = self.tree.as_mut() {
            tree.focused = true;
            let len = tree.entries.len();
            let start = if len <= h {
                0
            } else {
                tree.selected.saturating_sub(h / 2).min(len - h)
            };
            let idx = start + row as usize;
            if idx < len {
                tree.selected = idx;
            }
        }
    }

    /// Open `path` in a NEW document and switch to it — or just switch, if it
    /// is already open. Nothing is displaced, so unlike before there is no
    /// reason to refuse when the current buffer has unsaved changes.
    fn open_file(&mut self, path: PathBuf) -> bool {
        // One file, one buffer, however it was spelled: the clean path catches
        // `n.md` against `./n.md`, `same_file` a symlink against its target.
        let path = self.absolute(&path);
        let real = std::fs::canonicalize(&path).ok();
        let open = self.docs.iter().position(|d| {
            d.buffer.path.as_deref().is_some_and(|p| {
                p == path || real.is_some() && std::fs::canonicalize(p).ok() == real
            })
        });
        if let Some(i) = open {
            self.switch_to(i);
            return true;
        }
        match Buffer::open(path.clone(), &self.config.markdown.plain_text_extensions) {
            Ok(mut buffer) => {
                buffer.read_only = self.view;
                self.docs.push(BufferState::new(buffer, self.config.markdown.math));
                self.switch_to(self.docs.len() - 1);
                self.notify(format!("opened {}", self.shown(&path)), FlashKind::Info);
                true
            }
            Err(e) => {
                self.notify(format!("{e}"), FlashKind::Error);
                false
            }
        }
    }

    // ---------------------------------------------------------- links

    /// Where the link under the cursor points, if the cursor is on one.
    fn link_dest(&self) -> Option<Dest> {
        let line = self.buffer.line_text(self.buffer.cursor.line);
        let span = inline::span_at(&line, self.buffer.cursor.col, &self.config.markdown)?;
        let target = inline::target_of(&span, &line)?;
        Some(match span.kind {
            // A wiki target is a NAME, even when it looks like a path: §14.2
            // resolution tries it relative to this file first anyway.
            Inline::WikiLink => Dest::Note(link::Link::parse(&target)?),
            Inline::Autolink => Dest::Url(target),
            _ if is_url(&target) => Dest::Url(target),
            // `Link::parse` rather than a hand-rolled split, so `note.md#Head`
            // in a markdown link means the same thing it means in `[[…]]`.
            _ => {
                let l = link::Link::parse(&target)?;
                Dest::Path {
                    path: self.file_dir().join(&l.target),
                    section: l.section,
                }
            }
        })
    }

    /// The vault root bare-name resolution searches from, for the edited file.
    fn link_root(&self) -> PathBuf {
        compile::search_root(&self.link_from(), &self.config.transclude)
    }

    /// The file links are resolved RELATIVE to. An unnamed buffer has no path
    /// of its own, so it borrows the working directory's — the same fallback
    /// `file_dir` makes, and for the same reason.
    fn link_from(&self) -> PathBuf {
        match self.buffer.path.as_ref() {
            Some(p) => self.absolute(p),
            None => self.cwd().join("untitled.md"),
        }
    }

    /// `gf` — open what the cursor is on.
    fn follow_link(&mut self) {
        match self.link_dest() {
            None => self.notify("no link under the cursor", FlashKind::Error),
            // Deliberately not "silently do the other thing". `gx` is one key
            // away, and naming it here is how it gets learned.
            Some(Dest::Url(u)) => {
                self.notify(format!("{u} is a URL — gx opens it"), FlashKind::Info)
            }
            Some(Dest::Path { path, section }) => self.open_or_create(path, &section, false),
            Some(Dest::Note(l)) => {
                let (root, from) = (self.link_root(), self.link_from());
                match link::resolve(&l, &from, &root) {
                    Ok(path) => self.open_or_create(path, &l.section, true),
                    // A name nothing answers to is a note not written YET.
                    // Resolution is unchanged; what is new is that its miss is
                    // a starting point rather than an error.
                    Err(link::Unresolved::Missing(_)) => {
                        let path = self.file_dir().join(&l.target);
                        self.open_or_create(path, &l.section, true)
                    }
                    // The refusal becomes a choice: the finder already knows
                    // how to pick one path out of several.
                    Err(link::Unresolved::Ambiguous(_, paths)) => {
                        let n = paths.len();
                        self.finder = Some(Finder::from_paths(paths, &root));
                        self.notify(
                            format!("{n} notes are called {:?} — pick one", l.target),
                            FlashKind::Info,
                        );
                    }
                }
            }
        }
    }

    /// Open `path`, writing an empty file first if it is a note that does not
    /// exist yet.
    ///
    /// The creation is the point of following a link in a vault: `[[tomorrow]]`
    /// is written before `tomorrow.md` is, and the link is how the note gets
    /// started. It is created ON DISK rather than as an unnamed buffer so that
    /// the link that made it resolves from now on — including from every other
    /// note already pointing here.
    ///
    /// `name` is true for a wiki link's target, a note's name rather than a
    /// path (see `named_ext`).
    fn open_or_create(&mut self, path: PathBuf, section: &link::Section, name: bool) {
        let path = if path.exists() { path } else { with_default_ext(path, name) };
        if !path.exists() {
            // `exists` follows links: a link to a missing note lands here too,
            // and there is nothing to create, only a link to fix.
            match ops::taken(&path) {
                Ok(false) => {}
                Ok(true) => {
                    self.notify(format!("{} is a broken link", self.shown(&path)), FlashKind::Error);
                    return;
                }
                Err(e) => {
                    self.notify(format!("{e:#}"), FlashKind::Error);
                    return;
                }
            }
            if !creatable(&path, name) {
                self.notify(format!("no {}", self.shown(&path)), FlashKind::Error);
                return;
            }
            if let Err(e) = ops::create(&path, false) {
                self.notify(format!("{e}"), FlashKind::Error);
                return;
            }
            self.disk_changed();
            if self.open_file(path.clone()) {
                self.notify(format!("created {}", self.shown(&path)), FlashKind::Info);
            }
            return;
        }
        if self.open_file(path) {
            self.jump_to_section(section);
        }
    }

    /// Put the cursor on the heading or block a `#section` asked for.
    ///
    /// A section that is not there is reported but not refused — the note is
    /// already open, and landing at its top beats not moving at all.
    fn jump_to_section(&mut self, section: &link::Section) {
        if matches!(section, link::Section::All) {
            return;
        }
        let text = self.buffer.rope.to_string();
        match link::section_line(&text, section) {
            Some(line) => {
                self.buffer.cursor = Cursor::new(line, 0);
                self.sync_after_input();
            }
            None => self.notify(
                format!("no {:?} in this note", section_label(section)),
                FlashKind::Error,
            ),
        }
    }

    /// `gx` — hand the link under the cursor to the desktop.
    ///
    /// Works on a note or a picture too, not just a URL: `gx` on an
    /// `![[photo.png]]` is how you see it at full size, and on a `[…](x.pdf)`
    /// how you read it. It never CREATES — an external opener has nothing to
    /// do with a note that does not exist.
    fn open_external(&mut self) {
        let arg = match self.link_dest() {
            None => {
                self.notify("no link under the cursor", FlashKind::Error);
                return;
            }
            Some(Dest::Url(u)) => u,
            Some(Dest::Path { path, .. }) if path.exists() => path.display().to_string(),
            Some(Dest::Note(l)) => {
                let (root, from) = (self.link_root(), self.link_from());
                match link::resolve(&l, &from, &root) {
                    Ok(p) => p.display().to_string(),
                    Err(e) => {
                        self.notify(format!("{e}"), FlashKind::Error);
                        return;
                    }
                }
            }
            Some(Dest::Path { path, .. }) => {
                self.notify(format!("no {}", self.shown(&path)), FlashKind::Error);
                return;
            }
        };
        self.spawn_opener(&arg);
    }

    /// Run the desktop's opener on one argument.
    ///
    /// Spawned directly, never through a shell, so nothing written in a note
    /// can be read as a command. The leading-dash refusal closes the one hole
    /// an argument vector does not: `open -a` is a FLAG, whatever it was meant
    /// to be, and a document should not be able to reach one.
    fn spawn_opener(&mut self, arg: &str) {
        if arg.starts_with('-') {
            self.notify("refusing a target that starts with '-'", FlashKind::Error);
            return;
        }
        // No test may ever open a browser or a file manager.
        if cfg!(test) {
            self.notify(format!("opened {arg}"), FlashKind::Info);
            return;
        }
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "explorer"
        } else {
            "xdg-open"
        };
        let spawned = std::process::Command::new(cmd)
            .arg(arg)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawned {
            // Reaped on a thread it outlives: these openers hand off and exit
            // at once, and a session full of un-waited children would collect
            // one zombie per `gx`.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                self.notify(format!("opened {arg}"), FlashKind::Info);
            }
            Err(e) => self.notify(format!("{cmd}: {e}"), FlashKind::Error),
        }
    }

    // ---------------------------------------------------------------- panes

    /// The document the focused pane is showing. Every `self.buffer` in this
    /// file resolves through here (see the `Deref` impl).
    pub fn current(&self) -> usize {
        self.layout
            .pane(self.focus_pane)
            .map(|p| p.doc)
            .unwrap_or(0)
            .min(self.docs.len().saturating_sub(1))
    }

    fn focused_pane(&self) -> Option<&Pane> {
        self.layout.pane(self.focus_pane)
    }

    /// The focused pane's scroll position — a hint the renderer refines.
    pub fn scroll_hint(&self) -> usize {
        self.focused_pane().map(|p| p.scroll.get()).unwrap_or(0)
    }

    /// The whole area panes are laid out in — the frame minus the tree sidebar
    /// and the status line, matching what the renderer does.
    fn pane_area(&self) -> Rect {
        crate::render::frame::pane_area(self, Rect {
            x: 0,
            y: 0,
            width: self.term_size.0,
            height: self.term_size.1,
        })
    }

    /// Split, for tests in other modules (the renderer's).
    #[cfg(test)]
    pub fn split_pane_for_test(&mut self, vertical: bool) {
        self.split_pane(vertical);
    }

    /// `<leader>sv` / `<C-w>v` / `:vsplit` — a second view onto the same
    /// document, side by side (or stacked, for a horizontal split).
    fn split_pane(&mut self, vertical: bool) {
        let doc = self.current();
        let scroll = self.focused_pane().map(|p| p.scroll.get()).unwrap_or(0);
        let cursor = self.buffer.cursor;
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        if self.layout.split(self.focus_pane, vertical, Pane { id, doc, scroll: std::cell::Cell::new(scroll), weight: crate::render::pane::EVEN, cursor }) {
            self.focus_pane = id;
            self.touch_status();
        }
    }

    /// Close the focused pane. The last one stays: with no pane there is
    /// nothing to draw, and the document is not what is being closed here.
    fn close_pane(&mut self) -> bool {
        if self.layout.count() < 2 {
            return false;
        }
        let gone = self.focus_pane;
        // Pick the neighbour BEFORE the tree forgets where this pane was.
        let area = self.pane_area();
        let next = self
            .layout
            .neighbor(area, gone, Dir::Left)
            .or_else(|| self.layout.neighbor(area, gone, Dir::Up))
            .or_else(|| self.layout.neighbor(area, gone, Dir::Right))
            .or_else(|| self.layout.neighbor(area, gone, Dir::Down));
        self.layout.close(gone);
        self.focus_pane = next.unwrap_or_else(|| self.layout.ids()[0]);
        self.sync_after_input();
        self.touch_status();
        true
    }

    /// `<C-w>o` — close every pane but this one.
    fn only_pane(&mut self) {
        let Some(pane) = self.focused_pane().cloned() else {
            return;
        };
        self.layout = Node::Leaf(pane);
        self.touch_status();
    }

    fn focus_pane_id(&mut self, id: PaneId) {
        let Some(pane) = self.layout.pane(id) else { return };
        if id == self.focus_pane {
            return;
        }
        let cursor = pane.cursor;
        self.focus_pane = id;
        // Restore where this pane was looking. `sync_after_input` clamps it,
        // which is what catches a saved position that edits elsewhere have run
        // off the end of.
        self.buffer.cursor = cursor;
        // A selection belongs to the pane it was made in: `sync_after_input`
        // ends it here.
        self.sync_after_input();
        self.touch_status();
    }

    /// `<C-w>w` — cycle through the panes in layout order.
    fn cycle_pane(&mut self) {
        let ids = self.layout.ids();
        if ids.len() < 2 {
            return;
        }
        let i = ids.iter().position(|id| *id == self.focus_pane).unwrap_or(0);
        self.focus_pane_id(ids[(i + 1) % ids.len()]);
    }

    // -------------------------------------------------------------- buffers

    /// Note that a file may have changed on disk; see `disk_epoch`.
    fn disk_changed(&mut self) {
        self.disk_epoch = self.disk_epoch.wrapping_add(1);
    }

    /// Make document `i` current. The one being left keeps its cursor, scroll
    /// and parse cache, so coming back costs nothing.
    fn switch_to(&mut self, i: usize) {
        if i >= self.docs.len() {
            return;
        }
        // Recorded HERE rather than at each caller: every way of reaching
        // another document — `:b`, the switcher, following a link, closing a
        // buffer — goes through this one function, and each of them is a move
        // `<C-^>` should be able to undo.
        let from = self.current();
        if from != i {
            self.alternate = Some(from);
        }
        let cursor = self.docs[i].buffer.cursor;
        if let Some(pane) = self.layout.pane_mut(self.focus_pane) {
            pane.doc = i;
            pane.scroll.set(0);
            // A pane arriving at a document picks up where that document was
            // left, not where this pane was in the last one.
            pane.cursor = cursor;
        }
        self.mode = Mode::Normal;
        self.anchor = None;
        self.visual_resumes_insert = false;
        self.pending.reset();
        // Coming back to a document re-reads its embeds: the notes they name
        // may have been written, or created, while it was out of sight.
        self.disk_changed();
        // The block cache of a document edited while it was in the background
        // (there is no such path today, but reload could add one) resyncs on
        // the next frame through the usual revision check.
        self.active = self.active_set(None);
        self.touch_status();
    }

    /// `:bn` / `:bp` — the buffer list is a ring.
    fn cycle_buffer(&mut self, forward: bool) {
        let n = self.docs.len();
        if n < 2 {
            self.notify("only one buffer", FlashKind::Info);
            return;
        }
        let cur = self.current();
        let next = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
        self.switch_to(next);
        self.notify(self.doc_summary(), FlashKind::Info);
    }

    /// `:b <name>` — switch to the open buffer whose name contains `name`.
    fn switch_by_name(&mut self, name: &str) {
        if let Ok(n) = name.parse::<usize>() {
            if n >= 1 && n <= self.docs.len() {
                self.switch_to(n - 1);
                return;
            }
        }
        let lower = name.to_lowercase();
        let hit = self
            .docs
            .iter()
            .position(|d| d.name().to_lowercase().contains(&lower));
        match hit {
            Some(i) => self.switch_to(i),
            None => self.notify(format!("no buffer matching {name:?}"), FlashKind::Error),
        }
    }

    /// `:bd` — close the current document. The last one never closes: an editor
    /// with no buffer has nothing to draw.
    fn close_buffer(&mut self, force: bool) {
        if self.buffer.modified && !force {
            self.notify("unsaved changes — :bd! to discard", FlashKind::Error);
            return;
        }
        if self.docs.len() == 1 {
            self.notify("last buffer — :q to quit", FlashKind::Error);
            return;
        }
        let gone = self.current();
        self.docs.remove(gone);
        // Every pane showing a document after the removed one shifts down; the
        // ones that were showing the removed document fall back to its
        // neighbour, which is what `min` does here.
        for id in self.layout.ids() {
            if let Some(pane) = self.layout.pane_mut(id) {
                if pane.doc > gone {
                    pane.doc -= 1;
                }
                pane.doc = pane.doc.min(self.docs.len() - 1);
            }
        }
        // The alternate shifts with the panes, for the same reason — and the
        // document that was closed stops being somewhere to go back to.
        self.alternate = match self.alternate {
            Some(a) if a == gone => None,
            Some(a) if a > gone => Some(a - 1),
            other => other,
        };
        let to = gone.min(self.docs.len() - 1);
        // `switch_to` would record the neighbour we are landing on, which is
        // not where `<C-^>` should go back to after a close.
        let keep = self.alternate;
        self.switch_to(to);
        self.alternate = keep;
        self.notify(self.doc_summary(), FlashKind::Info);
    }

    /// `3/5  notes.md` — where you are in the buffer list.
    fn doc_summary(&self) -> String {
        format!(
            "{}/{}  {}",
            self.current() + 1,
            self.docs.len(),
            self.buffer.display_name()
        )
    }

    /// `:ls` — the open buffers on one line, the current one marked.
    fn buffer_list(&self) -> String {
        self.docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let mark = if i == self.current() { "▸" } else { " " };
                let dirty = if d.buffer.modified { self.config.glyphs.modified.as_str() } else { "" };
                format!("{mark}{} {}{dirty}", i + 1, d.name())
            })
            .collect::<Vec<_>>()
            .join("   ")
    }

    /// Every open document that still has unsaved changes.
    /// The ONE place "may we quit?" is answered, and it answers for EVERY open
    /// document — not just the one on screen.
    ///
    /// `:q` and `<leader>q` used to disagree: the command counted every buffer
    /// and the action looked only at `self.buffer`, so quitting from a binding
    /// discarded a modified background buffer without a word.
    /// `:q` — with more than one buffer open this closes the CURRENT one and
    /// stays in the editor, so a session unwinds the way it was built up: one
    /// file at a time. The last buffer takes the editor with it.
    ///
    /// `:Q` leaves in one step instead, and `!` discards either way.
    fn try_quit(&mut self, force: bool) {
        if !force && self.buffer.modified {
            self.notify("unsaved changes — :q! to discard", FlashKind::Error);
            return;
        }
        if self.docs.len() > 1 {
            return self.close_buffer(force);
        }
        self.quit = true;
    }

    /// `:Q` (also `:qa`) — leave with every buffer at once, but only when
    /// nothing would be lost. The COUNT is the useful part of the refusal: it
    /// says how much is still unsaved without making the reader walk the list
    /// to find out.
    fn quit_all(&mut self, force: bool) {
        let unsaved = self.unsaved_count();
        if force || unsaved == 0 {
            self.quit = true;
            return;
        }
        let what = if unsaved == 1 {
            "1 buffer has unsaved changes".to_string()
        } else {
            format!("{unsaved} buffers have unsaved changes")
        };
        self.notify(format!("{what} — :Q! to discard"), FlashKind::Error);
    }

    fn unsaved_count(&self) -> usize {
        self.docs.iter().filter(|d| d.buffer.modified).count()
    }

    // ---------------------------------------------------------- fuzzy finder

    /// `<leader>ff`/`fF`: open the finder. Each open rewalks, so the list is
    /// never stale against files created since.
    ///
    /// Not a toggle, unlike the tree: an open finder is a text field that
    /// swallows every key, so its own binding would arrive as query text. Esc
    /// closes it — the same deal as the `:` box.
    fn open_finder(&mut self, root: PathBuf) {
        self.finder = Some(Finder::open(root));
    }

    /// `<C-^>` — back to the document this pane was showing before.
    ///
    /// Vim's alternate file, and the way back out of a link: `gf` opens a note
    /// in a new document, and this returns to the one that linked to it.
    /// Pressed twice it lands where it started, which is what makes it usable
    /// for reading two notes against each other.
    fn alternate_buffer(&mut self) {
        // Never itself: closing the document you followed a link INTO leaves
        // the alternate pointing at the one you land on, and "go back to where
        // you are" is not somewhere to go.
        let alt = self
            .alternate
            .filter(|a| *a < self.docs.len() && *a != self.current());
        match alt {
            Some(a) => {
                self.switch_to(a);
                self.notify(self.doc_summary(), FlashKind::Info);
            }
            None => self.notify("no alternate buffer", FlashKind::Error),
        }
    }

    /// `<leader>fb`: the same overlay over the open buffers.
    fn open_buffer_switcher(&mut self) {
        let entries = self
            .docs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let dirty = if d.buffer.modified { " ●" } else { "" };
                (i, format!("{}{dirty}", d.name()))
            })
            .collect();
        self.finder = Some(Finder::buffers(entries));
    }

    /// The finder's own tiny input mode: printable keys extend the query,
    /// Ctrl-n/p (and the arrows) move the selection, Enter opens, Esc closes.
    fn on_key_finder(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mut close = false;
        let mut to_open = None;
        let mut to_switch = None;
        if let Some(finder) = self.finder.as_mut() {
            match key.code {
                KeyCode::Esc => close = true,
                KeyCode::Char('c') | KeyCode::Char('[') if ctrl => close = true,
                KeyCode::Enter => {
                    match finder.kind {
                        finder::Kind::Files => to_open = finder.selected_path(),
                        finder::Kind::Buffers => to_switch = finder.selected_id(),
                    }
                    close = true;
                }
                KeyCode::Char('n') | KeyCode::Char('j') if ctrl => finder.down(),
                KeyCode::Char('p') | KeyCode::Char('k') if ctrl => finder.up(),
                KeyCode::Down | KeyCode::Tab => finder.down(),
                KeyCode::Up | KeyCode::BackTab => finder.up(),
                // Unlike the `:` line, an empty query stays open: the list
                // under it is still worth looking at.
                KeyCode::Backspace if finder.query.is_empty() => {}
                KeyCode::Char('h') if ctrl && finder.query.is_empty() => {}
                // The editing keys every field shares.
                _ => {
                    let mut outcome = FieldKey::Stay;
                    finder.edit_query(|q| outcome = field_key(key, q));
                    close = outcome == FieldKey::Cancel;
                }
            }
        }
        if close {
            self.finder = None;
        }
        if let Some(i) = to_switch {
            self.switch_to(i);
        }
        if let Some(path) = to_open {
            // Opening from the finder leaves the tree where it was, but focus
            // belongs with the file that just opened.
            if self.open_file(path) {
                if let Some(t) = self.tree.as_mut() {
                    t.focused = false;
                }
            }
        }
    }

    fn repeat_dot(&mut self) {
        self.repeat_dot_with(None);
    }

    /// `.`, or `3.`: with a count, the change is replayed with that count in
    /// place of the one it was made with, and is remembered that way, so a
    /// later `.` repeats it with the new count too.
    fn repeat_dot_with(&mut self, count: Option<usize>) {
        // A `.` that ended up inside `dot` (in Visual) must not replay itself
        // forever.
        if self.dot.is_empty() || self.replaying {
            return;
        }
        if let Some(n) = count {
            self.dot = with_count(&self.dot, n);
        }
        self.replaying = true;
        for input in self.dot.clone() {
            match input {
                Replay::Key(key) => self.on_key(key),
                Replay::Paste(text) => self.on_paste(text),
            }
        }
        self.replaying = false;
        // Reached through `run` (`3.`, `1.`, `".`, a bound key), the keys
        // that asked for the repeat are not the change to repeat next: the
        // replayed one is. Marking the revision as seen keeps them out.
        self.dot_rev = self.buffer.revision;
    }

    // ----------------------------------------------------------------- normal

    /// Normal and Visual mode are one line of code each now: hand the key to
    /// the grammar (SPEC.md §7.2) and run whatever command comes back.
    fn on_key_normal(&mut self, key: KeyEvent) {
        // `q` leaves a page you are only reading, as it leaves `less`. Only as
        // a whole command: `"q` and `fq` still get their q.
        if self.view
            && self.pending.is_clean()
            && key.code == KeyCode::Char('q')
            && key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
        {
            self.try_quit(false);
            return;
        }
        self.feed_pending(key, Table::Normal);
    }

    fn on_key_visual(&mut self, key: KeyEvent) {
        self.feed_pending(key, Table::Visual);
    }

    fn feed_pending(&mut self, key: KeyEvent, table: Table) {
        let Some(k) = Key::from_event(key) else {
            return;
        };
        // The machine is borrowed out of `self` for the call: it holds no
        // editor state, so it cannot need `&mut self` while resolving.
        let mut pending = std::mem::take(&mut self.pending);
        let resolution = pending.feed(k, &self.keymap, table);
        self.pending = pending;
        match resolution {
            Resolution::Pending => {}
            Resolution::Invalid => {}
            Resolution::Command(cmd) => self.run(cmd),
        }
    }

    // ------------------------------------------------------ pending-state verbs

    /// `u` / `:undo` / a bound `Action::Undo` — all three flash the same hint
    /// when there is nothing left to take back.
    fn undo_or_warn(&mut self) {
        if !self.buffer.undo() {
            self.notify("already at oldest change", FlashKind::Info);
        }
    }

    /// `Ctrl-r` and friends. Mirror of `undo_or_warn`.
    fn redo_or_warn(&mut self) {
        if !self.buffer.redo() {
            self.notify("already at newest change", FlashKind::Info);
        }
    }

    /// In read-only mode the cursor is the top of the screen (see
    /// `layout::scroll_offset`). Past the row where the last screenful starts
    /// it would move with nothing on screen moving, and `k` would then seem
    /// dead until it came back — so it stops there, as `less` does at the end.
    fn clamp_reading_cursor(&mut self) {
        let height = self.viewport_height();
        let line = {
            let cache = self.cache.borrow();
            let last_top = cache.total_rows().saturating_sub(height);
            let (row, _) = cache.locate(self.buffer.cursor.line, 0);
            if row <= last_top {
                return;
            }
            match cache.row(last_top) {
                Some(r) => r.line(),
                None => return,
            }
        };
        self.buffer.cursor = Cursor::new(line, 0);
    }

    /// The lines shown raw: the cursor's (or the selection's), or none at all
    /// in read-only mode, where there is nothing to edit under a cursor.
    fn active_set(&self, anchor: Option<Cursor>) -> ActiveSet {
        if self.view {
            ActiveSet::none()
        } else {
            ActiveSet::compute(&self.mode, &self.buffer.cursor, anchor)
        }
    }

    /// Enter or leave read-only mode, for every open buffer: a page to read,
    /// not to edit. Every line conceals, the caret is hidden, an edit is
    /// refused with a message, and `q` quits. Leaving it is `i` after that
    /// message, or `:view` again.
    pub fn set_view(&mut self, on: bool) {
        self.view = on;
        // Entering says "i to edit", so the very next `i` does exactly that.
        self.edit_offered = on;
        for doc in &mut self.docs {
            doc.buffer.read_only = on;
        }
        if on {
            self.leave_visual();
            self.notify("read-only — q to quit, i to edit", FlashKind::Info);
        } else {
            self.notify("editing", FlashKind::Info);
        }
        self.active = self.active_set(self.anchor);
        self.needs_redraw = true;
    }

    /// Start on `buffer` instead of the blank page: text piped in on stdin.
    pub fn open_unnamed(&mut self, buffer: Buffer) {
        self.buffer = buffer;
        // Built here, not by `refresh_blocks`: both buffers are at revision 0,
        // which it would take to mean nothing changed.
        self.blocks = BlockCache::build(&self.buffer, self.config.markdown.math);
    }

    /// Turn zen mode on or off, remembering what it hid so leaving it restores
    /// the user's own settings rather than a hardcoded default.
    /// Whether zen mode is on: no chrome, the hint bar included.
    pub fn zen_on(&self) -> bool {
        self.zen.is_some()
    }

    /// The command being typed, for the hint bar to say what can follow it.
    pub fn pending(&self) -> &Pending {
        &self.pending
    }

    pub fn set_zen(&mut self, on: bool) {
        match (on, self.zen.take()) {
            (true, None) => {
                self.zen = Some(ZenState {
                    status: self.config.status.enabled,
                    scroll_hint: self.config.layout.scroll_hint,
                });
                self.config.status.enabled = false;
                self.config.layout.scroll_hint = false;
            }
            (true, Some(saved)) => self.zen = Some(saved),
            (false, Some(saved)) => {
                self.config.status.enabled = saved.status;
                self.config.layout.scroll_hint = saved.scroll_hint;
            }
            (false, None) => {}
        }
    }

    /// Re-apply zen over a freshly loaded config, capturing the new file's
    /// values as what leaving zen should restore.
    fn reapply_zen(&mut self) {
        if self.zen.is_some() {
            self.zen = None;
            self.set_zen(true);
        }
    }

    /// Decide where one undo step ends inside an insert session. SPEC.md §4:
    /// a whole `i…Esc` as one step is too coarse, so the step is split at a
    /// word boundary, at a newline, and after `editor.undo_coalesce_ms` of
    /// thinking time — the three places a writer would expect `u` to stop.
    fn coalesce_insert(&mut self, boundary: bool) {
        let now = Instant::now();
        let idle = self
            .last_insert
            .is_some_and(|t| now.duration_since(t) >= self.coalesce_gap());
        if boundary || idle {
            self.buffer.history.split();
        }
        self.last_insert = Some(now);
    }

    fn coalesce_gap(&self) -> Duration {
        Duration::from_millis(self.config.editor.undo_coalesce_ms.max(1))
    }

    /// Whether the cursor sits on whitespace (or past the end of its line).
    fn cursor_on_blank(&self) -> bool {
        let c = self.buffer.cursor;
        self.buffer
            .line_text(c.line)
            .chars()
            .nth(c.col)
            .is_none_or(|ch| ch.is_whitespace())
    }

    /// `vi(` / `vap`: make the object the selection, ready for any verb.
    fn select_object(&mut self, obj: char, around: bool) {
        let Some((first, last, linewise)) = self.object_bounds(obj, around) else {
            return;
        };
        if !matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            self.enter_visual(if linewise { Mode::VisualLine } else { Mode::Visual });
        } else if linewise {
            self.mode = Mode::VisualLine;
        }
        self.anchor = Some(first);
        self.buffer.cursor = last;
    }

    /// Apply an operator over a text object. Objects reach the same `operate_*`
    /// helpers as motions do — an object is just another way of naming a range,
    /// so `diw` and `dw` share every line of their edit, register and undo
    /// behavior.
    fn operate_object(&mut self, op: Operator, obj: char, around: bool, count: usize) {
        let Some((first, mut last, linewise)) = self.object_bounds(obj, around) else {
            return;
        };
        // `d2aw`: a count takes that many words, each one resolved from just
        // past the last. Only the word objects count; the rest are one thing.
        if matches!(obj, 'w' | 'W') && count > 1 {
            // `object_bounds` is cheap to call again: the block refresh is a
            // no-op at the same revision, and `Lists` is lazy.
            let home = self.buffer.cursor;
            let mut end = self.buffer.char_index(last) + 1;
            for _ in 1..count {
                if end >= self.buffer.rope.len_chars() {
                    break;
                }
                self.buffer.cursor = self.idx_to_cursor(end);
                match self.object_bounds(obj, around) {
                    Some((_, l, _)) if self.buffer.char_index(l) + 1 > end => {
                        end = self.buffer.char_index(l) + 1
                    }
                    _ => break,
                }
            }
            self.buffer.cursor = home;
            last = self.idx_to_cursor(end - 1);
        }
        if matches!(op, Operator::Indent | Operator::Outdent) && matches!(obj, 'l' | 'L') {
            return self.shift_list(op == Operator::Outdent, obj == 'L', first.line, last.line);
        }
        if linewise {
            self.operate_linewise(op, first.line, last.line);
        } else {
            self.operate_charwise(op, first, last, true);
        }
    }

    /// The cursors bounding a text object, and whether it is linewise.
    fn object_bounds(&mut self, obj: char, around: bool) -> Option<(Cursor, Cursor, bool)> {
        // The list objects read line kinds, which must describe this text.
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        let range = object::resolve(&self.buffer, &lists, obj, around)?;
        if range.end <= range.start {
            return None;
        }
        Some((
            self.idx_to_cursor(range.start),
            self.idx_to_cursor(range.end - 1),
            range.linewise,
        ))
    }

    fn apply_operator(&mut self, op: Operator, m: Motion, count: usize) {
        use motion::MotionKind::{Exclusive, Inclusive, Linewise};
        let from = self.buffer.cursor;
        let page = self.viewport_height();
        let res = match (op, m) {
            // vim's one irregular verb: on a non-blank, `cw` changes to the
            // END of the word rather than up to the next one, so it does not
            // swallow the space it is about to type over.
            (Operator::Change, Motion::WordForward { big }) if !self.cursor_on_blank() => {
                Some(motion::MotionResult {
                    target: motion::change_word_end(&self.buffer, count, big),
                    kind: Inclusive,
                })
            }
            _ => motion::resolve(&self.buffer, m, count, page, true),
        };
        let Some(res) = res else {
            return;
        };
        if res.kind == Linewise {
            return self.operate_linewise(op, from.line, res.target.line);
        }
        let inclusive = res.kind == Inclusive;
        let (start, mut end) = if (res.target.line, res.target.col) < (from.line, from.col) {
            (res.target, from)
        } else {
            (from, res.target)
        };
        // vim's rule for every exclusive motion, either way it moves
        // (`:help exclusive`): a range ending in column 0 of a later line ends
        // at the end of the line before instead — and is whole lines if it
        // started in the indent. So `dw` on a line's last word and `db` from
        // a line start both leave the line break alone.
        if res.kind == Exclusive && end.col == 0 && end.line > start.line {
            let line = end.line - 1;
            if start.col <= self.first_non_blank(start.line) {
                return self.operate_linewise(op, start.line, line);
            }
            end = Cursor::new(line, self.buffer.line_len(line));
        }
        if op == Operator::Delete && self.delete_goes_linewise(start, end, inclusive) {
            return self.operate_linewise(op, start.line, end.line);
        }
        self.operate_charwise(op, start, end, inclusive)
    }

    /// vim's `op_delete` rule: a charwise delete over more than one line that
    /// starts in the first line's indent and ends with nothing but blanks
    /// after it removes whole lines instead, so `d3w` from the start of
    /// `a / b c / d` leaves `d` rather than an empty line above it.
    fn delete_goes_linewise(&self, start: Cursor, end: Cursor, inclusive: bool) -> bool {
        if end.line <= start.line {
            return false;
        }
        let in_indent = start.col <= self.first_non_blank(start.line);
        let rest_blank = self
            .buffer
            .line_text(end.line)
            .chars()
            .skip(end.col + usize::from(inclusive))
            .all(char::is_whitespace);
        in_indent && rest_blank
    }

    // -------------------------------------------------------- writer verbs (g)

    fn idx_to_cursor(&self, idx: usize) -> Cursor {
        let idx = idx.min(self.buffer.rope.len_chars());
        let line = self.buffer.rope.char_to_line(idx);
        Cursor::new(line, idx - self.buffer.rope.line_to_char(line))
    }

    /// Toggle a symmetric inline marker (`**`, `*`, `==`, `` ` ``) around the
    /// word under the cursor, or the selection in Visual mode.
    fn writer_toggle(&mut self, marker: &str) {
        let (start, end) = if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            let anchor = self.anchor.unwrap_or(self.buffer.cursor);
            let cur = self.buffer.cursor;
            let (lo, hi) = if (anchor.line, anchor.col) <= (cur.line, cur.col) {
                (anchor, cur)
            } else {
                (cur, anchor)
            };
            self.end_visual();
            let s = self.buffer.char_index(lo);
            let e = (self.buffer.char_index(hi) + 1).min(self.buffer.rope.len_chars());
            (s, e)
        } else {
            let cur = self.buffer.cursor;
            match self.word_range(cur.line, cur.col) {
                Some((sc, ec)) => (
                    self.buffer.char_index(Cursor::new(cur.line, sc)),
                    self.buffer.char_index(Cursor::new(cur.line, ec)),
                ),
                None => return,
            }
        };
        // The bold and italic toggles act on what the RENDERER styles, read
        // with its own scanner, and nothing else. That makes `_x_` and `__x__`
        // emphasis like the star forms, and `snake_case` or `a_b_c` plain text.
        // `==` and `` ` `` have one spelling and keep the plain check.
        let italic = match marker {
            "*" => true,
            "**" => false,
            _ => return self.toggle_inline(marker, start, end),
        };
        // A line the renderer styles nothing on (a fence, a table row), or a
        // range over several lines, keeps the plain check too: it only ever
        // strips the marker it writes, so an underscore the reader sees
        // literally is never taken for emphasis.
        let Some(spans) = self.emphasis_spans(start, end) else {
            return self.toggle_inline(marker, start, end);
        };
        // The span the range lies in. Only one that holds ALL of it: a
        // selection running out of a span (`_a_ b`) is not about that span.
        let Some(span) = spans.iter().find(|s| s.outer.start <= start && end <= s.outer.end) else {
            return self.wrap_inline(marker, start, end);
        };
        let has = match span.kind {
            Inline::Italic => italic,
            Inline::Bold => !italic,
            _ => true,
        };
        // Whichever part of the span the range covers, the toggle is about
        // the span: it changes the span's own delimiter runs, star or
        // underscore, by the marker's length.
        let n = marker.len();
        let (s, e) = (span.inner.start, span.inner.end);
        let at = start.clamp(s, e);
        if has {
            // One from `***x***` leaves `**x**`; all of `_x_` leaves `x`.
            self.buffer.delete_chars(e, e + n);
            self.buffer.delete_chars(s - n, s);
            self.buffer.cursor = self.idx_to_cursor(at - n);
        } else {
            // Lengthen the runs: `**x**` becomes `***x***`. A marker written
            // INSIDE the span (`**_x_**`) would show literally, since the
            // renderer styles no span within another; a run of three is the
            // one bold-and-italic it draws.
            let delim = self.buffer.rope.char(span.outer.start).to_string().repeat(n);
            let (at_e, at_s) = (self.idx_to_cursor(e), self.idx_to_cursor(s));
            self.buffer.insert_str(at_e, &delim);
            self.buffer.insert_str(at_s, &delim);
            self.buffer.cursor = self.idx_to_cursor(at + n);
        }
    }

    /// The emphasis spans the renderer styles on the line `[start, end)` sits
    /// on, as char indexes into the rope.
    ///
    /// The renderer styles the top-level spans of a line and, inside a link's
    /// text, one level more (`inline::scan_styled` then `to_styled`):
    /// `_word_` inside `**…**` or `==…==` shows its underscores, so it is not
    /// emphasis here either, while `[_word_](u)` is. `None` where
    /// it styles nothing (`allows_inline`, with the same `[markdown] tables`
    /// override as `render::cache::build`) or the range crosses a line.
    fn emphasis_spans(&mut self, start: usize, end: usize) -> Option<Vec<EmphasisSpan>> {
        self.refresh_blocks();
        let rope = &self.buffer.rope;
        let line = rope.char_to_line(start);
        if end > start && rope.char_to_line(end - 1) != line {
            return None;
        }
        let block = match self.blocks.kinds.get(line) {
            Some(BlockKind::Table) if !self.config.markdown.tables => BlockKind::Paragraph,
            Some(kind) => kind.clone(),
            None => BlockKind::Paragraph,
        };
        if !block.allows_inline() {
            return None;
        }
        let base = rope.line_to_char(line);
        let spans = inline::scan_styled(&self.buffer.line_text(line), &self.config.markdown)
            .into_iter()
            .filter(|s| matches!(s.kind, Inline::Italic | Inline::Bold | Inline::BoldItalic))
            .map(|s| EmphasisSpan {
                kind: s.kind,
                outer: s.outer.start + base..s.outer.end + base,
                inner: s.inner.start + base..s.inner.end + base,
            })
            .collect();
        Some(spans)
    }

    /// The word (alphanumeric/`_` run) under `col`, as a `[start, end)` column
    /// range, or `None` if the cursor is not on a word character.
    fn word_range(&self, line: usize, col: usize) -> Option<(usize, usize)> {
        let chars: Vec<char> = self.buffer.line_text(line).chars().collect();
        if chars.is_empty() {
            return None;
        }
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let col = col.min(chars.len() - 1);
        if !is_word(chars[col]) {
            return None;
        }
        let mut s = col;
        while s > 0 && is_word(chars[s - 1]) {
            s -= 1;
        }
        let mut e = col;
        while e < chars.len() && is_word(chars[e]) {
            e += 1;
        }
        Some((s, e))
    }

    /// Wrap `[start, end)` in `marker`, or strip it if already wrapped.
    fn toggle_inline(&mut self, marker: &str, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let mlen = marker.chars().count();
        let len = self.buffer.rope.len_chars();
        let before = if start >= mlen {
            self.buffer.rope.slice(start - mlen..start).to_string()
        } else {
            String::new()
        };
        let after = if end + mlen <= len {
            self.buffer.rope.slice(end..end + mlen).to_string()
        } else {
            String::new()
        };

        if before == marker && after == marker {
            // Strip: delete the trailing marker first so `start` stays valid.
            self.buffer.delete_chars(end, end + mlen);
            self.buffer.delete_chars(start - mlen, start);
            self.buffer.cursor = self.idx_to_cursor(start - mlen);
        } else {
            self.wrap_inline(marker, start, end);
        }
    }

    /// Wrap `[start, end)` in `marker`, whatever is around it already.
    fn wrap_inline(&mut self, marker: &str, start: usize, end: usize) {
        if start >= end {
            return;
        }
        // Insert at the end first, then the start, so `start` stays valid.
        let (at_end, at_start) = (self.idx_to_cursor(end), self.idx_to_cursor(start));
        self.buffer.insert_str(at_end, marker);
        self.buffer.insert_str(at_start, marker);
        self.buffer.cursor = self.idx_to_cursor(start + marker.chars().count());
    }

    /// `gt`: flip a task checkbox, or add one to a plain list item.
    fn toggle_task(&mut self) {
        // The block cache is refreshed by the RENDER path, so between two keys
        // in one event batch it can still describe the line before the last
        // edit — and `gt` on a line that only just became a list item would
        // read the kind it had before. Same guard, and the same reason, as
        // `open_line_break`; it early-returns when the revisions match.
        self.refresh_blocks();
        let line = self.buffer.cursor.line;
        let chars: Vec<char> = self.buffer.line_text(line).chars().collect();
        let base = self.buffer.rope.line_to_char(line);

        // A task's box sits right after the list marker — or the indent, on a
        // line that is not an item — and only there: a `[x]` further on is
        // link text or prose (`See [x](notes.md)`), never a box to tick.
        let indent = chars.iter().take_while(|c| **c == ' ' || **c == '\t').count();
        let item = match self.blocks.kinds.get(line) {
            Some(BlockKind::ListItem { marker, .. }) => Some(*marker),
            _ => None,
        };
        let mut p = indent;
        if let Some(marker) = item {
            match marker {
                Marker::Ordered => {
                    while p < chars.len() && chars[p].is_ascii_digit() {
                        p += 1;
                    }
                    p += 1;
                }
                _ => p += 1,
            }
            if p < chars.len() && (chars[p] == ' ' || chars[p] == '\t') {
                p += 1;
            }
        }
        let p = p.min(chars.len());

        // Flip an existing `[ ]` / `[x]`.
        if let ['[', inner @ (' ' | 'x' | 'X'), ']', ..] = chars[p..] {
            let new = if inner == ' ' { "x" } else { " " };
            self.buffer.delete_chars(base + p + 1, base + p + 2);
            self.buffer.insert_str(Cursor::new(line, p + 1), new);
            return;
        }

        // Otherwise, if it is a list item, insert a checkbox after the marker.
        if item.is_some() {
            self.buffer.insert_str(Cursor::new(line, p), "[ ] ");
        }
    }

    /// `ga`: pad every cell of the table under the cursor to its column's
    /// width, in the file. The cursor stays on the same character of the
    /// same cell; an already aligned table is left untouched, so `ga` on one
    /// adds nothing to undo.
    fn align_table(&mut self) {
        if !self.align_table_at_cursor() {
            self.notify("not in a table", FlashKind::Error);
        }
    }

    /// `editor.table_align = "auto"`: leaving Insert after an edit in a table
    /// aligns it, as `ga` would. It joins the Insert session's undo step, so
    /// one `u` takes back the typing and the alignment together. Anywhere
    /// else it does nothing, and says nothing.
    fn auto_align_table(&mut self) {
        let auto = TableAlign::parse(&self.config.editor.table_align) == Some(TableAlign::Auto);
        if auto && self.buffer.revision != self.dot_rev {
            self.align_table_at_cursor();
        }
    }

    /// Align the table on the cursor's line. False when it is not in one.
    fn align_table_at_cursor(&mut self) -> bool {
        self.refresh_blocks();
        let at = self.buffer.cursor;
        let Some((s, e)) = table::bounds(&self.blocks.kinds, at.line) else {
            return false;
        };
        let rows: Vec<String> = (s..=e).map(|l| self.buffer.line_text(l)).collect();
        let aligned = table::align(&rows);
        if aligned == rows {
            return true;
        }
        let from = self.buffer.rope.line_to_char(s);
        let to = self.buffer.char_index(Cursor::new(e, self.buffer.line_len(e)));
        self.buffer.delete_chars(from, to);
        self.buffer.insert_str(Cursor::new(s, 0), &aligned.join("\n"));
        let col = table::cursor_col(&rows[at.line - s], &aligned[at.line - s], at.col);
        self.buffer.cursor = Cursor::new(at.line, col);
        true
    }

    /// `g1`..`g6` set the heading level of the current line; `g0` strips it.
    fn set_heading(&mut self, level: u8) {
        let line = self.buffer.cursor.line;
        let chars: Vec<char> = self.buffer.line_text(line).chars().collect();
        let hashes = chars.iter().take_while(|c| **c == '#').count();
        let mut content = hashes;
        while content < chars.len() && (chars[content] == ' ' || chars[content] == '\t') {
            content += 1;
        }
        let base = self.buffer.rope.line_to_char(line);
        if content > 0 {
            self.buffer.delete_chars(base, base + content);
        }
        if level >= 1 {
            let prefix = format!("{} ", "#".repeat(level as usize));
            self.buffer.insert_str(Cursor::new(line, 0), &prefix);
            self.buffer.cursor = Cursor::new(line, prefix.chars().count());
        } else {
            self.buffer.cursor = Cursor::new(line, 0);
        }
    }

    fn enter_insert(&mut self) {
        self.insert_repeat = None;
        self.cc_indent_line = None;
        self.mode = Mode::Insert;
        self.touch_status();
    }

    fn open_line(&mut self, above: bool) {
        let line = self.buffer.cursor.line;
        let at = if above {
            Cursor::new(line, 0)
        } else {
            Cursor::new(line, self.buffer.line_len(line))
        };
        if above {
            self.buffer.insert_str(at, "\n");
            self.buffer.cursor = Cursor::new(line, 0);
        } else {
            self.buffer.insert_str(at, "\n");
            self.buffer.cursor = Cursor::new(line + 1, 0);
        }
        self.enter_insert();
    }

    /// `x`: delete `count` characters from under the cursor, on this line only.
    fn delete_chars_under(&mut self, count: usize) {
        let c = self.buffer.cursor;
        let len = self.buffer.line_len(c.line);
        if len == 0 || c.col >= len {
            return;
        }
        let end_col = c.col.saturating_add(count).min(len);
        let start = self.buffer.char_index(c);
        let end = self.buffer.char_index(Cursor::new(c.line, end_col));
        if end > start {
            let text = self.buffer.rope.slice(start..end).to_string();
            self.buffer.delete_chars(start, end);
            self.store_register(text, false, true);
        }
    }

    /// `X`: delete `count` characters before the cursor, on this line.
    fn delete_chars_before(&mut self, count: usize) {
        let c = self.buffer.cursor;
        if c.col == 0 {
            return;
        }
        let start_col = c.col.saturating_sub(count);
        let a = self.buffer.char_index(Cursor::new(c.line, start_col));
        let b = self.buffer.char_index(c);
        if b > a {
            let text = self.buffer.rope.slice(a..b).to_string();
            self.buffer.delete_chars(a, b);
            self.buffer.cursor = Cursor::new(c.line, start_col);
            self.store_register(text, false, true);
        }
    }

    /// `J`: join the next line onto this one, separated by a single space (unless
    /// this line is empty / already ends in space, or the next line is empty).
    /// `NJ` joins N lines.
    fn join_lines(&mut self, count: usize) {
        for _ in 0..count.saturating_sub(1).max(1) {
            let line = self.buffer.cursor.line;
            if line >= self.buffer.last_line() {
                break;
            }
            let cur_len = self.buffer.line_len(line);
            let cur_text = self.buffer.line_text(line);
            let next_text = self.buffer.line_text(line + 1);
            let lead = next_text.chars().take_while(|c| *c == ' ' || *c == '\t').count();
            let eol = self.buffer.char_index(Cursor::new(line, cur_len));
            let next_start = self.buffer.rope.line_to_char(line + 1);
            self.buffer.delete_chars(eol, next_start + lead);

            let ends_ws = cur_text.chars().last().is_some_and(|c| c == ' ' || c == '\t');
            let next_empty = next_text.chars().nth(lead).is_none();
            if cur_len > 0 && !ends_ws && !next_empty {
                self.buffer.insert_str(Cursor::new(line, cur_len), " ");
            }
            self.buffer.cursor = Cursor::new(line, cur_len);
        }
    }

    /// `~`: flip the case of `count` characters and advance.
    fn toggle_case(&mut self, count: usize) {
        let cur = self.buffer.cursor;
        let chars: Vec<char> = self.buffer.line_text(cur.line).chars().collect();
        let n = count.min(chars.len().saturating_sub(cur.col));
        if n == 0 {
            return;
        }
        let mut out = String::new();
        for &c in &chars[cur.col..cur.col + n] {
            if c.is_uppercase() {
                out.extend(c.to_lowercase());
            } else if c.is_lowercase() {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
        }
        let a = self.buffer.char_index(cur);
        let b = self.buffer.char_index(Cursor::new(cur.line, cur.col + n));
        self.buffer.delete_chars(a, b);
        self.buffer.insert_str(cur, &out);
        let len = self.buffer.line_len(cur.line);
        self.buffer.cursor = Cursor::new(cur.line, (cur.col + n).min(len.saturating_sub(1)));
    }

    /// `;` / `,`: repeat the last `f`/`t` (`,` reverses direction).
    ///
    /// A repeated till that would stay put goes on to the match after, as
    /// vim's does: `t` stops just short of its match, so searching again from
    /// there finds the same match and would never move.
    /// The find `;` (`,` with `reverse`) stands for, and the count to give
    /// it: one more when a till would otherwise stay where it is.
    fn find_again(&self, reverse: bool, count: usize) -> Option<(Motion, usize)> {
        let (target, forward, till) = self.last_find?;
        let m = Motion::FindChar { target, forward: forward ^ reverse, till };
        let stays = till && motion::resolve(&self.buffer, m, count, 1, false).is_none();
        Some((m, if stays { count + 1 } else { count }))
    }

    /// `*`: search forward for the word under the cursor.
    fn search_word_under_cursor(&mut self) {
        let cur = self.buffer.cursor;
        if let Some((s, e)) = self.word_range(cur.line, cur.col) {
            let word: String = self.buffer.line_text(cur.line).chars().skip(s).take(e - s).collect();
            self.search = Some(Search { pattern: word, reverse: false });
            self.search_move(false);
        }
    }

    /// `gl`: wrap the word (or selection) as `[text](url)` and drop the cursor
    /// into the empty URL slot in Insert mode.
    fn wrap_link(&mut self) {
        let (start, end) = if matches!(self.mode, Mode::Visual | Mode::VisualLine) {
            let anchor = self.anchor.unwrap_or(self.buffer.cursor);
            let cur = self.buffer.cursor;
            let (lo, hi) = if (anchor.line, anchor.col) <= (cur.line, cur.col) {
                (anchor, cur)
            } else {
                (cur, anchor)
            };
            self.end_visual();
            let s = self.buffer.char_index(lo);
            let e = (self.buffer.char_index(hi) + 1).min(self.buffer.rope.len_chars());
            (s, e)
        } else {
            let cur = self.buffer.cursor;
            match self.word_range(cur.line, cur.col) {
                Some((sc, ec)) => (
                    self.buffer.char_index(Cursor::new(cur.line, sc)),
                    self.buffer.char_index(Cursor::new(cur.line, ec)),
                ),
                None => return,
            }
        };
        if start >= end {
            return;
        }
        let (at_end, at_start) = (self.idx_to_cursor(end), self.idx_to_cursor(start));
        self.buffer.insert_str(at_end, "]()");
        self.buffer.insert_str(at_start, "[");
        self.buffer.cursor = self.idx_to_cursor(end + 3);
        self.enter_insert();
    }

    /// `D` / `C`: operate from the cursor to the end of the line.
    fn change_or_delete_to_eol(&mut self, op: Operator) {
        let c = self.buffer.cursor;
        let len = self.buffer.line_len(c.line);
        if len > c.col {
            self.operate_charwise(op, c, Cursor::new(c.line, len - 1), true);
        } else if op == Operator::Change {
            self.enter_insert();
        }
    }

    /// `r`: overwrite `count` characters with `ch`.
    fn replace_chars(&mut self, ch: char, count: usize) {
        let cur = self.buffer.cursor;
        let len = self.buffer.line_len(cur.line);
        let n = count.min(len.saturating_sub(cur.col));
        if n == 0 {
            return;
        }
        let start = self.buffer.char_index(cur);
        let end = self.buffer.char_index(Cursor::new(cur.line, cur.col + n));
        self.buffer.delete_chars(start, end);
        let repl: String = std::iter::repeat_n(ch, n).collect();
        self.buffer.insert_str(cur, &repl);
        self.buffer.cursor = Cursor::new(cur.line, cur.col + n - 1);
    }

    /// Delete/change/yank/case a charwise range between two cursors.
    fn operate_charwise(&mut self, op: Operator, a: Cursor, b: Cursor, inclusive: bool) {
        // Indent is a LINE operation whatever motion named it: `>w` shifts the
        // whole line, exactly as in vim.
        if matches!(op, Operator::Indent | Operator::Outdent) {
            return self.operate_linewise(op, a.line, b.line);
        }
        let (lo, hi) = if (a.line, a.col) <= (b.line, b.col) {
            (a, b)
        } else {
            (b, a)
        };
        let start = self.buffer.char_index(lo);
        let mut end = self.buffer.char_index(hi);
        if inclusive {
            end += 1;
        }
        end = end.min(self.buffer.rope.len_chars());
        if start >= end {
            // An empty range changes nothing, but `c` still starts typing
            // there, as vim's `cT(` next to the `(` does.
            if op == Operator::Change {
                self.buffer.cursor = lo;
                self.enter_insert();
            }
            return;
        }
        let text = self.buffer.rope.slice(start..end).to_string();
        match op {
            Operator::Yank => {
                self.store_register(text, false, false);
                self.buffer.cursor = lo;
            }
            Operator::Delete => {
                self.buffer.delete_chars(start, end);
                self.store_register(text, false, true);
                self.buffer.cursor = lo;
            }
            Operator::Change => {
                self.buffer.delete_chars(start, end);
                self.store_register(text, false, true);
                self.buffer.cursor = lo;
                self.enter_insert();
            }
            Operator::Lowercase | Operator::Uppercase => {
                self.recase(start, end, op == Operator::Uppercase);
                self.buffer.cursor = lo;
            }
            Operator::Indent | Operator::Outdent => unreachable!("handled above"),
        }
    }

    /// Delete/change/yank whole lines `l1..=l2`.
    fn operate_linewise(&mut self, op: Operator, l1: usize, l2: usize) {
        let last = self.buffer.last_line();
        let (a, b) = (l1.min(l2), l1.max(l2).min(last));
        let start = self.buffer.rope.line_to_char(a);
        let end = if b < last {
            self.buffer.rope.line_to_char(b + 1)
        } else {
            self.buffer.rope.len_chars()
        };
        let mut text = self.buffer.rope.slice(start..end).to_string();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        match op {
            Operator::Yank => {
                self.store_register(text, true, false);
                // `yy` and `yj` leave the cursor where it was; a range that
                // starts above it (`yk`, `yip`) takes it to its first line.
                let cur = self.buffer.cursor;
                if a < cur.line {
                    let col = cur.col.min(self.buffer.line_len(a).saturating_sub(1));
                    self.buffer.cursor = Cursor::new(a, col);
                }
            }
            Operator::Delete => {
                self.buffer.delete_chars(start, end);
                self.store_register(text, true, true);
                let nl = self.buffer.last_line();
                self.buffer.cursor = Cursor::new(a.min(nl), 0);
            }
            Operator::Change => {
                // With `auto_indent`, the line to type into keeps the first
                // line's indent, as vim's `cc` does with 'autoindent'.
                let indent: String = if self.config.editor.auto_indent {
                    text.chars().take_while(|c| *c == ' ' || *c == '\t').collect()
                } else {
                    String::new()
                };
                self.buffer.delete_chars(start, end);
                self.store_register(text, true, true);
                // Leave one line to type into.
                let at = Cursor::new(a.min(self.buffer.line_count().saturating_sub(1)), 0);
                self.buffer.insert_str(at, &format!("{indent}\n"));
                let line = a.min(self.buffer.line_count().saturating_sub(1));
                self.buffer.cursor = Cursor::new(line, indent.chars().count());
                self.enter_insert();
                if !indent.is_empty() {
                    self.cc_indent_line = Some(line);
                }
            }
            Operator::Lowercase | Operator::Uppercase => {
                self.recase(start, end, op == Operator::Uppercase);
                self.buffer.cursor = Cursor::new(a, 0);
            }
            Operator::Indent | Operator::Outdent => {
                self.shift_lines(a, b, op == Operator::Outdent);
                self.buffer.cursor = Cursor::new(a, self.first_non_blank(a));
            }
        }
    }

    /// Rewrite an absolute char range in one case. Delete + insert so it is a
    /// single history step, and so a mapping that changes length (`ß` -> `SS`)
    /// still lands correctly.
    fn recase(&mut self, start: usize, end: usize, upper: bool) {
        let text = self.buffer.rope.slice(start..end).to_string();
        let mapped = if upper {
            text.to_uppercase()
        } else {
            text.to_lowercase()
        };
        if mapped == text {
            return;
        }
        let at = self.idx_to_cursor(start);
        self.buffer.delete_chars(start, end);
        self.buffer.insert_str(at, &mapped);
    }

    /// `>al` `<al` `>il` `<il` `>aL` `<aL`: shift lines `l1..=l2` of the item
    /// (or level) under the cursor, unless that would break the list's
    /// structure — then refuse, change nothing, and say why. The cursor stays
    /// on the same character of text.
    fn shift_list(&mut self, outdent: bool, level: bool, l1: usize, l2: usize) {
        let width = self.config.editor.tab_width.max(1);
        let line = self.buffer.cursor.line;
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, width);
        let top_level = lists.parent(line).is_none();
        let at_col = lists.item_indent(line).unwrap_or(0);
        // How far to shift, in the document's own steps rather than always
        // `tab_width`. Out, the item lands on its parent's column: it becomes
        // the parent's sibling, and a deeper item cannot skip a level. In, it
        // lines up with the children it joins, or else nests by the step the
        // nearest nesting in the file uses.
        //
        // A whole level moves by one step either way, and out no further than
        // its parent's column. Its own distance from that parent is what
        // changes, so it is not a measure of the step.
        let skip = if level { lists.level_items(line) } else { Vec::new() };
        let step = lists.step(line, &skip).unwrap_or(width);
        let gap = lists.parent(line).map(|p| at_col.saturating_sub(lists.item_indent(p).unwrap_or(0)));
        let cols = if outdent && level {
            gap.map_or(step, |g| step.min(g))
        } else if outdent {
            gap.unwrap_or(width)
        } else if level {
            step
        } else {
            lists
                .prev_sibling_bounds(line)
                .and_then(|(p, _)| lists.first_child(p))
                .and_then(|c| lists.item_indent(c))
                .filter(|&c| c > at_col)
                .map_or(step, |c| c - at_col)
        };
        let refusal = if outdent {
            top_level.then_some("already at the top level")
        } else if level {
            // A top-level list indented 4 columns is an indented code block
            // to every other Markdown tool. Shoin would still call it a list,
            // which only hides the problem. A tab is `width` columns too.
            let to = list::indent_cols(&self.buffer.line_text(l1), width) + cols;
            (top_level && to >= 4).then_some("a top-level list 4 columns in reads as code")
        } else {
            // The first child has nothing to nest under: it would only look
            // deeper while staying the same child of the same parent.
            lists.prev_sibling_bounds(line).is_none().then_some("nothing to indent under")
        };
        if let Some(why) = refusal {
            return self.notify(why, FlashKind::Error);
        }
        // A shift changes columns, never line numbers, so these still name
        // the same text afterwards: the item, the neighbours it may have left
        // behind, its first child (which `>il` and `<il` re-parent), and the
        // list it is about to join. A whole level keeps every relationship,
        // so there is nothing to redo.
        let mut touched = Vec::new();
        if !level {
            let prev = lists.prev_sibling_bounds(line).map(|b| b.0);
            touched.push(line);
            touched.extend(lists.first_child(line));
            touched.extend(prev);
            touched.extend(lists.next_sibling_bounds(line).map(|b| b.0));
            if outdent {
                touched.extend(lists.parent(line));
            } else {
                touched.extend(prev.and_then(|p| lists.first_child(p)));
            }
        }
        let starts = list_starts(&lists, &touched);

        let at = self.buffer.cursor;
        let before = self.buffer.line_len(at.line) as isize;
        self.shift_lines_by(l1, l2, outdent, cols);
        if self.config.editor.list_renumber && !touched.is_empty() {
            self.renumber_lists(&touched, &starts, l1);
        }
        let moved = self.buffer.line_len(at.line) as isize - before;
        self.buffer.cursor = Cursor::new(at.line, (at.col as isize + moved).max(0) as usize);
    }

    /// `Alt-j` / `Alt-k`: move the item under the cursor, children and all,
    /// past `count` of its siblings — stopping at the end of the list rather
    /// than failing when the count is larger. It never leaves its list: that
    /// is what `<al` is for.
    ///
    /// In Visual mode it moves the run of siblings the selection touches
    /// (`Lists::sibling_run`) as one block, and the selection stays on it.
    ///
    /// The block and the siblings it passes swap places as two blocks of
    /// text, and the blank lines between them stay where they were. The
    /// cursor, and in Visual the anchor, follow the block on the same
    /// character.
    fn move_list_item(&mut self, down: bool, count: usize) {
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        let at = self.buffer.cursor;
        let anchor = self.visual_anchor();
        // `run`: the item lines of the first and last item that move;
        // `item`: the lines they span, children included.
        let found = match anchor {
            None => lists.item_bounds(at.line).map(|b| ((b.0, b.0), b)).ok_or(RunError::NotInList),
            Some(a) => lists
                .sibling_run(a.line.min(at.line), a.line.max(at.line))
                .and_then(|run| {
                    let end = lists.item_bounds(run.1).ok_or(RunError::NotInList)?.1;
                    Ok((run, (run.0, end)))
                }),
        };
        let (run, item) = match found {
            Ok(found) => found,
            Err(RunError::NotInList) => return self.notify("not in a list", FlashKind::Error),
            Err(RunError::NoItem) => {
                return self.notify("selection touches no item", FlashKind::Error);
            }
            Err(RunError::TwoLists) => {
                return self.notify("selection spans two lists", FlashKind::Error);
            }
        };
        // The far end of the siblings passed over.
        let step = |line: usize| match down {
            true => lists.next_sibling_bounds(line),
            false => lists.prev_sibling_bounds(line),
        };
        let Some(near) = step(if down { run.1 } else { run.0 }) else {
            let why = if down { "last item" } else { "first item" };
            return self.notify(why, FlashKind::Error);
        };
        let mut far = near;
        for _ in 1..count {
            match step(far.0) {
                Some(b) => far = b,
                None => break,
            }
        }
        let past = if down { (near.0, far.1) } else { (far.0, near.1) };
        let start = list_starts(&lists, &[item.0]).first().map(|(_, n)| *n);
        // Each end's line length, to follow a renumber that widens it.
        let width = |c: Cursor| self.buffer.line_len(c.line) as isize;
        let (cursor_was, anchor_was) = (width(at), anchor.map(width));

        // `first`, the gap, then `second` become `second`, the gap, `first`.
        let (first, second) = if down { (item, past) } else { (past, item) };
        let lines = |(a, b): (usize, usize)| (a..=b).map(|l| self.buffer.line_text(l)).collect::<Vec<_>>();
        let gap: Vec<String> = (first.1 + 1..second.0).map(|l| self.buffer.line_text(l)).collect();
        let swapped = [lines(second), gap.clone(), lines(first)].concat().join("\n");

        let from = self.buffer.rope.line_to_char(first.0);
        let to = self.buffer.char_index(Cursor::new(second.1, self.buffer.line_len(second.1)));
        self.buffer.delete_chars(from, to);
        self.buffer.insert_str(Cursor::new(first.0, 0), &swapped);

        let new_start = if down { first.0 + (second.1 - second.0 + 1) + gap.len() } else { first.0 };
        let new_end = new_start + (item.1 - item.0);
        // Its own list, counting from where it started — the other lists the
        // two blocks hold moved whole, so their numbers are still in order.
        if self.config.editor.list_renumber && start.is_some() {
            self.refresh_blocks();
            let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
            let edits = lists.renumber(new_start, start);
            self.apply_renumber(edits);
        }
        // An end inside the block moves with its line, and its column with the
        // line's text, which only changed if its number did. An end on a
        // blank line outside it lands on the block's edge.
        let follow = |buffer: &Buffer, c: Cursor, was: isize| {
            if c.line < item.0 {
                Cursor::new(new_start, 0)
            } else if c.line > item.1 {
                Cursor::new(new_end, buffer.line_len(new_end).saturating_sub(1))
            } else {
                let line = new_start + (c.line - item.0);
                let moved = buffer.line_len(line) as isize - was;
                Cursor::new(line, (c.col as isize + moved).max(0) as usize)
            }
        };
        self.buffer.cursor = follow(&self.buffer, at, cursor_was);
        if let (Some(a), Some(was)) = (anchor, anchor_was) {
            self.anchor = Some(follow(&self.buffer, a, was));
        }
    }

    /// `move_lines_down` / `move_lines_up`: the selected lines (the cursor's
    /// line outside Visual) swap with the `count` lines past them, like Vim's
    /// `:'<,'>m '>+1 | gv`. Both ends of the selection move with the text, so
    /// it stays selected for the next press, and each press is its own undo
    /// step because every Visual key opens one. A count reaching past the
    /// buffer's end goes as far as it can; none to go at all is refused.
    fn move_lines(&mut self, down: bool, count: usize) {
        let cur = self.buffer.cursor;
        let anchor = self.visual_anchor();
        let other = anchor.unwrap_or(cur);
        // `sync_after_input` keeps the anchor inside the text between keys,
        // but not between the keys of a `.` replay, where an edit can shorten
        // the text under it.
        let last = self.buffer.last_line();
        let (top, bot) = (cur.line.min(other.line).min(last), cur.line.max(other.line).min(last));
        let room = if down { self.buffer.last_line() - bot } else { top };
        let n = count.min(room);
        if n == 0 {
            let why = if down { "last line" } else { "first line" };
            return self.notify(why, FlashKind::Error);
        }
        // `first` then `second` become `second` then `first`.
        let (first, second) = if down { ((top, bot), (bot + 1, bot + n)) } else { ((top - n, top - 1), (top, bot)) };
        let lines = |(a, b): (usize, usize)| (a..=b).map(|l| self.buffer.line_text(l)).collect::<Vec<_>>();
        let swapped = [lines(second), lines(first)].concat().join("\n");

        let from = self.buffer.rope.line_to_char(first.0);
        let to = self.buffer.char_index(Cursor::new(second.1, self.buffer.line_len(second.1)));
        self.buffer.delete_chars(from, to);
        self.buffer.insert_str(Cursor::new(first.0, 0), &swapped);

        let line = |l: usize| if down { l + n } else { l - n };
        self.buffer.cursor = Cursor::new(line(cur.line), cur.col);
        self.anchor = anchor.map(|a| Cursor::new(line(a.line.min(last)), a.col));
    }

    /// Rewrite the ordinals `edits` name. Each is on a line of its own and
    /// leaves the line count alone, so the order they go in does not matter.
    fn apply_renumber(&mut self, edits: impl IntoIterator<Item = list::Renumber>) {
        for r in edits {
            let start = self.buffer.char_index(Cursor::new(r.line, r.cols.start));
            self.buffer.delete_chars(start, start + r.cols.len());
            self.buffer.insert_str(Cursor::new(r.line, r.cols.start), &r.number);
        }
    }

    /// `>al` / `<al` on the item under the cursor, from any mode — the
    /// `list_indent` / `list_outdent` actions and Insert-mode Tab.
    fn shift_list_item(&mut self, outdent: bool) {
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        match lists.item_bounds(self.buffer.cursor.line) {
            Some((s, e)) => self.shift_list(outdent, false, s, e),
            None => self.notify("not in a list", FlashKind::Error),
        }
    }

    /// `:fixlist` / `fix_list`: re-indent the list under the cursor so that
    /// CommonMark tools read it the way Shoin shows it (`Lists::realign`).
    /// One undo step; the cursor stays on the same character of text.
    fn fix_list(&mut self) {
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        let Some(plan) = lists.realign(self.buffer.cursor.line, self.blocks.math) else {
            return self.notify("not in a list", FlashKind::Error);
        };
        let skipped = match plan.skipped.as_slice() {
            [] => String::new(),
            lines => {
                let at: Vec<String> = lines.iter().map(|l| (l + 1).to_string()).collect();
                let (items, them) = if lines.len() == 1 { ("item", "it") } else { ("items", "them") };
                format!(
                    "left {} {items} as written (line{} {}): moving {them} would change how Shoin shows the list",
                    lines.len(),
                    if lines.len() == 1 { "" } else { "s" },
                    at.join(", ")
                )
            }
        };
        if plan.lines.is_empty() {
            return match skipped.is_empty() {
                true => self.notify("list already lined up", FlashKind::Info),
                false => self.notify(skipped, FlashKind::Info),
            };
        }
        let at = self.buffer.cursor;
        let before = self.buffer.line_len(at.line) as isize;
        // One undo step. Bound in Insert, it is a step of its own inside the
        // session, as a paste is, and leaves the session's grouping alone.
        let inserting = matches!(self.mode, Mode::Insert);
        if inserting {
            self.coalesce_insert(true);
        } else {
            self.buffer.history.begin_group(Some(at));
        }
        for &(line, cols) in &plan.lines {
            let start = self.buffer.rope.line_to_char(line);
            let ws = leading_ws_cols(&self.buffer.line_text(line));
            self.buffer.delete_chars(start, start + ws);
            self.buffer.insert_str(Cursor::new(line, 0), &" ".repeat(cols));
        }
        if inserting {
            self.buffer.history.split();
        } else {
            self.buffer.history.end_group();
        }
        let moved = self.buffer.line_len(at.line) as isize - before;
        self.buffer.cursor = Cursor::new(at.line, (at.col as isize + moved).max(0) as usize);
        let n = plan.lines.len();
        let mut msg = format!("re-indented {n} line{}", if n == 1 { "" } else { "s" });
        if !skipped.is_empty() {
            msg = format!("{msg}; {skipped}");
        }
        self.notify(msg, FlashKind::Info);
    }

    /// Whether Insert-mode Tab acts on a list item: `editor.list_tab` is on
    /// and the cursor is on an item's own line. A continuation line, or any
    /// other, keeps the plain Tab.
    fn list_tab_applies(&mut self) -> bool {
        if !self.config.editor.list_tab {
            return false;
        }
        self.refresh_blocks();
        matches!(
            self.blocks.kinds.get(self.buffer.cursor.line),
            Some(BlockKind::ListItem { .. })
        )
    }

    /// Put every ordered level containing one of `lines` back in sequence
    /// after `moved` (an item line) changed level. Lines outside a list, and
    /// bullet lists, are skipped.
    ///
    /// A list keeps the start number it had: `starts` are the levels as they
    /// were, and a level now counts from the old one it shares an item with —
    /// the moved item aside, since it just arrived. Sharing none, it is a
    /// sub-list the edit created, and counts from 1.
    fn renumber_lists(&mut self, lines: &[usize], starts: &[(Vec<usize>, u64)], moved: usize) {
        self.refresh_blocks();
        let lists = Lists::new(&self.buffer, &self.blocks.kinds, self.config.editor.tab_width);
        let start = |items: &[usize]| {
            starts
                .iter()
                .find(|(old, _)| old.iter().any(|i| *i != moved && items.contains(i)))
                .map_or(1, |(_, n)| *n)
        };
        // Keyed by line: two of `lines` in one level ask for the same edits.
        let edits: std::collections::BTreeMap<usize, list::Renumber> = lines
            .iter()
            .flat_map(|&l| lists.renumber(l, Some(start(&lists.level_items(l)))))
            .map(|r| (r.line, r))
            .collect();
        self.apply_renumber(edits.into_values());
    }

    /// `>`/`<` over whole lines. One indent unit is `tab_width` spaces, or a
    /// literal tab when `expand_tab` is off. Blank lines are left alone —
    /// indenting whitespace-only lines just leaves trailing whitespace behind.
    fn shift_lines(&mut self, l1: usize, l2: usize, outdent: bool) {
        let width = self.config.editor.tab_width.max(1);
        self.shift_lines_by(l1, l2, outdent, width);
    }

    /// `shift_lines` by `cols` columns instead of one indent unit. Indenting
    /// inserts tabs only when `expand_tab` is off and `cols` is whole tabs,
    /// spaces otherwise; outdenting removes leading whitespace up to `cols`
    /// columns, a tab counting as `tab_width`.
    fn shift_lines_by(&mut self, l1: usize, l2: usize, outdent: bool, cols: usize) {
        let width = self.config.editor.tab_width.max(1);
        let cols = cols.max(1);
        let unit: String = if !self.config.editor.expand_tab && cols.is_multiple_of(width) {
            "\t".repeat(cols / width)
        } else {
            " ".repeat(cols)
        };
        for line in l1..=l2.min(self.buffer.line_count().saturating_sub(1)) {
            let text = self.buffer.line_text(line);
            if text.trim().is_empty() {
                continue;
            }
            if !outdent {
                self.buffer.insert_str(Cursor::new(line, 0), &unit);
                continue;
            }
            let mut drop = 0usize;
            let mut gone = 0usize;
            for c in text.chars() {
                if gone >= cols {
                    break;
                }
                match c {
                    '\t' => gone += width,
                    ' ' => gone += 1,
                    _ => break,
                }
                drop += 1;
            }
            if drop > 0 {
                let start = self.buffer.char_index(Cursor::new(line, 0));
                self.buffer.delete_chars(start, start + drop);
            }
        }
    }

    /// Column of the first non-blank character on `line`.
    fn first_non_blank(&self, line: usize) -> usize {
        self.buffer
            .line_text(line)
            .chars()
            .take_while(|c| c.is_whitespace())
            .count()
    }

    /// Record text into the register a `"x` prefix named, or — with no prefix —
    /// into the unnamed register plus the one vim would also fill: `"0` for a
    /// yank, the `"1`-`"9` ring for a delete.
    ///
    /// An UPPERCASE name appends to that register instead of replacing it, so
    /// `"Ayy` on several lines collects them.
    ///
    /// `"+` and `"*` are the system clipboard — one clipboard, not X11's two:
    /// PRIMARY is the selection a terminal already owns. With
    /// `[editor] clipboard` on, an unnamed store goes there as well, the way
    /// vim's `clipboard=unnamedplus` does; a NAMED one still does not, so
    /// `"ayy` remains a way to keep something out of it.
    fn store_register(&mut self, text: String, linewise: bool, deleted: bool) {
        let reg = Register { text, linewise };
        if let Some(name) = self.pending_register.take() {
            if is_clipboard_register(name) {
                self.copy_to_clipboard(reg);
                return;
            }
            let key = name.to_ascii_lowercase();
            let merged = if name.is_ascii_uppercase() {
                match self.registers.get(&key) {
                    Some(prev) => {
                        // Lines on either side make the result lines, each
                        // ending in a newline: `"ayy"Ayl` holds `ab\na\n`, and
                        // `"ayl"Ayy` holds `a\nab\n`, as in vim.
                        let linewise = prev.linewise || reg.linewise;
                        let mut text = prev.text.clone();
                        if linewise && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(&reg.text);
                        if linewise && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        Register { text, linewise }
                    }
                    None => reg.clone(),
                }
            } else {
                reg.clone()
            };
            self.registers.insert(key, merged.clone());
            self.registers.insert('"', merged);
            return;
        }
        if deleted {
            for n in (1..9u8).rev() {
                let from = (b'0' + n) as char;
                let to = (b'0' + n + 1) as char;
                if let Some(r) = self.registers.get(&from).cloned() {
                    self.registers.insert(to, r);
                }
            }
            self.registers.insert('1', reg.clone());
        } else {
            self.registers.insert('0', reg.clone());
        }
        if self.config.editor.clipboard {
            self.copy_to_clipboard(reg);
        } else {
            self.registers.insert('"', reg);
        }
    }

    /// Copy to the system clipboard, keeping a record of what went: `"+` for
    /// `read_clipboard_register` to recognise, and `"` as every store fills.
    fn copy_to_clipboard(&mut self, reg: Register) {
        self.clipboard.copy(&reg.text);
        self.registers.insert('+', reg.clone());
        self.registers.insert('"', reg);
    }

    /// The register a command should read: the one a `"x` prefix named (consumed
    /// here), else the unnamed one — or the system clipboard, for `"+`/`"*` or
    /// an unnamed paste with `[editor] clipboard` on.
    fn take_register(&mut self) -> Option<Register> {
        let name = self
            .pending_register
            .take()
            .unwrap_or('"')
            .to_ascii_lowercase();
        let system =
            is_clipboard_register(name) || (name == '"' && self.config.editor.clipboard);
        if !system {
            return self.registers.get(&name).cloned();
        }
        match self.clipboard.paste() {
            // A clipboard that holds no text — emptied, or an image copied
            // since — pastes nothing, not a copy it has since replaced.
            Some(text) if text.is_empty() => None,
            Some(text) => Some(self.read_clipboard_register(text)),
            // No clipboard to read — no program, or over SSH, where copying
            // went out as OSC 52 and cannot come back. Shoin's own record of
            // its last copy is the best answer left.
            None => {
                let key = if name == '"' { '"' } else { '+' };
                self.registers.get(&key).cloned()
            }
        }
    }

    /// A register for text read off the system clipboard, which says nothing
    /// about lines. If it still holds what Shoin last put there, that copy's
    /// record knows whether it was `yy` or `yw`; otherwise text ending in a
    /// newline is taken for whole lines, which is vim's rule too.
    fn read_clipboard_register(&self, text: String) -> Register {
        if let Some(ours) = self.registers.get(&'+').filter(|r| r.text == text) {
            return ours.clone();
        }
        let linewise = text.ends_with('\n');
        Register { text, linewise }
    }

    /// `p` / `P`: paste the register after / before the cursor, `count` times
    /// over as one piece of text (`3p`).
    fn paste(&mut self, after: bool, count: usize) {
        let mut reg = match self.take_register() {
            Some(r) => r,
            None => return,
        };
        if reg.linewise && !reg.text.ends_with('\n') {
            reg.text.push('\n');
        }
        // Checked BEFORE the repeat: `99999p` would otherwise ask for
        // gigabytes. Only a count is refused — one copy of a register is
        // already in memory, however large it is.
        if count > 1 && count.saturating_mul(reg.text.len()) > PASTE_LIMIT {
            return self.notify("paste: too large", FlashKind::Error);
        }
        reg.text = reg.text.repeat(count.max(1));
        if reg.linewise {
            let cur = self.buffer.cursor;
            let line = if after { cur.line + 1 } else { cur.line };
            if line >= self.buffer.line_count() {
                // Appending past the last line: add a separator newline first.
                let end_line = self.buffer.line_count().saturating_sub(1);
                let tail = self.buffer.line_len(end_line);
                self.buffer.insert_str(Cursor::new(end_line, tail), "\n");
                self.buffer
                    .insert_str(Cursor::new(end_line + 1, 0), reg.text.trim_end_matches('\n'));
                self.buffer.cursor = Cursor::new(end_line + 1, self.first_non_blank(end_line + 1));
            } else {
                self.buffer.insert_str(Cursor::new(line, 0), &reg.text);
                self.buffer.cursor = Cursor::new(line, self.first_non_blank(line));
            }
        } else {
            let cur = self.buffer.cursor;
            let len = self.buffer.line_len(cur.line);
            let col = if after && len > 0 {
                (cur.col + 1).min(len)
            } else {
                cur.col
            };
            let at = Cursor::new(cur.line, col);
            if self.buffer.insert_str(at, &reg.text) {
                // On the last character of one line of text; at the start of
                // several, as vim leaves it.
                self.buffer.cursor = if reg.text.contains('\n') {
                    at
                } else {
                    let n = reg.text.chars().count();
                    Cursor::new(cur.line, col + n.saturating_sub(1))
                };
            }
        }
    }

    // ----------------------------------------------------------------- visual

    fn enter_visual(&mut self, mode: Mode) {
        // A selection dragged out of a counted insert ends the count: what
        // follows is no longer the text the count was for.
        self.insert_repeat = None;
        self.anchor = Some(self.buffer.cursor);
        self.mode = mode;
        self.visual_owner = Some((self.focus_pane, self.current()));
        // A keyboard `v` always returns to Normal. Only the mouse sets this,
        // and it sets it straight after this call.
        self.visual_resumes_insert = false;
        self.touch_status();
    }

    /// The selection's other end, in Visual mode only. `sync_after_input`
    /// drops the anchor outside Visual after each input; this covers the rest
    /// of the input that left Visual, before that runs.
    fn visual_anchor(&self) -> Option<Cursor> {
        match self.mode {
            Mode::Visual | Mode::VisualLine => self.anchor,
            _ => None,
        }
    }

    /// Leave Visual for whatever comes next: back into the insert session when
    /// the selection was dragged out of one, Normal otherwise.
    ///
    /// Every Visual exit funnels through here — `Esc`, the operators, and the
    /// writer verbs alike — so the resume is honoured once rather than
    /// remembered at each of them.
    fn end_visual(&mut self) {
        self.mode = if self.visual_resumes_insert {
            Mode::Insert
        } else {
            Mode::Normal
        };
        self.visual_resumes_insert = false;
        self.anchor = None;
    }

    fn leave_visual(&mut self) {
        self.end_visual();
        self.pending.reset();
    }

    fn visual_operate(&mut self, op: Operator) {
        let anchor = self.anchor.unwrap_or(self.buffer.cursor);
        let cursor = self.buffer.cursor;
        let linewise = matches!(self.mode, Mode::VisualLine);
        self.end_visual();
        if linewise {
            self.operate_linewise(op, anchor.line, cursor.line);
        } else {
            // Visual selections are inclusive of the cursor character.
            self.operate_charwise(op, anchor, cursor, true);
        }
    }

    /// The Visual meaning of a Normal-mode edit key, which the Visual table
    /// passes through unchanged (`bindings::visual`). Returns true when the
    /// key is handled here, and `run` goes no further.
    ///
    /// Without this, `p`, `~`, `r`, `J` and `D` ran their Normal action at
    /// the cursor and left Visual on, ignoring the selection.
    fn visual_key(&mut self, action: &Action, count: usize) -> bool {
        match action {
            Action::PasteAfter => self.visual_paste(true, count),
            Action::PasteBefore => self.visual_paste(false, count),
            Action::ToggleCase => self.visual_map_chars(|c, out| {
                if c.is_uppercase() {
                    out.extend(c.to_lowercase());
                } else if c.is_lowercase() {
                    out.extend(c.to_uppercase());
                } else {
                    out.push(c);
                }
            }),
            Action::ReplaceChar(r) => {
                let r = *r;
                self.visual_map_chars(move |_, out| out.push(r));
            }
            Action::JoinLines => {
                let (a, b) = self.visual_lines();
                self.end_visual();
                self.buffer.cursor = Cursor::new(a, 0);
                // At least two lines, as vim: `vJ` on one line joins the next.
                self.join_lines((b - a + 1).max(2));
            }
            // The capital letters work on whole LINES in Visual, whatever
            // kind of selection it is.
            Action::DeleteToEol | Action::DeleteCharBack => self.visual_lines_operate(Operator::Delete),
            Action::YankLine => self.visual_lines_operate(Operator::Yank),
            Action::ChangeToEol => self.visual_lines_operate(Operator::Change),
            // Insert before the selection's first character, or after its
            // last (in `V`, at the start of its first line or the end of its
            // last), as vim's `I`/`A` do outside block mode.
            Action::InsertLineStart | Action::AppendLineEnd => {
                let (_, end, lo, linewise) = self.visual_span();
                let (_, last) = self.visual_lines();
                self.end_visual();
                self.buffer.cursor = match (action, linewise) {
                    (Action::InsertLineStart, _) => lo,
                    (_, true) => Cursor::new(last, self.buffer.line_len(last)),
                    // After the last selected character; a selection ending on
                    // a line break appends at that line's end.
                    _ => {
                        let at = self.idx_to_cursor(end);
                        if end > 0 && self.buffer.rope.char(end - 1) == '\n' {
                            let l = at.line.saturating_sub(1);
                            Cursor::new(l, self.buffer.line_len(l))
                        } else {
                            at
                        }
                    }
                };
                self.enter_insert();
            }
            // Edits with no Visual meaning do nothing, rather than edit at
            // the cursor with a selection still showing.
            Action::OpenAbove
            | Action::OpenBelow
            | Action::Insert
            | Action::Append
            | Action::DeleteChar
            | Action::Redo
            | Action::Repeat => {}
            _ => return false,
        }
        true
    }

    /// The selection's first and last line.
    fn visual_lines(&self) -> (usize, usize) {
        let a = self.anchor.unwrap_or(self.buffer.cursor).line;
        let b = self.buffer.cursor.line;
        (a.min(b), a.max(b))
    }

    /// The selection as a char range `start..end`, the first character's
    /// position, and whether it is linewise. A linewise range takes each line
    /// whole, its last newline included when there is one.
    fn visual_span(&self) -> (usize, usize, Cursor, bool) {
        if matches!(self.mode, Mode::VisualLine) {
            let (a, b) = self.visual_lines();
            let start = self.buffer.rope.line_to_char(a);
            let end = if b < self.buffer.last_line() {
                self.buffer.rope.line_to_char(b + 1)
            } else {
                self.buffer.rope.len_chars()
            };
            return (start, end, Cursor::new(a, 0), true);
        }
        let anchor = self.anchor.unwrap_or(self.buffer.cursor);
        let cur = self.buffer.cursor;
        let lo = if (anchor.line, anchor.col) <= (cur.line, cur.col) { anchor } else { cur };
        let hi = if lo == anchor { cur } else { anchor };
        let start = self.buffer.char_index(lo);
        let end = (self.buffer.char_index(hi) + 1).min(self.buffer.rope.len_chars());
        (start, end, lo, false)
    }

    /// `~` and `r{c}` in Visual: rewrite every selected character but the
    /// line breaks, end Visual, and leave the cursor at the selection's start.
    fn visual_map_chars(&mut self, f: impl Fn(char, &mut String)) {
        let (start, end, lo, _) = self.visual_span();
        self.end_visual();
        let text = self.buffer.rope.slice(start..end).to_string();
        let mut mapped = String::with_capacity(text.len());
        for c in text.chars() {
            if c == '\n' {
                mapped.push(c);
            } else {
                f(c, &mut mapped);
            }
        }
        if mapped != text {
            self.buffer.delete_chars(start, end);
            self.buffer.insert_str(lo, &mapped);
        }
        self.buffer.cursor = lo;
    }

    /// `D`/`X`, `Y` and `C`/`S`/`R` in Visual: the operator on the selected
    /// lines.
    fn visual_lines_operate(&mut self, op: Operator) {
        let (a, b) = self.visual_lines();
        self.end_visual();
        self.operate_linewise(op, a, b);
    }

    /// `p` / `P` in Visual: replace the selection with the register, `count`
    /// copies of it. `p` puts what it replaced in the unnamed register, so
    /// the next `p` swaps it back; `P` leaves the registers as they were
    /// (vim 9), so one yank can replace several selections.
    fn visual_paste(&mut self, keep_replaced: bool, count: usize) {
        let Some(reg) = self.take_register() else {
            self.leave_visual();
            return;
        };
        if count > 1 && count.saturating_mul(reg.text.len()) > PASTE_LIMIT {
            self.leave_visual();
            return self.notify("paste: too large", FlashKind::Error);
        }
        let (start, mut end, lo, linewise) = self.visual_span();
        self.end_visual();
        // A charwise selection that ends on a line break (`vp` on an empty
        // line, `v$p`) replaces the text, not the break: the next line stays
        // where it is.
        if !linewise && end > start && self.buffer.rope.char(end - 1) == '\n' {
            end -= 1;
        }
        let ends_at_eof = end == self.buffer.rope.len_chars();
        let replaced = self.buffer.rope.slice(start..end).to_string();

        let reg_linewise = reg.linewise;
        let mut one = reg.text;
        if reg_linewise && !one.ends_with('\n') {
            one.push('\n');
        }
        let mut text = one.repeat(count.max(1));
        if reg_linewise && !linewise {
            // Lines into the middle of a line split it around them.
            text.insert(0, '\n');
        } else if !reg_linewise && linewise {
            // Text in place of lines is a line of its own.
            text.push('\n');
        }
        // Lines replacing the last line of the file must not leave a blank
        // one after them.
        if linewise && ends_at_eof && !replaced.ends_with('\n') && text.ends_with('\n') {
            text.pop();
        }

        self.buffer.delete_chars(start, end);
        self.buffer.insert_str(lo, &text);
        if keep_replaced {
            let mut replaced = replaced;
            if linewise && !replaced.ends_with('\n') {
                replaced.push('\n');
            }
            self.store_register(replaced, linewise, true);
        }
        self.buffer.cursor = if reg_linewise {
            let line = if linewise { lo.line } else { lo.line + 1 };
            let line = line.min(self.buffer.last_line());
            Cursor::new(line, self.first_non_blank(line))
        } else if text.trim_end_matches('\n').contains('\n') {
            // At the start of text that spans lines, as Normal `p` leaves it.
            lo
        } else {
            // On the last character of one line of text.
            let n = text.trim_end_matches('\n').chars().count();
            self.idx_to_cursor(start + n.saturating_sub(1))
        };
    }

    // ----------------------------------------------------------------- insert

    /// Enter in Insert mode. With `editor.auto_indent` the new line continues
    /// what the old one was: its leading whitespace, and — as the setting's own
    /// description promises — a list marker or quote prefix.
    ///
    /// Enter on an EMPTY item ends the list instead of laying down another
    /// marker, which is the only way out of one without reaching for Esc.
    fn newline_with_indent(&mut self) {
        let at = self.buffer.cursor;
        if !self.config.editor.auto_indent {
            if self.buffer.insert_str(at, "\n") {
                self.buffer.cursor = Cursor::new(at.line + 1, 0);
            }
            return;
        }
        // The block cache is refreshed by the RENDER path, so between two keys
        // in one event batch it can still describe the line before the last
        // edit. Ask for it here: it early-returns when the revisions match.
        self.refresh_blocks();
        let line = self.buffer.line_text(at.line);
        let ws: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let block = self.blocks.kinds.get(at.line).cloned();
        let prefix = match &block {
            Some(BlockKind::ListItem { marker, checked, .. }) => {
                Some(list_continuation(&line, *marker, *checked))
            }
            Some(BlockKind::Quote(_)) => quote_continuation(&line),
            _ => None,
        };

        let Some(prefix) = prefix else {
            let text = format!("\n{ws}");
            if self.buffer.insert_str(at, &text) {
                self.buffer.cursor = Cursor::new(at.line + 1, ws.chars().count());
            }
            return;
        };

        // An item with nothing but its marker: clear the line and stop the list
        // rather than adding an empty one below it.
        if line.chars().skip(prefix.chars().count()).all(char::is_whitespace)
            && at.col >= prefix.chars().count()
        {
            let start = self.buffer.char_index(Cursor::new(at.line, 0));
            let end = self.buffer.char_index(Cursor::new(at.line, line.chars().count()));
            self.buffer.delete_chars(start, end);
            self.buffer.cursor = Cursor::new(at.line, 0);
            let at = self.buffer.cursor;
            if self.buffer.insert_str(at, "\n") {
                self.buffer.cursor = Cursor::new(at.line + 1, 0);
            }
            return;
        }

        let text = format!("\n{prefix}");
        if self.buffer.insert_str(at, &text) {
            self.buffer.cursor = Cursor::new(at.line + 1, prefix.chars().count());
        }
    }

    /// `editor.auto_pair` — insert the closing half of a bracket or quote.
    ///
    /// Returns true when it handled the key. OFF by default, and the rules are
    /// shaped by why: this is a PROSE editor, and prose is full of unmatched
    /// quotes. So a quote never pairs against a word character (`don't` must
    /// stay `don't`, and a closing `"` after a word is a closing quote), and
    /// nothing pairs directly in front of one.
    fn auto_pair_insert(&mut self, c: char) -> bool {
        if !self.config.editor.auto_pair {
            return false;
        }
        let at = self.buffer.cursor;
        let line = self.buffer.line_text(at.line);
        let next = line.chars().nth(at.col);
        let prev = at.col.checked_sub(1).and_then(|i| line.chars().nth(i));

        // Typing the closer that is already sitting there steps over it, so
        // finishing a pair by hand does not leave a stray second one.
        if closer_of(c).is_some() && next == Some(c) {
            self.buffer.cursor.set_col(at.col + 1);
            return true;
        }
        let Some(close) = pair_for(c) else {
            return false;
        };
        // Wrapping existing text is what Visual `gb`/`gl` are for; here it
        // would just split a word in half.
        if next.is_some_and(char::is_alphanumeric) {
            return false;
        }
        // A self-closing mark (quote, backtick) after a word is punctuation.
        if close == c && prev.is_some_and(|p| p.is_alphanumeric()) {
            return false;
        }
        let text: String = [c, close].iter().collect();
        if self.buffer.insert_str(at, &text) {
            self.buffer.cursor.set_col(at.col + 1);
        }
        true
    }

    /// Backspace between the two halves of an empty pair removes both — the
    /// counterpart to `auto_pair_insert`, or undoing a mistyped `(` would leave
    /// its `)` behind.
    fn auto_pair_backspace(&mut self) -> bool {
        if !self.config.editor.auto_pair {
            return false;
        }
        let at = self.buffer.cursor;
        if at.col == 0 {
            return false;
        }
        let line = self.buffer.line_text(at.line);
        let open = line.chars().nth(at.col - 1);
        let close = line.chars().nth(at.col);
        if open.and_then(pair_for) != close || close.is_none() {
            return false;
        }
        let idx = self.buffer.char_index(at);
        if self.buffer.delete_chars(idx - 1, idx + 1).is_some() {
            self.buffer.cursor.set_col(at.col - 1);
        }
        true
    }

    /// `input.escape_alias` (default `jk`) — SPEC.md §7.2.
    ///
    /// Deliberately NOT a keymap entry: the first character must land in the
    /// buffer immediately, or typing "jam" would stutter while the editor waited
    /// to see whether a `k` was coming. So each character is inserted as normal
    /// and this runs AFTERWARDS; completing the alias removes what it just wrote
    /// and leaves Insert mode.
    ///
    /// `input.sequence_timeout_ms` bounds the gap BETWEEN characters, not the
    /// whole sequence: `j` … long pause … `k` is a writer typing two letters,
    /// and must stay two letters.
    fn feed_escape_alias(&mut self, typed: char) {
        let alias: Vec<char> = self.config.input.escape_alias.chars().collect();
        if alias.is_empty() || self.replaying_insert {
            return;
        }
        let timeout = Duration::from_millis(self.config.input.sequence_timeout_ms);
        let in_time = self
            .escape_since
            .is_some_and(|t| t.elapsed() <= timeout);

        self.escape_run = if self.escape_run > 0 && in_time && typed == alias[self.escape_run] {
            self.escape_run + 1
        } else if typed == alias[0] {
            // A stale or broken run still restarts here, so `jjk` works.
            1
        } else {
            0
        };
        self.escape_since = (self.escape_run > 0).then(Instant::now);

        if self.escape_run < alias.len() {
            return;
        }
        self.escape_run = 0;
        self.escape_since = None;

        // Take back exactly the characters the alias just wrote. They were all
        // typed as `Char`s in a row, so they are on this line, ending at the
        // cursor — anything else means something intervened and the run would
        // have been broken already.
        let at = self.buffer.cursor;
        let n = alias.len();
        if at.col < n {
            return;
        }
        // The alias's keys were recorded as typing; they are not text to repeat.
        if let Some(r) = self.insert_repeat.as_mut() {
            let keep = r.keys.len().saturating_sub(n);
            r.keys.truncate(keep);
        }
        let end = self.buffer.char_index(at);
        if self.buffer.delete_chars(end - n, end).is_some() {
            self.buffer.cursor.set_col(at.col - n);
        }
        self.leave_insert();
    }

    /// Esc, and everything that behaves like it: Normal mode with the cursor
    /// stepped back onto the last character it was sitting after.
    fn leave_insert(&mut self) {
        self.mode = Mode::Normal;
        self.escape_run = 0;
        self.escape_since = None;
        self.repeat_insert();
        // `cc` then `Esc` with nothing typed: the indent it laid down was
        // never used, so it goes, as vim's 'autoindent' takes it back.
        if let Some(line) = self.cc_indent_line.take() {
            let text = self.buffer.line_text(line);
            if line == self.buffer.cursor.line && !text.is_empty() && text.trim().is_empty() {
                let start = self.buffer.rope.line_to_char(line);
                let n = text.chars().count();
                self.buffer.delete_chars(start, start + n);
                self.buffer.cursor = Cursor::new(line, 0);
            }
        }
        if self.buffer.cursor.col > 0 {
            let col = self.buffer.cursor.col - 1;
            self.buffer.cursor.set_col(col);
        }
        self.auto_align_table();
        self.touch_status();
    }

    /// `3ix<Esc>` is `xxx`, `2otext<Esc>` two lines: the keys this insert
    /// session typed are replayed `count - 1` more times (each after a new
    /// line, for `o`/`O`), so auto-pair, Backspace and list continuation
    /// come out as they did the first time. A session that did anything but
    /// type (an arrow, a paste, a drag) dropped its count on the way.
    fn repeat_insert(&mut self) {
        let Some(r) = self.insert_repeat.take() else { return };
        let copies = r.count - 1;
        if r.keys.is_empty() || copies.saturating_mul(r.keys.len()) > INSERT_REPEAT_LIMIT {
            return;
        }
        // Typing that went back past where it began (Backspace over older
        // text) has no first copy to follow: nothing is repeated.
        let grown = self.buffer.rope.len_chars().checked_sub(r.len_before);
        let end = self.buffer.char_index(self.buffer.cursor);
        let Some(grown) = grown.filter(|_| r.line || end >= r.start) else { return };
        self.replaying_insert = true;
        for k in 1..=copies {
            if r.line {
                self.open_line(false);
            } else {
                self.buffer.cursor = self.idx_to_cursor(r.start + k * grown);
            }
            for &key in &r.keys {
                self.on_key_insert(key);
            }
        }
        self.replaying_insert = false;
        self.mode = Mode::Normal;
    }

    /// Keep a counted insert's typing for `repeat_insert`; anything that is
    /// not typing ends the count.
    fn record_insert_key(&mut self, key: KeyEvent) {
        if self.replaying_insert {
            return;
        }
        let Some(r) = self.insert_repeat.as_mut() else { return };
        let chord = key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab if !chord => {
                r.keys.push(key)
            }
            _ => self.insert_repeat = None,
        }
    }

    fn on_key_insert(&mut self, key: KeyEvent) {
        self.record_insert_key(key);
        // `[keys.insert]` first — the same "config overrides, built-ins remain"
        // rule Normal and Visual follow. Single keys only (`Keymap::insert_key`
        // says why), and only `Act` verbs: a motion or an operator has no
        // meaning while typing.
        if let Some(k) = Key::from_event(key) {
            if let Some(Verb::Act(action)) = self.keymap.insert_key(k).cloned() {
                self.escape_run = 0;
                self.escape_since = None;
                self.apply_action(action);
                return;
            }
        }
        // Anything that is not a plain character breaks a half-typed alias:
        // `j`, Backspace, `k` is not `jk`.
        if !matches!(key.code, KeyCode::Char(_)) {
            self.escape_run = 0;
            self.escape_since = None;
        }
        match key.code {
            KeyCode::Esc => self.leave_insert(),
            // Ctrl-C leaves without the count's copies, as in vim.
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_repeat = None;
                self.mode = Mode::Normal;
                self.escape_run = 0;
                self.escape_since = None;
                self.touch_status();
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.write(None, false);
            }
            // A control chord arrives as its plain letter plus CONTROL, so
            // without this `<C-a>` would type an `a`. Unbound chords do nothing.
            KeyCode::Char(_) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.escape_run = 0;
                self.escape_since = None;
            }
            KeyCode::Char(c) => {
                self.coalesce_insert(c.is_whitespace());
                if !self.auto_pair_insert(c) {
                    let at = self.buffer.cursor;
                    let mut s = [0u8; 4];
                    if self.buffer.insert_str(at, c.encode_utf8(&mut s)) {
                        let col = at.col + 1;
                        self.buffer.cursor.set_col(col);
                    }
                }
                self.feed_escape_alias(c);
            }
            KeyCode::Enter => {
                self.coalesce_insert(true);
                self.newline_with_indent();
            }
            KeyCode::Backspace => {
                if self.auto_pair_backspace() {
                    return;
                }
                let c = self.buffer.cursor;
                if c.col > 0 {
                    let idx = self.buffer.char_index(c);
                    if self.buffer.delete_chars(idx - 1, idx).is_some() {
                        self.buffer.cursor.set_col(c.col - 1);
                    }
                } else if c.line > 0 {
                    let prev_len = self.buffer.line_len(c.line - 1);
                    let idx = self.buffer.char_index(c);
                    if self.buffer.delete_chars(idx - 1, idx).is_some() {
                        self.buffer.cursor = Cursor::new(c.line - 1, prev_len);
                    }
                }
            }
            // On a list item, Tab and Shift-Tab move the whole item in and
            // out — what every writing app does — rather than type a tab.
            KeyCode::Tab | KeyCode::BackTab if self.list_tab_applies() => {
                let outdent = key.code == KeyCode::BackTab
                    || key.modifiers.contains(KeyModifiers::SHIFT);
                self.shift_list_item(outdent);
            }
            KeyCode::Tab => {
                let at = self.buffer.cursor;
                let text = if self.config.editor.expand_tab {
                    " ".repeat(self.config.editor.tab_width)
                } else {
                    "\t".to_string()
                };
                let n = text.chars().count();
                if self.buffer.insert_str(at, &text) {
                    self.buffer.cursor.set_col(at.col + n);
                }
            }

            // Cursor movement while inserting. The cursor may sit one past the
            // last character here (`Mode::allows_past_end`), so movement uses the
            // full line length, not length-1.
            KeyCode::Left => {
                let c = self.buffer.cursor;
                if c.col > 0 {
                    self.buffer.cursor.set_col(c.col - 1);
                } else if c.line > 0 {
                    let prev = c.line - 1;
                    self.buffer.cursor = Cursor::new(prev, self.buffer.line_len(prev));
                }
            }
            KeyCode::Right => {
                let c = self.buffer.cursor;
                let len = self.buffer.line_len(c.line);
                if c.col < len {
                    self.buffer.cursor.set_col(c.col + 1);
                } else if c.line + 1 < self.buffer.line_count() {
                    self.buffer.cursor = Cursor::new(c.line + 1, 0);
                }
            }
            KeyCode::Up => self.move_by(Motion::Up, 1),
            KeyCode::Down => self.move_by(Motion::Down, 1),
            KeyCode::Home => self.buffer.cursor.set_col(0),
            KeyCode::End => {
                let len = self.buffer.line_len(self.buffer.cursor.line);
                self.buffer.cursor.set_col(len);
            }
            KeyCode::PageUp => self.move_by(Motion::HalfPageUp, 1),
            KeyCode::PageDown => self.move_by(Motion::HalfPageDown, 1),
            KeyCode::Delete => {
                let idx = self.buffer.char_index(self.buffer.cursor);
                if idx < self.buffer.rope.len_chars() {
                    self.buffer.delete_chars(idx, idx + 1);
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------------------- command

    fn on_key_command(&mut self, key: KeyEvent, mut buf: String) {
        match field_key(key, &mut buf) {
            FieldKey::Cancel => {
                self.mode = Mode::Normal;
                self.flash = None;
            }
            FieldKey::Submit => {
                self.mode = Mode::Normal;
                self.run_command(&buf);
            }
            FieldKey::Stay => self.mode = Mode::Command(buf),
        }
    }

    // ------------------------------------------------------------ tree prompts

    /// A panel's one-line question. Swallows every key, exactly as `:` does:
    /// an open prompt is a text field, so `a` would otherwise re-open it.
    fn on_key_prompt(&mut self, key: KeyEvent, mut p: Prompt) {
        if p.kind.is_confirm() {
            // A plain `y` only: `Ctrl-y` is not a yes.
            let chord = key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') if !chord => {
                    self.mode = Mode::Normal;
                    self.finish_prompt(p);
                }
                // Anything that is not a yes is a no. A destructive answer must
                // be typed on purpose, never fallen into.
                _ => {
                    self.mode = Mode::Normal;
                    self.flash = None;
                }
            }
            return;
        }
        // `Ctrl-U` matters most here: `r` and `m` open PRE-FILLED, so
        // replacing the name outright needs to be one key.
        match field_key(key, &mut p.input) {
            FieldKey::Cancel => {
                self.mode = Mode::Normal;
                self.flash = None;
            }
            FieldKey::Submit => {
                self.mode = Mode::Normal;
                self.finish_prompt(p);
            }
            FieldKey::Stay => self.mode = Mode::Prompt(p),
        }
    }

    /// Carry out an answered prompt, report what happened, and put the tree's
    /// cursor where the user will look for it.
    fn finish_prompt(&mut self, p: Prompt) {
        // Export is not a tree operation and must work with the tree closed,
        // so it is answered before the tree is required.
        if let PromptKind::Export { format, justify } = p.kind {
            return self.finish_export(format, justify, &p.target, p.input.trim());
        }
        let Some(tree) = self.tree.as_ref() else { return };
        let root = tree.root.clone();
        let was = tree.selected;

        let outcome = match &p.kind {
            PromptKind::Create => {
                let raw = p.input.trim();
                if raw.is_empty() {
                    return;
                }
                // Neo-tree's convention: a trailing `/` means a directory. It
                // reads as one, which is the whole reason to use it.
                let is_dir = raw.ends_with('/');
                let path = p.target.join(expand_home(raw.trim_end_matches('/')));
                ops::create(&path, is_dir).map(|()| Some(path))
            }
            PromptKind::Rename => {
                let raw = p.input.trim();
                if raw.is_empty() {
                    return;
                }
                // A rename stays put; only the name changes. Anything with a
                // separator in it is a move, and `m` is where that lives.
                if raw.contains('/') {
                    self.notify("a rename is a name, not a path — use m to move", FlashKind::Error);
                    return;
                }
                let dest = p.target.with_file_name(raw);
                self.move_path(&p.target, &dest)
            }
            PromptKind::Move => {
                let raw = p.input.trim();
                if raw.is_empty() {
                    return;
                }
                let typed = expand_home(raw);
                let mut dest = if typed.is_absolute() {
                    typed
                } else {
                    self.tree.as_ref().map(|t| t.resolve(raw)).unwrap_or_else(|| root.join(raw))
                };
                // Moving ONTO a directory means "into it", the way `mv` does.
                if dest.is_dir() {
                    if let Some(name) = p.target.file_name() {
                        dest = dest.join(name);
                    }
                }
                self.move_path(&p.target, &dest)
            }
            PromptKind::Delete { .. } => ops::remove(&p.target).map(|()| None),
            // Answered at the top of this function, with no tree required.
            PromptKind::Export { .. } => return,
        };

        match outcome {
            Ok(landed) => {
                self.disk_changed();
                let verb = match p.kind {
                    PromptKind::Create => "created",
                    PromptKind::Rename => "renamed",
                    PromptKind::Move => "moved",
                    PromptKind::Delete { .. } => "deleted",
                    // Answered above, before the tree was required.
                    PromptKind::Export { .. } => unreachable!(),
                };
                let name = landed.as_ref().unwrap_or(&p.target);
                let shown = self
                    .tree
                    .as_ref()
                    .map(|t| t.relative(name))
                    .unwrap_or_else(|| name.display().to_string());
                if let Some(tree) = self.tree.as_mut() {
                    match &landed {
                        Some(path) => tree.reveal(path),
                        None => {
                            tree.refresh();
                            tree.select_nearest(was);
                        }
                    }
                }
                self.notify(format!("{shown} {verb}"), FlashKind::Info);
            }
            Err(e) => self.notify(format!("{e}"), FlashKind::Error),
        }
    }

    /// `:export [md|txt|html|pdf] [justify]` — open the save dialog for the
    /// finished doc. `justify` stretches every wrapped line but a
    /// paragraph's last to fill `layout.measure` — only `html` and `txt` have
    /// a layout of their own to stretch, so it is refused on `md` (raw
    /// markup, no visual layout) and `pdf` (pandoc's own template decides
    /// that).
    ///
    /// The document is flattened from what is ON DISK, so an unsaved buffer is
    /// refused rather than exported from a stale file. Silently exporting
    /// yesterday's version is the one outcome nobody could detect.
    fn open_export(&mut self, arg: &str) {
        let mut format_tok = None;
        let mut justify = false;
        for tok in arg.split_whitespace() {
            match tok.to_ascii_lowercase().as_str() {
                "justify" | "justified" => justify = true,
                _ if format_tok.is_none() => format_tok = Some(tok),
                _ => return self.notify(format!("export: unexpected {tok:?}"), FlashKind::Error),
            }
        }
        let format = match format_tok {
            None => Format::Markdown,
            Some(f) => match Format::parse(f) {
                Some(f) => f,
                None => {
                    return self
                        .notify(format!("export: {f:?} is not md, txt, html or pdf"), FlashKind::Error)
                }
            },
        };
        if justify && !matches!(format, Format::Html | Format::Text) {
            return self.notify("export: justify only applies to html or txt", FlashKind::Error);
        }
        let Some(src) = self.buffer.path.clone() else {
            return self.notify("export: save this file first", FlashKind::Error);
        };
        if self.buffer.modified {
            return self.notify("export: unsaved changes — :w first", FlashKind::Error);
        }
        // Suggested, not decided: the reader edits this before pressing Enter.
        // Shown relative to the working directory, because an absolute path in
        // a 60-column box is mostly directories they already know they are in.
        let suggestion = crate::export::default_path(&src, format);
        let shown = self.shown(&suggestion);
        self.mode = Mode::Prompt(Prompt {
            kind: PromptKind::Export { format, justify },
            target: src,
            input: shown,
        });
    }

    /// Carry out an answered export prompt.
    fn finish_export(&mut self, format: Format, justify: bool, source: &Path, dest: &str) {
        if dest.is_empty() {
            return;
        }
        let dest = self.typed_path(dest);
        // HTML is drawn from the theme AS AUTHORED, not from the running one:
        // `self.theme` has been adapted to this terminal's color depth, and a
        // page has no such limit to be adapted to.
        let page = crate::export::html::Page {
            theme: crate::render::theme::Theme::authored(&self.config.theme)
                .unwrap_or_else(|_| self.theme.clone()),
            measure: self.config.layout.measure,
            base: source
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .to_path_buf(),
            justify,
            md: self.config.markdown.for_export(),
        };
        match crate::export::write(source, &dest, format, &self.config.transclude, &page) {
            Err(e) => self.notify(format!("export: {e}"), FlashKind::Error),
            Ok(()) => {
                let name = dest
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dest.display().to_string());
                self.notify(format!("{name} exported"), FlashKind::Info);
            }
        }
    }

    /// Rename or move a path, carrying any OPEN buffer along with it.
    ///
    /// Without the second half, renaming a file you are editing would leave the
    /// buffer pointing at a name that no longer exists, and the next `:w` would
    /// silently recreate the old one.
    fn move_path(&mut self, from: &Path, to: &Path) -> anyhow::Result<Option<PathBuf>> {
        let (from, to) = (self.absolute(from), self.absolute(to));
        // Which buffers go along is decided BEFORE the rename, while `from`
        // still resolves: a buffer spelled through a symlinked folder (a
        // `/home` that is really `/var/home`) is matched by where it really
        // is. Not when `from` IS a symlink: moving a link leaves the file it
        // points at, and that file's buffer, where they are. A moved
        // DIRECTORY takes every buffer beneath it, not just an exact match.
        let is_link = std::fs::symlink_metadata(&from).is_ok_and(|m| m.file_type().is_symlink());
        let real_from = if is_link { None } else { std::fs::canonicalize(&from).ok() };
        let carried: Vec<(usize, PathBuf)> = self
            .docs
            .iter()
            .enumerate()
            .filter_map(|(i, doc)| {
                let path = doc.buffer.path.as_deref()?;
                let rest = match path.strip_prefix(&from) {
                    Ok(rest) => rest.to_path_buf(),
                    Err(_) => {
                        let real = std::fs::canonicalize(path).ok()?;
                        real.strip_prefix(real_from.as_deref()?).ok()?.to_path_buf()
                    }
                };
                Some((i, rest))
            })
            .collect();
        ops::rename(&from, &to)?;
        for (i, rest) in carried {
            self.docs[i].buffer.path = Some(if rest.as_os_str().is_empty() {
                to.clone()
            } else {
                to.join(rest)
            });
        }
        Ok(Some(to))
    }

    /// Open a tree prompt on the selected row, if the operation makes sense for
    /// it. The root row is deliberately untouchable: every other path in the
    /// pane is expressed relative to it.
    fn tree_prompt(&mut self, kind: PromptKind) {
        let Some(tree) = self.tree.as_ref() else { return };
        let Some(entry) = tree.selected_entry() else { return };

        let (target, input) = match kind {
            PromptKind::Create => (tree.target_dir(), String::new()),
            PromptKind::Rename => (entry.path.clone(), entry.name.clone()),
            PromptKind::Move => (entry.path.clone(), tree.relative(&entry.path)),
            PromptKind::Delete { .. } => (entry.path.clone(), String::new()),
            // `:export` opens its own prompt; the tree never raises one.
            PromptKind::Export { .. } => return,
        };
        if !matches!(kind, PromptKind::Create) && tree.selected_is_root() {
            self.notify("the tree root stays put", FlashKind::Error);
            return;
        }
        // Counted here rather than in the label, so a huge directory is walked
        // once and only when a deletion is actually being asked about.
        let kind = match kind {
            PromptKind::Delete { .. } => PromptKind::Delete {
                entries: ops::count_entries(&target, DELETE_COUNT_CAP),
            },
            other => other,
        };
        self.mode = Mode::Prompt(Prompt { kind, target, input });
    }

    // ----------------------------------------------------------------- search

    fn on_key_search(&mut self, key: KeyEvent, mut query: String, reverse: bool) {
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
    fn search_move(&mut self, backward: bool) {
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

    pub(crate) fn run_command(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        let (name, arg) = match cmd.split_once(char::is_whitespace) {
            Some((n, a)) => (n, a.trim()),
            None => (cmd, ""),
        };

        match name {
            "w" | "write" => {
                self.write(if arg.is_empty() { None } else { Some(arg) }, false);
            }
            // `!` is "I know it changed, write anyway" — the only way past the
            // external-modification guard.
            "w!" | "write!" => {
                self.write(if arg.is_empty() { None } else { Some(arg) }, true);
            }
            // `:saveas` switches the buffer to the new file; `:w <path>` only
            // writes a copy there.
            "sav" | "saveas" | "sav!" | "saveas!" => {
                if arg.is_empty() {
                    self.notify("usage: :saveas <path>", FlashKind::Error);
                } else {
                    self.save_as(arg, name.ends_with('!'));
                }
            }
            "q" => self.try_quit(false),
            // Capital Q leaves outright — the whole session, every buffer.
            // `qa`/`qall` are vim's spelling of the same thing.
            "Q" | "qa" | "qall" => self.quit_all(false),
            "Q!" | "qa!" | "qall!" => self.quit_all(true),
            "sp" | "split" => self.split_pane(false),
            "vs" | "vsp" | "vsplit" => self.split_pane(true),
            "close" | "clo" => {
                if !self.close_pane() {
                    self.notify("last pane — :q to quit", FlashKind::Info);
                }
            }
            "only" | "on" => self.only_pane(),
            "view" => self.set_view(!self.view),
            "bn" | "bnext" => self.cycle_buffer(true),
            "bp" | "bprev" | "bprevious" => self.cycle_buffer(false),
            "b" | "buffer" => {
                if arg.is_empty() {
                    self.notify(self.buffer_list(), FlashKind::Info);
                } else {
                    self.switch_by_name(arg);
                }
            }
            "ls" | "buffers" => self.notify(self.buffer_list(), FlashKind::Info),
            "bd" | "bdelete" => self.close_buffer(false),
            "bd!" | "bdelete!" => self.close_buffer(true),
            "e" | "edit" => {
                if arg.is_empty() {
                    self.notify("usage: :e <path>", FlashKind::Error);
                } else {
                    self.open_file(self.typed_path(arg));
                }
            }
            "q!" => self.try_quit(true),
            // With a path, `:wq` saves the buffer AS that file and then
            // quits: a copy (what `:w <path>` writes) would leave this file's
            // edits unsaved, and `:wq!` would then throw them away.
            "wq" | "x" => {
                if arg.is_empty() {
                    self.write(None, false);
                } else {
                    self.save_as(arg, false);
                }
                self.try_quit(false);
            }
            // The `!` forces the WRITE, not the quit: a write that fails
            // still keeps the editor open, or the unsaved text would be lost.
            "wq!" | "x!" => {
                let written = if arg.is_empty() {
                    self.write(None, true)
                } else {
                    self.save_as(arg, true)
                };
                if written {
                    self.try_quit(true);
                }
            }
            // Vim's abbreviations, so muscle memory lands: `:u`, `:un`, `:und`,
            // `:undo`, and `:red`/`:redo`. Not `:r` or `:re` — `:r` is `:read`
            // in vim, and taking it for undo here would train the wrong reflex
            // against every other editor.
            "u" | "un" | "und" | "undo" => self.undo_or_warn(),
            "red" | "redo" => self.redo_or_warn(),
            "reload" | "e!" => self.reload_config(),
            // NOT `:e!`, which this editor already spends on the config. A
            // command that re-reads the file needs its own name rather than a
            // second meaning for one that is taken.
            "diff" | "diffsplit" => self.open_diff(),
            "revert" => self.revert(false),
            "revert!" => self.revert(true),
            "help" | "h" => self.open_help(arg),
            "focus" => {
                let mode = if arg.is_empty() {
                    Some(self.focus.next())
                } else {
                    FocusMode::parse(arg)
                };
                match mode {
                    Some(m) => self.set_focus(m),
                    None => self.notify(
                        format!("focus: off · paragraph · sentence, not {arg:?}"),
                        FlashKind::Error,
                    ),
                }
            }
            "typewriter" | "tw" => {
                self.config.layout.typewriter = !self.config.layout.typewriter;
                let on = if self.config.layout.typewriter { "on" } else { "off" };
                self.notify(format!("typewriter {on}"), FlashKind::Info);
            }
            "number" | "nu" => self.set_numbers(arg),
            "align" => self.set_align(arg),
            "export" => self.open_export(arg),
            // §14.3. Also re-reads every target, so it doubles as the way to
            // refresh an expansion after editing the file it came from.
            "embed" => self.set_embed_mode(arg),
            "set" => self.set_option(arg),
            "zen" => self.apply_action(Action::ToggleZen),
            // Through `run`, so read-only mode refuses it like any edit.
            "fixlist" => self.run(Command::bare(Action::FixList)),
            "" => {}
            other => self.notify(format!("not a command: {other}"), FlashKind::Error),
        }
    }

    /// `:embed [off|short|long|full]` — how much of an embed to expand
    /// (SPEC.md §14.3).
    ///
    /// With no argument it TOGGLES, and turning it back on returns to whichever
    /// mode was last chosen: someone reading in `full` who glances at the raw
    /// source expects `:embed` twice to put them back where they were.
    fn set_embed_mode(&mut self, arg: &str) {
        use crate::transclude::Mode;
        let next = if arg.trim().is_empty() {
            if self.embed_mode.is_on() {
                Mode::Off
            } else {
                self.last_embed_mode
            }
        } else {
            match Mode::parse(arg) {
                Some(m) => m,
                None => {
                    return self.notify(
                        format!("embed: {arg:?} is not none, short, rec or full"),
                        FlashKind::Error,
                    )
                }
            }
        };
        if next.is_on() {
            self.last_embed_mode = next;
        }
        self.embed_mode = next;
        self.notify(format!("embed {}", next.name()), FlashKind::Info);
    }

    /// `:number [relative|absolute]` — the line-number gutter.
    ///
    /// With no argument it TOGGLES, and turning it back on returns to whichever
    /// mode was last chosen: someone who switched to `relative` for a `5dj`
    /// expects `:number` twice to put them back in `relative`, not to default
    /// to `absolute`.
    fn set_numbers(&mut self, arg: &str) {
        let next = if arg.trim().is_empty() {
            if self.numbers.is_on() {
                NumberMode::Off
            } else {
                self.last_numbers
            }
        } else {
            match NumberMode::parse(arg) {
                Some(m) => m,
                None => {
                    return self.notify(
                        format!("number: off · relative · absolute, not {arg:?}"),
                        FlashKind::Error,
                    )
                }
            }
        };
        if next.is_on() {
            self.last_numbers = next;
        }
        self.numbers = next;
        self.notify(format!("number: {}", next.label()), FlashKind::Info);
    }

    /// `:align [left|center|right|justified]` — how each row of text sits
    /// within the writing column (SPEC.md §6). Column placement itself
    /// (`layout.align`, `center | left`) is untouched by this.
    ///
    /// With no argument it CYCLES left → center → right → justified → left,
    /// same shape as `:focus`: there is no single "off" state to toggle back
    /// to, so cycling is the only motion that reaches every value from a key
    /// press alone.
    fn set_align(&mut self, arg: &str) {
        let next = if arg.trim().is_empty() {
            self.text_align.next()
        } else {
            match TextAlign::parse(arg) {
                Some(a) => a,
                None => {
                    return self.notify(
                        format!("align: left · center · right · justified, not {arg:?}"),
                        FlashKind::Error,
                    )
                }
            }
        };
        self.text_align = next;
        self.notify(format!("align: {}", next.label()), FlashKind::Info);
    }

    /// `:set <key> [on|off]` or `:set <key>=<value>` — toggle when no value.
    ///
    /// A NUMERIC option reports its current value instead of toggling when no
    /// value is given, since there is nothing to flip. The render cache keys its
    /// entries on the measure, so changing it re-wraps on the next frame with no
    /// help from here.
    fn set_option(&mut self, arg: &str) {
        let (key, val) = match arg.split_once(['=', ' ', '\t']) {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (arg.trim(), ""),
        };
        let resolve = |v: &str, cur: bool| match v {
            "" => !cur,
            "on" | "true" | "1" | "yes" => true,
            _ => false,
        };
        match key {
            // The text measure — `layout.measure` in the config file, and the
            // one setting a writer wants to try on rather than commit to.
            "measure" | "width" => {
                let cur = self.config.layout.measure;
                if val.is_empty() {
                    return self.notify(format!("measure={cur}"), FlashKind::Info);
                }
                let Ok(n) = val.parse::<u16>() else {
                    return self.notify(format!("measure: {val:?} is not a number"), FlashKind::Error);
                };
                if n < MIN_MEASURE {
                    return self.notify(
                        format!("measure: {MIN_MEASURE} columns is the minimum"),
                        FlashKind::Error,
                    );
                }
                self.config.layout.measure = n;
                return self.notify(format!("measure={n}"), FlashKind::Info);
            }
            // Line height on a grid that has none: blank rows after each line.
            // Numeric, so like `measure` it reports rather than toggles.
            "line_spacing" | "spacing" => {
                let cur = self.config.layout.line_spacing;
                if val.is_empty() {
                    return self.notify(format!("line_spacing={cur}"), FlashKind::Info);
                }
                let Ok(n) = val.parse::<u16>() else {
                    return self.notify(
                        format!("line_spacing: {val:?} is not a number"),
                        FlashKind::Error,
                    );
                };
                self.config.layout.line_spacing = n.min(crate::render::layout::MAX_LINE_SPACING);
                let set = self.config.layout.line_spacing;
                return self.notify(format!("line_spacing={set}"), FlashKind::Info);
            }
            // Reports on/off rather than falling through to the generic
            // "set autosave", which for a toggle says everything except which
            // way it went — and this is a toggle that writes files.
            "autosave" => {
                let on = resolve(val, self.config.editor.autosave);
                self.config.editor.autosave = on;
                self.arm_autosave();
                let every = crate::fs::save::Autosave::from_config(&self.config.editor)
                    .every
                    .minutes();
                return match on {
                    true => self.notify(format!("autosave on · every {every} min"), FlashKind::Info),
                    false => self.notify("autosave off", FlashKind::Info),
                };
            }
            "autoreload" => {
                let on = resolve(val, self.config.editor.autoreload);
                self.config.editor.autoreload = on;
                return match on {
                    true => self.notify("autoreload on", FlashKind::Info),
                    // Leave any standing conflict flag alone: the files really
                    // did diverge, and turning the watcher off does not undo it.
                    false => self.notify("autoreload off", FlashKind::Info),
                };
            }
            "clipboard" | "cb" => {
                let on = resolve(val, self.config.editor.clipboard);
                self.config.editor.clipboard = on;
                return match on {
                    true => self.notify("clipboard on · y d p use the system clipboard", FlashKind::Info),
                    false => self.notify("clipboard off · \"+y \"+p still reach it", FlashKind::Info),
                };
            }
            // Minutes, so it reports rather than toggles — like `measure`.
            // Setting it does NOT turn autosave on: the interval is a
            // preference, switching it on is a decision.
            "autosave_interval" | "autosave_min" => {
                use crate::fs::save::AutosaveInterval;
                let cur = self.config.editor.autosave_interval;
                if val.is_empty() {
                    return self.notify(format!("autosave_interval={cur}"), FlashKind::Info);
                }
                let n = val.parse::<u8>().ok().and_then(AutosaveInterval::parse);
                let Some(n) = n else {
                    return self.notify(
                        format!(
                            "autosave_interval: {} to {} minutes, not {val:?}",
                            AutosaveInterval::MIN,
                            AutosaveInterval::MAX
                        ),
                        FlashKind::Error,
                    );
                };
                self.config.editor.autosave_interval = n.minutes();
                self.arm_autosave();
                return self.notify(
                    format!("autosave_interval={}", n.minutes()),
                    FlashKind::Info,
                );
            }
            "code_syntax" | "syntax" => {
                self.config.markdown.code_syntax = resolve(val, self.config.markdown.code_syntax);
            }
            // The hint bar. Says which way it went: turned off, the row just
            // goes quiet, which would look like nothing happened.
            "hints" => {
                let on = resolve(val, self.config.hints.enabled);
                self.config.hints.enabled = on;
                let msg = if on { "hints on" } else { "hints off · [hints] enabled = false keeps it off" };
                return self.notify(msg, FlashKind::Info);
            }
            "typewriter" | "tw" => {
                self.config.layout.typewriter = resolve(val, self.config.layout.typewriter);
            }
            "conceal" => {
                self.config.layout.conceal = resolve(val, self.config.layout.conceal);
            }
            "embed" => return self.set_embed_mode(val),
            "numbers" | "number" | "nu" => return self.set_numbers(val),
            "align" => return self.set_align(val),
            "mouse" => {
                self.config.input.mouse = resolve(val, self.config.input.mouse);
                self.set_mouse_capture(self.config.input.mouse);
            }
            "focus" => {
                let mode = if val.is_empty() {
                    Some(self.focus.next())
                } else {
                    FocusMode::parse(val)
                };
                match mode {
                    Some(m) => self.set_focus(m),
                    None => self.notify(
                        format!("focus: off · paragraph · sentence, not {val:?}"),
                        FlashKind::Error,
                    ),
                }
                return;
            }
            other => {
                self.notify(format!("unknown option: {other}"), FlashKind::Error);
                return;
            }
        }
        self.notify(format!("set {key}"), FlashKind::Info);
    }

    /// `:w`. True when the file was written.
    ///
    /// `:w <path>` on a buffer that already has a file writes a COPY and keeps
    /// editing the original, as vim does: `:w backup.md` is a checkpoint, not
    /// a move. An unnamed buffer takes the name instead, which is how it gets
    /// one (`:saveas` switches a named buffer).
    fn write(&mut self, path: Option<&str>, force: bool) -> bool {
        if let Some(typed) = path {
            if self.buffer.path.is_none() {
                return self.save_as(typed, force);
            }
            let p = self.typed_path(typed);
            if !self.buffer.is_own_file(&p) {
                return self.write_copy(&p, force);
            }
        }
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        let result = self.buffer.save(policy, force);
        self.after_write(result)
    }

    /// `:saveas <path>`, and `:w <path>` on an unnamed buffer: write there and
    /// make it this buffer's file, styled by its new extension.
    fn save_as(&mut self, typed: &str, force: bool) -> bool {
        let p = self.typed_path(typed);
        // A file is edited in ONE buffer (`open_file` holds to that); taking
        // over another buffer's file would leave two copies racing to save.
        let current = self.current();
        if self.docs.iter().enumerate().any(|(i, d)| i != current && d.buffer.is_own_file(&p)) {
            self.notify(format!("{} is open in another buffer", self.shown(&p)), FlashKind::Error);
            return false;
        }
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        let result = self.buffer.save_as(p.clone(), policy, force);
        if result.is_ok() {
            let exts = &self.config.markdown.plain_text_extensions;
            self.buffer.syntax = crate::fs::open::syntax_for(&p, exts);
        }
        self.after_write(result)
    }

    /// `:w <path>` from a buffer with its own file: the text goes to `path`,
    /// and the buffer stays what it was.
    fn write_copy(&mut self, path: &Path, force: bool) -> bool {
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        match self.buffer.write_copy(path, policy, force) {
            Ok(()) => {
                self.disk_changed();
                let mine = self.buffer.display_name();
                self.notify(
                    format!("{} written (still editing {mine})", self.shown(path)),
                    FlashKind::Info,
                );
                true
            }
            Err(e) => {
                self.notify(format!("{e}"), FlashKind::Error);
                false
            }
        }
    }

    /// Report a write of this buffer's own file.
    fn after_write(&mut self, result: anyhow::Result<()>) -> bool {
        match result {
            Ok(()) => {
                self.claim_save_revisions();
                self.disk_changed();
                let name = self.buffer.display_name();
                let words = self.buffer.word_count();
                self.notify(format!("{name} written · {words} words"), FlashKind::Info);
                // The clock starts from the last write, not from startup.
                self.arm_autosave();
                true
            }
            Err(e) => {
                self.notify(format!("{e}"), FlashKind::Error);
                false
            }
        }
    }
}

/// The marker prefix the next list item should open with: the same indent, the
/// same bullet — or the NEXT ordinal, since a numbered list that repeats `1.`
/// renumbers itself the moment it is rendered. A task continues as an EMPTY
/// box; carrying a tick over would tick off work nobody did.
fn list_continuation(line: &str, marker: Marker, checked: Option<bool>) -> String {
    let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
    let body = &line[indent.len()..];
    let mut out = indent;
    match marker {
        Marker::Ordered => {
            let digits: String = body.chars().take_while(char::is_ascii_digit).collect();
            let n: usize = digits.parse().unwrap_or(0);
            let sep = body.chars().nth(digits.len()).filter(|c| matches!(c, '.' | ')'));
            out.push_str(&format!("{}{} ", n + 1, sep.unwrap_or('.')));
        }
        Marker::Dash => out.push_str("- "),
        Marker::Star => out.push_str("* "),
        Marker::Plus => out.push_str("+ "),
    }
    if checked.is_some() {
        out.push_str("[ ] ");
    }
    out
}

/// The `>` run a quote continues with, spaced as the line had it.
fn quote_continuation(line: &str) -> Option<String> {
    let mut out = String::new();
    for c in line.chars() {
        match c {
            ' ' | '\t' | '>' => out.push(c),
            _ => break,
        }
    }
    out.contains('>').then_some(out)
}

/// The closing half of an auto-pair opener. Quotes and backticks close
/// themselves; brackets have a distinct partner.
fn pair_for(c: char) -> Option<char> {
    Some(match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' => '"',
        '\'' => '\'',
        '`' => '`',
        _ => return None,
    })
}

/// Whether this character is the CLOSING half of a pair — the set typing can
/// step over rather than duplicate.
fn closer_of(c: char) -> Option<char> {
    Some(match c {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        '"' | '\'' | '`' => c,
        _ => return None,
    })
}

/// `"+` and `"*`: the system clipboard.
fn is_clipboard_register(name: char) -> bool {
    matches!(name, '+' | '*')
}

/// The ordered levels containing `lines`, each as its item lines and the
/// number it starts from. What `App::renumber_lists` keeps a list's start by.
fn list_starts(lists: &Lists, lines: &[usize]) -> Vec<(Vec<usize>, u64)> {
    lines
        .iter()
        .filter_map(|&l| {
            let items = lists.level_items(l);
            let (start, _) = lists.ordinal(*items.first()?)?;
            Some((items, start))
        })
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod fuzz;

#[cfg(test)]
mod perf;
