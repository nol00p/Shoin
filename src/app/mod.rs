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

    /// Never shared with another document, and never reused, so a buffer
    /// revision can say which document it counts (see `DotMark`).
    id: u64,

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
    fn new(buffer: Buffer, math: bool, tab_width: usize) -> BufferState {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        BufferState {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            blocks: BlockCache::build(&buffer, math, tab_width),
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
    /// A Tab completion cycle on the `:` line, while one is under way.
    pub(crate) completion: Option<complete::Completion>,

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
    /// The document and its revision when the in-flight command began, to
    /// tell if it edited.
    dot_rev: DotMark,
    /// Where the keys of the command under way start in `recording`: a `.`
    /// in Visual takes its own keys back out.
    command_start: usize,
    /// A selection the mouse made, not yet written into `recording` as the
    /// keys that would make it again.
    drag_extent: bool,
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
    /// A selection the mouse made, made again from the cursor: as many
    /// characters on (`lines == 0`), or as many lines down to column `cols`,
    /// with the cursor at the start when it was dragged leftwards. Written
    /// as a selection rather than as keys, so a remapped `l` or `o` cannot
    /// change it, and an end past the last character survives.
    Select { lines: usize, cols: usize, backwards: bool },
}

/// Where `.` measures "did this command edit?" from: a revision of ONE
/// document. Revisions count per document, so a revision taken in one says
/// nothing about another, and a command that ends in a different document
/// than it began in edited nothing there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DotMark {
    doc: u64,
    rev: u64,
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
        Some(Replay::Paste(_) | Replay::Select { .. }) | None => return keys.to_vec(),
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
mod complete;

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
        let mut app = App {
            active: ActiveSet {
                start: buffer.cursor.line,
                end: buffer.cursor.line,
            },
            theme: Theme::from_config(&config.theme).unwrap_or_default(),
            docs: vec![BufferState::new(buffer, config.markdown.math, config.editor.tab_width)],
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
            completion: None,
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
            dot_rev: DotMark::default(),
            command_start: 0,
            drag_extent: false,
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
        };
        // Edits are counted from the first document as it opened.
        app.mark_dot();
        Ok(app)
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
            let can_show = self.can_show_notice();
            if let Some(text) = crate::recover::tick(self.docs.iter().map(|d| &d.buffer), can_show) {
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

    /// Keep the block cache in step with the buffer.
    ///
    /// Incremental: edits record the lines they touched (`Buffer::take_dirty`),
    /// so the rescan resumes from the first one's cached carry state and stops
    /// once past the last one the carry re-converges. Only when no
    /// line was reported (a wholesale buffer swap, or a cache that never
    /// matched the document's length) does it fall back to a full rebuild.
    pub(crate) fn refresh_blocks(&mut self) {
        // `[markdown] math` decides what a `$$` line opens and `tab_width`
        // how deep a tabbed line is; a cache built under other values is
        // wrong everywhere, not just at an edit. Every document, since a
        // split pane shows another one's kinds and a config reload (`:e!`)
        // changes the settings for them all.
        let (math, tab_width) = (self.config.markdown.math, self.config.editor.tab_width);
        if !self.blocks.built_for(math, tab_width) {
            for doc in self.docs.iter_mut().filter(|d| !d.blocks.built_for(math, tab_width)) {
                doc.blocks = BlockCache::build(&doc.buffer, math, tab_width);
                doc.buffer.clear_dirty();
            }
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
                doc.blocks = BlockCache::build(&doc.buffer, self.config.markdown.math, self.config.editor.tab_width);
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
                        doc.blocks = BlockCache::build(&doc.buffer, math, self.config.editor.tab_width);
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

    /// Drop a flash whose time is up, reporting whether it actually cleared —
    /// the status line needs a repaint when it does.
    fn expire_flash(&mut self) -> bool {
        if self.flash.as_ref().is_some_and(|f| Instant::now() >= f.expires_at) {
            self.flash = None;
            return true;
        }
        false
    }

    /// Whether a recovery notice may take the message line: not while a
    /// message is up, so a second notice does not replace the first a pass
    /// (100 ms) later. It waits for that one to time out or be cleared.
    fn can_show_notice(&self) -> bool {
        self.flash.as_ref().is_none_or(|f| f.text.is_none())
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
            self.end_recording();
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
            self.drag_extent = false;
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
        if let Some(res) = motion::resolve(&self.buffer, m, count, page, false, self.config.editor.tab_width) {
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

        let window = ends_command(&cmd.action);
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
        if window {
            self.end_recording();
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
            Action::Command => {
                self.completion = None;
                self.mode = Mode::Command(String::new());
            }
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
                self.mark_dot();
            }
            self.describe_drag();
            if self.pending.is_clean() {
                self.command_start = self.recording.len();
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
            self.end_recording();
        }

        // Seal the step unless we are still inserting.
        if !matches!(self.mode, Mode::Insert) {
            self.buffer.history.end_group();
        }

        // Command finished (back to a clean boundary): if it edited, remember it.
        if !self.replaying && self.is_clean() && self.edited_since_mark() {
            self.dot = std::mem::take(&mut self.recording);
        }
    }

}

/// Window, buffer and panel actions: each ends the command under way, whether
/// or not it went anywhere (`App::end_recording`), so none of them is ever
/// the change `.` repeats. The one list of them, for every way an action
/// runs: the grammar, `[keys.*]` bindings in any mode.
fn ends_command(action: &Action) -> bool {
    matches!(
        action,
        Action::Window(_)
            | Action::ToggleSplit { .. }
            | Action::ClosePane
            | Action::OnlyPane
            | Action::FollowLink
            | Action::AlternateBuffer
            | Action::FindBuffer
            | Action::FindFile { .. }
            | Action::FileTree { .. }
            | Action::Quit { .. }
            | Action::WriteQuit
    )
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
