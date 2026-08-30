//! Application state and the event loop. SPEC.md §9.
//!
//! Build-order steps 1-3. Key handling here is direct: the operator-pending
//! machine (`input::pending`) and the configurable keymap (`input::keymap`)
//! take over in steps 7 and 9 respectively, at which point `on_key` shrinks to
//! a dispatch into `Action`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
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

use crate::config::{Config, ConfigWatcher};
use crate::help::Help;
use crate::finder::{self, Finder};
use crate::tree::{Activate, FileTree};
use crate::input::action::{Action, Operator, Root, Target};
use crate::input::bindings::{Command, Verb};
use crate::input::keymap::Keymap;
use crate::input::mode::{Mode, Prompt, PromptKind};
use crate::input::pending::{Key, Pending, Resolution, Table};
use crate::render::cache::RenderCache;
use crate::render::conceal::ActiveSet;
use crate::render::focus::{FocusMode, FocusRegion};
use crate::render::pane::{Dir, Node, Pane, PaneId};
use crate::render::frame;
use crate::render::markdown::block::{BlockCache, BlockKind, Marker};
use crate::render::markdown::inline::{self, Inline};
use crate::render::theme::Theme;
use crate::fs::ops;
use crate::export::Format;
use crate::text::buffer::Buffer;
use crate::text::cursor::Cursor;
use crate::text::motion::{self, Motion};
use crate::text::object;
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

    /// Lowest line edited since this document's render cache last synced, so it
    /// can splice its entries to follow the shift instead of re-parsing what
    /// moved. Accumulated from `Buffer::dirty_line` in `refresh_blocks`.
    render_dirty: Cell<Option<usize>>,
}

impl BufferState {
    /// Hand the render cache the lowest line edited since it last synced, and
    /// clear it. Read through `&self` because the cache syncs from the render
    /// path.
    pub fn take_render_dirty(&self) -> Option<usize> {
        self.render_dirty.take()
    }

    fn new(buffer: Buffer) -> BufferState {
        BufferState {
            blocks: BlockCache::build(&buffer),
            buffer,
            cache: RefCell::new(RenderCache::default()),
            render_dirty: Cell::new(None),
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
    /// See `docs/history/IDEAS.md` #5 for why the tree is a sidebar and not a leaf.
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
    /// How much of an `![[…]]` line expands in place. Starts at
    /// `transclude.embed`; `:embed [mode]` changes it.
    pub embed_mode: crate::transclude::Mode,
    /// The mode a bare `:embed` turns back on, so toggling off and on again
    /// returns to `rec` rather than dropping to `short`.
    last_embed_mode: crate::transclude::Mode,
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
    /// Visual-mode selection anchor; `None` outside Visual/VisualLine.
    pub anchor: Option<Cursor>,

    /// Live config-file watcher; `None` when running on built-in defaults.
    watcher: Option<ConfigWatcher>,

    /// Focus mode and its cached bright region. SPEC.md §6.
    pub focus: FocusMode,
    pub focus_region: Option<FocusRegion>,

    /// The last executed search; drives `n`/`N` and match highlighting.
    pub search: Option<Search>,

    // --- `.` repeat (SPEC.md §7.3) ---
    /// Keys of the last change, replayed by `.`.
    dot: Vec<KeyEvent>,
    /// Keys of the command currently in flight.
    recording: Vec<KeyEvent>,
    /// Buffer revision when the in-flight command began, to tell if it edited.
    dot_rev: u64,
    /// True while replaying `dot`, so the replay is not itself recorded.
    replaying: bool,

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

/// The last executed search, for `n`/`N` repeats and match highlighting.
#[derive(Clone)]
pub struct Search {
    pub pattern: String,
    /// The original direction (`/` vs `?`); `n` follows it, `N` reverses it.
    pub reverse: bool,
}

/// First index at or after `from` where `needle` occurs in `hay`.
fn char_find(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == needle[..])
}

/// Last index at or before `before` where `needle` occurs in `hay`.
fn char_rfind(hay: &[char], needle: &[char], before: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    let max = before.min(hay.len() - needle.len());
    (0..=max).rev().find(|&i| hay[i..i + needle.len()] == needle[..])
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

/// Extensions a dead link may CREATE. A missing note is a note to write; a
/// missing `report.pdf` is a missing file, and touching an empty one in its
/// place would answer a question nobody asked.
const CREATABLE: &[&str] = &["md", "markdown", "txt"];

/// A link target with no extension means a note — the same default
/// `link::candidates` resolves with, so what `gf` creates is what the link
/// finds next time.
fn with_default_ext(path: PathBuf) -> PathBuf {
    match path.extension() {
        Some(_) => path,
        None => path.with_extension("md"),
    }
}

fn creatable(path: &Path) -> bool {
    match path.extension() {
        None => true,
        Some(e) => CREATABLE.contains(&e.to_string_lossy().to_lowercase().as_str()),
    }
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

impl App {
    pub fn new(
        config: Config,
        file: Option<PathBuf>,
        config_explicit: Option<PathBuf>,
    ) -> Result<Self> {
        let buffer = match file {
            Some(path) => Buffer::open(path, &config.markdown.plain_text_extensions)?,
            None => Buffer::empty(),
        };
        let config_focus = config.layout.focus.clone();
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
        // A failed watch is not fatal — the editor just runs without hot reload.
        let watcher = ConfigWatcher::new(config_explicit).ok().flatten();
        Ok(App {
            active: ActiveSet {
                start: buffer.cursor.line,
                end: buffer.cursor.line,
            },
            theme: Theme::from_config(&config.theme).unwrap_or_default(),
            docs: vec![BufferState::new(buffer)],
            alternate: None,
            layout: Node::leaf(1, 0),
            focus_pane: 1,
            next_pane_id: 2,
            splash_art,
            configured,
            embed_mode: crate::transclude::Mode::parse(&config.transclude.embed)
                .unwrap_or_default(),
            last_embed_mode: crate::transclude::Mode::Short,
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
            anchor: None,
            watcher,
            focus: FocusMode::parse(&config_focus).unwrap_or(FocusMode::Off),
            focus_region: None,
            search: None,
            dot: Vec::new(),
            recording: Vec::new(),
            dot_rev: 0,
            replaying: false,
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
            finder: None,
            last_insert: None,
            zen: None,
            needs_redraw: true,
        })
    }

    pub fn run_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        if let Ok(size) = terminal.size() {
            self.term_size = (size.width, size.height);
        }
        while !self.quit {
            self.refresh_blocks();
            self.refresh_focus();
            self.sync_cursor_shape();
            if self.needs_redraw {
                {
                    let snapshot = &*self;
                    terminal.draw(|f| frame::render(f, snapshot))?;
                }
                // Only after a redraw, which is the whole economy of it: the
                // payload goes down the wire once per frame that changed, not
                // once per frame. ratatui has just repainted the cells the
                // pictures sat on, so they have to go back.
                self.paint_images();
                self.needs_redraw = false;
            }
            self.step()?;
        }
        // Hand the caret back to the terminal's own default on the way out.
        let _ = execute!(std::io::stdout(), SetCursorStyle::DefaultUserShape);
        set_cursor_color(None);
        Ok(())
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
    fn paint_images(&self) {
        use std::io::Write;
        let mut places = std::mem::take(&mut *self.images.borrow_mut());
        if self.image_protocol == crate::image::Protocol::None {
            return;
        }
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

        if command != self.cursor_colored {
            self.cursor_colored = command;
            set_cursor_color(command.then(|| cursor_blue(&self.theme)));
        }
    }

    /// Keep the block cache in step with the buffer.
    ///
    /// Incremental: edits record the lowest line they touched in
    /// `Buffer::dirty_line`, so the rescan resumes from that line's cached
    /// carry state and stops as soon as the carry re-converges. Only when no
    /// line was reported (a wholesale buffer swap, or a cache that never
    /// matched the document's length) does it fall back to a full rebuild.
    pub(crate) fn refresh_blocks(&mut self) {
        // Both caches want the edited line, but only one can consume it, so it
        // is forwarded to the render cache here on its way past.
        if let Some(line) = self.buffer.dirty_line {
            let merged = match self.render_dirty.get() {
                Some(prev) => prev.min(line),
                None => line,
            };
            self.render_dirty.set(Some(merged));
        }
        if self.blocks.revision == self.buffer.revision {
            self.buffer.dirty_line = None;
            return;
        }
        match self.buffer.dirty_line.take() {
            // `invalidate_from` takes the EDITED line — it resumes a line
            // earlier itself — and falls back to a full build when it has no
            // valid carry state to resume from (the head of the file, or a
            // shorter cache).
            Some(line) => {
                // Split the borrow: `blocks` and `buffer` are two fields of one
                // document, and the deref that reaches them takes all of `self`.
                let i = self.current();
                let doc = &mut self.docs[i];
                doc.blocks.invalidate_from(&doc.buffer, line);
            }
            None => {
                let i = self.current();
                let doc = &mut self.docs[i];
                doc.blocks = BlockCache::build(&doc.buffer);
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
                self.config = new;
                // A reload re-reads the file, which knows nothing about `--zen`
                // or a `:zen` toggle — put those back on top of it.
                self.reapply_zen();
                if self.config.input.mouse != was_mouse {
                    self.set_mouse_capture(self.config.input.mouse);
                }
                // Re-emit the caret for a possibly-changed [cursor].
                self.cursor_shape = None;
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
            if self.quit || absorbed >= BATCH_MAX || Instant::now() >= deadline {
                break;
            }
        }
        Ok(absorbed)
    }

    /// Apply one terminal event to the editor's state, without drawing.
    fn absorb(&mut self, event: CtEvent) {
        match event {
            CtEvent::Key(key) if key.kind == KeyEventKind::Press => {
                self.on_key(key);
                self.sync_after_input();
                self.needs_redraw = true;
            }
            CtEvent::Mouse(m) => {
                self.on_mouse(m);
                self.sync_after_input();
                self.needs_redraw = true;
            }
            CtEvent::Resize(w, h) => {
                self.term_size = (w, h);
                self.needs_redraw = true;
            }
            _ => {}
        }
    }

    /// Enable or disable terminal mouse capture at runtime.
    fn set_mouse_capture(&self, on: bool) {
        use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        let _ = if on {
            execute!(std::io::stdout(), EnableMouseCapture)
        } else {
            execute!(std::io::stdout(), DisableMouseCapture)
        };
    }

    /// Left-click positions the cursor; the wheel scrolls by moving it.
    fn on_mouse(&mut self, m: MouseEvent) {
        if !self.config.input.mouse {
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let tw = if self.tree.is_some() {
                    crate::tree::WIDTH.min(self.term_size.0 / 2)
                } else {
                    0
                };
                if self.tree.is_some() && m.column < tw {
                    self.tree_click(m.row);
                    return;
                }
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: self.term_size.0,
                    height: self.term_size.1,
                };
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
                }
            }
            MouseEventKind::ScrollDown => self.move_by(Motion::Down, 3),
            MouseEventKind::ScrollUp => self.move_by(Motion::Up, 3),
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
        let last = self.buffer.line_count().saturating_sub(1);
        if self.buffer.cursor.line > last {
            self.buffer.cursor.line = last;
        }
        let len = self.buffer.line_len(self.buffer.cursor.line);
        let past_end = self.mode.allows_past_end();
        self.buffer.cursor.clamp(len, past_end);
        self.active = ActiveSet::compute(&self.mode, &self.buffer.cursor, self.anchor);
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
        if let Some(res) = motion::resolve(&self.buffer, m, count, page) {
            self.buffer.cursor = res.target;
        }
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
        // The register the `"x` prefix named; `store_register`/`take_register`
        // consume it, and anything that does not use it must not keep it.
        self.pending_register = cmd.register;
        let count = cmd.count.max(1);

        match cmd.action {
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
            Action::DeleteChar => self.delete_chars_under(count),
            Action::DeleteCharBack => self.delete_chars_before(count),
            Action::DeleteToEol => self.change_or_delete_to_eol(Operator::Delete),
            Action::ChangeToEol => self.change_or_delete_to_eol(Operator::Change),
            Action::YankLine => {
                let line = self.buffer.cursor.line;
                let last = self.buffer.line_count().saturating_sub(1);
                self.operate_linewise(Operator::Yank, line, (line + count - 1).min(last));
            }
            // `3rx` replaces three characters, so this needs the count and
            // belongs here rather than in `apply_action`.
            Action::ReplaceChar(c) => self.replace_chars(c, count),
            Action::JoinLines => self.join_lines(count),
            Action::ToggleCase => self.toggle_case(count),
            Action::SearchNext { reverse } => {
                let backward = match self.search.as_ref() {
                    Some(s) => s.reverse != reverse,
                    None => reverse,
                };
                self.search_move(backward);
            }
            Action::RepeatFind { reverse } => self.repeat_find(reverse, count),
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
            // Toggle symmetry (docs/history/IDEAS.md): the binding that opens a split closes
            // it again. `<C-w>v` is the unconditional form, as in vim.
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
            Target::Motion(m) => {
                let m = self.resolve_screen_motion(m);
                self.apply_operator(op, m, count);
            }
            Target::Object { key, around } => self.operate_object(op, key, around),
            Target::Line => {
                let l1 = self.buffer.cursor.line;
                let last = self.buffer.line_count().saturating_sub(1);
                self.operate_linewise(op, l1, (l1 + count - 1).min(last));
            }
            Target::Selection => self.visual_operate(op),
        }
    }

    fn apply_action(&mut self, action: Action) {
        match action {
            Action::Save | Action::SaveStayInsert => self.write(None, false),
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
            Action::PasteAfter => self.paste(true),
            Action::PasteBefore => self.paste(false),
            Action::DeleteChar => self.delete_chars_under(1),
            Action::ToggleBold => self.writer_toggle("**"),
            Action::ToggleItalic => self.writer_toggle("*"),
            Action::ToggleHighlight => self.writer_toggle("=="),
            Action::ToggleCode => self.writer_toggle("`"),
            Action::ToggleTask => self.toggle_task(),
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
            Action::FileTree { root } => {
                let root = self.root_dir(root);
                self.toggle_tree(root);
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
            | Action::RepeatFind { .. }
            | Action::SearchWordUnderCursor
            | Action::AppendParagraph
            | Action::Window(_)
            | Action::ToggleSplit { .. }
            | Action::ClosePane
            | Action::OnlyPane
            | Action::ReplaceChar(_)
            | Action::JoinLines
            | Action::ToggleCase => {}
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // The help overlay owns all input while it is open.
        if self.help.is_some() {
            self.on_key_help(key);
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
            self.recording.push(key);
        }

        let mode = self.mode.clone();

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

        // Seal the step unless we are still inserting.
        if !matches!(self.mode, Mode::Insert) {
            self.buffer.history.end_group();
        }

        // Command finished (back to a clean boundary): if it edited, remember it.
        if !self.replaying && self.is_clean() && self.buffer.revision != self.dot_rev {
            self.dot = std::mem::take(&mut self.recording);
        }
    }

    // ------------------------------------------------------------------ help

    fn open_help(&mut self, topic: &str) {
        self.help = Some(Help::open(topic));
    }

    /// Scroll or close the help overlay.
    fn on_key_help(&mut self, key: KeyEvent) {
        let Some(help) = &mut self.help else { return };
        let max = help.lines.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.help = None,
            KeyCode::Char('j') | KeyCode::Down => help.scroll = (help.scroll + 1).min(max),
            KeyCode::Char('k') | KeyCode::Up => help.scroll = help.scroll.saturating_sub(1),
            KeyCode::Char('d') | KeyCode::PageDown => help.scroll = (help.scroll + 10).min(max),
            KeyCode::Char('u') | KeyCode::PageUp => help.scroll = help.scroll.saturating_sub(10),
            KeyCode::Char('g') | KeyCode::Home => help.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => help.scroll = max,
            _ => {}
        }
    }

    // ------------------------------------------------------------- file tree

    fn cwd(&self) -> PathBuf {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    /// A buffer path as the panels see it: absolute, so it can be matched
    /// against a root that always is.
    fn absolute(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd().join(p)
        }
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

    fn root_dir(&self, root: Root) -> PathBuf {
        match root {
            Root::File => self.file_dir(),
            Root::Home => self.home(),
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
                if let Some(t) = self.tree.as_mut() {
                    t.focused = false; // hand focus back to the editor
                }
            }
        }
    }

    /// Replace the current buffer with `path`. Refuses if there are unsaved
    /// changes. Returns whether it opened.
    /// A left-click inside the tree pane: focus it and select the clicked row.
    fn tree_click(&mut self, row: u16) {
        let h = self.term_size.1 as usize;
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
        if let Some(i) = self.docs.iter().position(|d| d.buffer.path.as_deref() == Some(&*path)) {
            self.switch_to(i);
            return true;
        }
        match Buffer::open(path.clone(), &self.config.markdown.plain_text_extensions) {
            Ok(buffer) => {
                self.docs.push(BufferState::new(buffer));
                self.switch_to(self.docs.len() - 1);
                self.notify(format!("opened {}", path.display()), FlashKind::Info);
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
            Some(Dest::Path { path, section }) => self.open_or_create(path, &section),
            Some(Dest::Note(l)) => {
                let (root, from) = (self.link_root(), self.link_from());
                match link::resolve(&l, &from, &root) {
                    Ok(path) => self.open_or_create(path, &l.section),
                    // A name nothing answers to is a note not written YET.
                    // Resolution is unchanged; what is new is that its miss is
                    // a starting point rather than an error.
                    Err(link::Unresolved::Missing(_)) => {
                        let path = self.file_dir().join(&l.target);
                        self.open_or_create(path, &l.section)
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
    fn open_or_create(&mut self, path: PathBuf, section: &link::Section) {
        let path = if path.exists() { path } else { with_default_ext(path) };
        if !path.exists() {
            if !creatable(&path) {
                self.notify(format!("no {}", path.display()), FlashKind::Error);
                return;
            }
            if let Err(e) = ops::create(&path, false) {
                self.notify(format!("{e}"), FlashKind::Error);
                return;
            }
            if self.open_file(path.clone()) {
                self.notify(format!("created {}", path.display()), FlashKind::Info);
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
                self.notify(format!("no {}", path.display()), FlashKind::Error);
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
        self.focused_pane().map(|p| p.scroll).unwrap_or(0)
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
        let scroll = self.focused_pane().map(|p| p.scroll).unwrap_or(0);
        let cursor = self.buffer.cursor;
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        if self.layout.split(self.focus_pane, vertical, Pane { id, doc, scroll, weight: crate::render::pane::EVEN, cursor }) {
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
        self.anchor = None;
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
            pane.scroll = 0;
            // A pane arriving at a document picks up where that document was
            // left, not where this pane was in the last one.
            pane.cursor = cursor;
        }
        self.mode = Mode::Normal;
        self.anchor = None;
        self.pending.reset();
        // The block cache of a document edited while it was in the background
        // (there is no such path today, but reload could add one) resyncs on
        // the next frame through the usual revision check.
        self.active = ActiveSet::compute(&self.mode, &self.buffer.cursor, None);
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
    /// Not a toggle, unlike the tree (docs/history/IDEAS.md, "toggle
    /// symmetry"): an open
    /// finder is a text field that swallows every key, so its own binding would
    /// arrive as query text. Esc closes it — the same deal as the `:` box.
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
                KeyCode::Char('u') if ctrl => finder.clear_query(),
                KeyCode::Char('n') | KeyCode::Char('j') if ctrl => finder.down(),
                KeyCode::Char('p') | KeyCode::Char('k') if ctrl => finder.up(),
                KeyCode::Down | KeyCode::Tab => finder.down(),
                KeyCode::Up | KeyCode::BackTab => finder.up(),
                KeyCode::Backspace => finder.pop_char(),
                KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                    finder.push_char(c)
                }
                _ => {}
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
        if self.dot.is_empty() {
            return;
        }
        self.replaying = true;
        for key in self.dot.clone() {
            self.on_key(key);
        }
        self.replaying = false;
    }

    // ----------------------------------------------------------------- normal

    /// Normal and Visual mode are one line of code each now: hand the key to
    /// the grammar (SPEC.md §7.2) and run whatever command comes back.
    fn on_key_normal(&mut self, key: KeyEvent) {
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

    /// Turn zen mode on or off, remembering what it hid so leaving it restores
    /// the user's own settings rather than a hardcoded default.
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
    fn operate_object(&mut self, op: Operator, obj: char, around: bool) {
        let Some((first, last, linewise)) = self.object_bounds(obj, around) else {
            return;
        };
        if linewise {
            self.operate_linewise(op, first.line, last.line);
        } else {
            self.operate_charwise(op, first, last, true);
        }
    }

    /// The cursors bounding a text object, and whether it is linewise.
    fn object_bounds(&self, obj: char, around: bool) -> Option<(Cursor, Cursor, bool)> {
        let range = object::resolve(&self.buffer, obj, around)?;
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
        // vim's one irregular verb: on a non-blank, `cw` changes to the END of
        // the word rather than up to the next one, so it does not swallow the
        // space it is about to type over.
        let m = match (op, m) {
            (Operator::Change, Motion::WordForward { big }) if !self.cursor_on_blank() => {
                Motion::WordEnd { big }
            }
            _ => m,
        };
        let from = self.buffer.cursor;
        let page = self.viewport_height();
        let res = match motion::resolve(&self.buffer, m, count, page) {
            Some(r) => r,
            None => return,
        };
        match res.kind {
            motion::MotionKind::Linewise => self.operate_linewise(op, from.line, res.target.line),
            motion::MotionKind::Inclusive => self.operate_charwise(op, from, res.target, true),
            motion::MotionKind::Exclusive => self.operate_charwise(op, from, res.target, false),
        }
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
            self.mode = Mode::Normal;
            self.anchor = None;
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
        self.toggle_inline(marker, start, end);
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
            // Wrap: insert at the end first, then the start.
            let (at_end, at_start) = (self.idx_to_cursor(end), self.idx_to_cursor(start));
            self.buffer.insert_str(at_end, marker);
            self.buffer.insert_str(at_start, marker);
            self.buffer.cursor = self.idx_to_cursor(start + mlen);
        }
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

        // Flip an existing `[ ]` / `[x]`.
        for i in 0..chars.len().saturating_sub(2) {
            if chars[i] == '[' && chars[i + 2] == ']' {
                let inner = chars[i + 1];
                if matches!(inner, ' ' | 'x' | 'X') {
                    let new = if inner == ' ' { "x" } else { " " };
                    self.buffer.delete_chars(base + i + 1, base + i + 2);
                    self.buffer.insert_str(Cursor::new(line, i + 1), new);
                    return;
                }
            }
        }

        // Otherwise, if it is a list item, insert a checkbox after the marker.
        if let Some(BlockKind::ListItem { marker, .. }) = self.blocks.kinds.get(line) {
            let indent = chars.iter().take_while(|c| **c == ' ' || **c == '\t').count();
            let mut p = indent;
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
            self.buffer.insert_str(Cursor::new(line, p.min(chars.len())), "[ ] ");
        }
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
        let end_col = (c.col + count).min(len);
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
            if line + 1 >= self.buffer.line_count() {
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
    fn repeat_find(&mut self, reverse: bool, count: usize) {
        if let Some((target, forward, till)) = self.last_find {
            let forward = forward ^ reverse;
            self.move_by(Motion::FindChar { target, forward, till }, count);
        }
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
            self.mode = Mode::Normal;
            self.anchor = None;
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
        let last = self.buffer.line_count().saturating_sub(1);
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
                self.buffer.cursor = Cursor::new(a, 0);
            }
            Operator::Delete => {
                self.buffer.delete_chars(start, end);
                self.store_register(text, true, true);
                let nl = self.buffer.line_count().saturating_sub(1);
                self.buffer.cursor = Cursor::new(a.min(nl), 0);
            }
            Operator::Change => {
                self.buffer.delete_chars(start, end);
                self.store_register(text, true, true);
                // Leave one empty line to type into.
                let at = Cursor::new(a.min(self.buffer.line_count().saturating_sub(1)), 0);
                self.buffer.insert_str(at, "\n");
                self.buffer.cursor = Cursor::new(a.min(self.buffer.line_count().saturating_sub(1)), 0);
                self.enter_insert();
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

    /// `>`/`<` over whole lines. One indent unit is `tab_width` spaces, or a
    /// literal tab when `expand_tab` is off. Blank lines are left alone —
    /// indenting whitespace-only lines just leaves trailing whitespace behind.
    fn shift_lines(&mut self, l1: usize, l2: usize, outdent: bool) {
        let width = self.config.editor.tab_width.max(1);
        let unit: String = if self.config.editor.expand_tab {
            " ".repeat(width)
        } else {
            "\t".to_string()
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
            // Outdent removes one tab, or up to `width` leading spaces.
            let mut drop = 0usize;
            for (i, c) in text.chars().enumerate() {
                if i >= width {
                    break;
                }
                match c {
                    '\t' => {
                        drop = i + 1;
                        break;
                    }
                    ' ' => drop = i + 1,
                    _ => break,
                }
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
    fn store_register(&mut self, text: String, linewise: bool, deleted: bool) {
        let reg = Register { text, linewise };
        if let Some(name) = self.pending_register.take() {
            let key = name.to_ascii_lowercase();
            let merged = if name.is_ascii_uppercase() {
                match self.registers.get(&key) {
                    Some(prev) => {
                        let mut text = prev.text.clone();
                        if prev.linewise && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(&reg.text);
                        Register {
                            text,
                            linewise: prev.linewise || reg.linewise,
                        }
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
        self.registers.insert('"', reg);
    }

    /// The register a command should read: the one a `"x` prefix named (consumed
    /// here), else the unnamed one.
    fn take_register(&mut self) -> Option<Register> {
        let name = self
            .pending_register
            .take()
            .unwrap_or('"')
            .to_ascii_lowercase();
        self.registers.get(&name).cloned()
    }

    /// `p` / `P`: paste the register after / before the cursor.
    fn paste(&mut self, after: bool) {
        let reg = match self.take_register() {
            Some(r) => r,
            None => return,
        };
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
                self.buffer.cursor = Cursor::new(end_line + 1, 0);
            } else {
                self.buffer.insert_str(Cursor::new(line, 0), &reg.text);
                self.buffer.cursor = Cursor::new(line, 0);
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
                let n = reg.text.chars().count();
                self.buffer.cursor = Cursor::new(cur.line, col + n.saturating_sub(1));
            }
        }
    }

    // ----------------------------------------------------------------- visual

    fn enter_visual(&mut self, mode: Mode) {
        self.anchor = Some(self.buffer.cursor);
        self.mode = mode;
        self.touch_status();
    }

    fn leave_visual(&mut self) {
        self.mode = Mode::Normal;
        self.anchor = None;
        self.pending.reset();
    }

    fn visual_operate(&mut self, op: Operator) {
        let anchor = self.anchor.unwrap_or(self.buffer.cursor);
        let cursor = self.buffer.cursor;
        let linewise = matches!(self.mode, Mode::VisualLine);
        self.mode = Mode::Normal;
        self.anchor = None;
        if linewise {
            self.operate_linewise(op, anchor.line, cursor.line);
        } else {
            // Visual selections are inclusive of the cursor character.
            self.operate_charwise(op, anchor, cursor, true);
        }
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
        if alias.is_empty() {
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
        if self.buffer.cursor.col > 0 {
            let col = self.buffer.cursor.col - 1;
            self.buffer.cursor.set_col(col);
        }
        self.touch_status();
    }

    fn on_key_insert(&mut self, key: KeyEvent) {
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
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = Mode::Normal;
                self.escape_run = 0;
                self.escape_since = None;
                self.touch_status();
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.write(None, false)
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
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.flash = None;
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.run_command(&buf);
            }
            KeyCode::Backspace => {
                if buf.pop().is_none() {
                    self.mode = Mode::Normal;
                } else {
                    self.mode = Mode::Command(buf);
                }
            }
            KeyCode::Char(c) => {
                buf.push(c);
                self.mode = Mode::Command(buf);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ tree prompts

    /// A panel's one-line question. Swallows every key, exactly as `:` does:
    /// an open prompt is a text field, so `a` would otherwise re-open it.
    fn on_key_prompt(&mut self, key: KeyEvent, mut p: Prompt) {
        if p.kind.is_confirm() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
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
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.flash = None;
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                self.finish_prompt(p);
            }
            // Backspace on an empty field cancels, matching the `:` box.
            KeyCode::Backspace => match p.input.pop() {
                Some(_) => self.mode = Mode::Prompt(p),
                None => self.mode = Mode::Normal,
            },
            // Clear the field, as the finder's query does. `r` and `m` open
            // PRE-FILLED, so replacing outright needs to be one key.
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                p.input.clear();
                self.mode = Mode::Prompt(p);
            }
            KeyCode::Char(c) => {
                p.input.push(c);
                self.mode = Mode::Prompt(p);
            }
            _ => {}
        }
    }

    /// Carry out an answered prompt, report what happened, and put the tree's
    /// cursor where the user will look for it.
    fn finish_prompt(&mut self, p: Prompt) {
        // Export is not a tree operation and must work with the tree closed,
        // so it is answered before the tree is required.
        if let PromptKind::Export { format } = p.kind {
            return self.finish_export(format, &p.target, p.input.trim());
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
                let path = p.target.join(raw.trim_end_matches('/'));
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
                let mut dest = self
                    .tree
                    .as_ref()
                    .map(|t| t.resolve(raw))
                    .unwrap_or_else(|| root.join(raw));
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

    /// `:export [md|txt|html|pdf]` — open the save dialog for the finished doc.
    ///
    /// The document is flattened from what is ON DISK, so an unsaved buffer is
    /// refused rather than exported from a stale file. Silently exporting
    /// yesterday's version is the one outcome nobody could detect.
    fn open_export(&mut self, arg: &str) {
        let arg = arg.trim();
        let format = if arg.is_empty() {
            Format::Markdown
        } else {
            match Format::parse(arg) {
                Some(f) => f,
                None => {
                    return self
                        .notify(format!("export: {arg:?} is not md, txt, html or pdf"), FlashKind::Error)
                }
            }
        };
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
        let shown = suggestion
            .strip_prefix(self.cwd())
            .unwrap_or(&suggestion)
            .to_string_lossy()
            .into_owned();
        self.mode = Mode::Prompt(Prompt {
            kind: PromptKind::Export { format },
            target: src,
            input: shown,
        });
    }

    /// Carry out an answered export prompt.
    fn finish_export(&mut self, format: Format, source: &Path, dest: &str) {
        if dest.is_empty() {
            return;
        }
        let dest = {
            let p = Path::new(dest);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.cwd().join(p)
            }
        };
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
        ops::rename(from, to)?;
        for doc in &mut self.docs {
            let Some(path) = doc.buffer.path.clone() else { continue };
            // A moved DIRECTORY takes every buffer beneath it, not just an
            // exact match.
            if let Ok(rest) = path.strip_prefix(from) {
                doc.buffer.path = Some(if rest.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(rest)
                });
            }
        }
        Ok(Some(to.to_path_buf()))
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
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                if !query.is_empty() {
                    self.search = Some(Search { pattern: query, reverse });
                    self.search_move(reverse);
                }
            }
            KeyCode::Backspace => {
                if query.pop().is_none() {
                    self.mode = Mode::Normal;
                } else {
                    self.mode = Mode::Search { query, reverse };
                }
            }
            KeyCode::Char(c) => {
                query.push(c);
                self.mode = Mode::Search { query, reverse };
            }
            _ => {}
        }
    }

    /// Move the cursor to the next match of the active search, wrapping around
    /// the buffer. `backward` searches toward the start.
    fn search_move(&mut self, backward: bool) {
        let pattern = match &self.search {
            Some(s) if !s.pattern.is_empty() => s.pattern.clone(),
            _ => return,
        };
        let needle: Vec<char> = pattern.chars().collect();
        let hay: Vec<char> = self.buffer.rope.chars().collect();
        let cur = self.buffer.char_index(self.buffer.cursor);
        let found = if backward {
            // Strictly before the cursor, else wrap to the end.
            cur.checked_sub(1)
                .and_then(|before| char_rfind(&hay, &needle, before))
                .or_else(|| char_rfind(&hay, &needle, hay.len()))
        } else {
            char_find(&hay, &needle, cur + 1).or_else(|| char_find(&hay, &needle, 0))
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

    fn run_command(&mut self, cmd: &str) {
        let cmd = cmd.trim();
        let (name, arg) = match cmd.split_once(char::is_whitespace) {
            Some((n, a)) => (n, a.trim()),
            None => (cmd, ""),
        };

        match name {
            "w" | "write" => self.write(if arg.is_empty() { None } else { Some(arg) }, false),
            // `!` is "I know it changed, write anyway" — the only way past the
            // external-modification guard.
            "w!" | "write!" => self.write(if arg.is_empty() { None } else { Some(arg) }, true),
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
                    let path = PathBuf::from(arg);
                    self.open_file(path);
                }
            }
            "q!" => self.try_quit(true),
            "wq" | "x" => {
                self.write(if arg.is_empty() { None } else { Some(arg) }, false);
                self.try_quit(false);
            }
            "wq!" | "x!" => {
                self.write(if arg.is_empty() { None } else { Some(arg) }, true);
                self.try_quit(true);
            }
            "reload" | "e!" => self.reload_config(),
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
            "export" => self.open_export(arg),
            // §14.3. Also re-reads every target, so it doubles as the way to
            // refresh an expansion after editing the file it came from.
            "embed" => self.set_embed_mode(arg),
            "set" => self.set_option(arg),
            "zen" => self.apply_action(Action::ToggleZen),
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
            "code_syntax" | "syntax" => {
                self.config.markdown.code_syntax = resolve(val, self.config.markdown.code_syntax);
            }
            "typewriter" | "tw" => {
                self.config.layout.typewriter = resolve(val, self.config.layout.typewriter);
            }
            "conceal" => {
                self.config.layout.conceal = resolve(val, self.config.layout.conceal);
            }
            "embed" => return self.set_embed_mode(val),
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

    fn write(&mut self, path: Option<&str>, force: bool) {
        let policy = crate::fs::save::SavePolicy::from_config(&self.config.editor);
        let result = match path {
            Some(p) => self.buffer.save_as(PathBuf::from(p), policy, force),
            None => self.buffer.save(policy, force),
        };
        match result {
            Ok(()) => {
                let name = self.buffer.display_name();
                let words = self.buffer.word_count();
                self.notify(format!("{name} written · {words} words"), FlashKind::Info);
            }
            Err(e) => self.notify(format!("{e}"), FlashKind::Error),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::save::SavePolicy;
    use crate::render::markdown::block::BlockCache;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn app_with(text: &str) -> App {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.buffer.insert_str(Cursor::new(0, 0), text);
        app.buffer.cursor = Cursor::new(0, 0);
        app.blocks = BlockCache::build(&app.buffer);
        app
    }

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
    /// `bracket_link` rejects it and the scan falls through to `wiki_at`. A
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
        assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer).kinds);

        // Delete the fence opener — every following line's carry shifts.
        feed(&mut app, "jjdd");
        app.refresh_blocks();
        assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer).kinds);
        assert_eq!(app.blocks.revision, app.buffer.revision);

        // Undo puts it back; undo bypasses `touch`, so it must mark its own
        // dirty line or the cache silently keeps the post-delete carries.
        app.buffer.undo();
        app.refresh_blocks();
        assert_eq!(app.blocks.kinds, BlockCache::build(&app.buffer).kinds);
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
    /// split at each word boundary. SPEC.md §4.
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

    /// SPEC §10: a write refuses to clobber a file something else changed
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

        std::fs::write(&second, "occupied\n").unwrap();
        app.buffer
            .save_as(second.clone(), SavePolicy::default(), false)
            .expect("a different target is not what disk_mtime describes");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "body\n");
        let _ = std::fs::remove_file(first);
        let _ = std::fs::remove_file(second);
    }

    /// SPEC §7.2: `jk` in Insert removes both characters and leaves the mode.
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
        let mut app = App::new(cfg, None, None).unwrap();
        app.buffer.insert_str(Cursor::new(0, 0), text);
        app.buffer.cursor = Cursor::new(0, 0);
        app.blocks = BlockCache::build(&app.buffer);
        app
    }

    #[test]
    fn leader_sequence_binding_fires() {
        // Default leader is space.
        let mut app = app_with_keys("word\n", &[("<leader>b", "toggle_bold")]);
        feed(&mut app, " b"); // <leader>b
        assert_eq!(text(&app), "**word**\n");
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
        assert_eq!(app.help.as_ref().unwrap().title, "bindings");
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
        assert_eq!(app.buffer.cursor, Cursor::new(1, 0));
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
}
