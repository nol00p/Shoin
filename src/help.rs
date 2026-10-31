//! In-app help, shown by `:help [topic]` in an overlay with a tab per category.
//!
//! The pages are written below in a small text form rather than as finished
//! lines, so the renderer can style them (keys, descriptions, notes each in
//! their own colour) and the lookup behind `:help <word>` can match a real key
//! instead of guessing from spacing:
//!
//! ```text
//! ## Section title — a note shown dimmed beside it
//! keys :: what they do, wrapped to the page however long it is
//! > a paragraph of prose (any line without ` :: `)
//! ```
//!
//! The Configuration tab is not written here at all: it is read from the
//! shipped `shoin/*.conf` files, whose comments already explain every option,
//! so an option documented there is in help the moment it exists.

use crate::config::init::SHIPPED;

/// How wide the help text is laid out. The overlay is wider on most screens;
/// a narrower one cuts the right edge rather than reflowing.
const WIDTH: usize = 76;
/// The widest key column; keys longer than this put their text on the line
/// below.
const KEY_MAX: usize = 22;

/// One line of a help page, as the renderer styles it.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// A section heading, and an optional note beside it.
    Title(String, String),
    /// Keys and their description, padded to the section's key column. A
    /// wrapped description continues on rows with empty `keys`.
    Entry { keys: String, desc: String, width: usize },
    /// A line of prose.
    Text(String),
    Blank,
}

impl Row {
    /// The row as plain text, the way search and the docs tests read it.
    pub fn plain(&self) -> String {
        match self {
            Row::Title(t, n) if n.is_empty() => t.clone(),
            Row::Title(t, n) => format!("{t}  {n}"),
            Row::Entry { keys, desc, width } => format!("  {keys:<width$}  {desc}").trim_end().to_string(),
            Row::Text(t) => format!("  {t}"),
            Row::Blank => String::new(),
        }
    }
}

pub struct Category {
    pub name: &'static str,
    pub rows: Vec<Row>,
}

/// The help overlay's state: every category, which one is showing, and the
/// search going on in it.
pub struct Help {
    pub cats: Vec<Category>,
    /// The category on screen.
    pub cat: usize,
    pub scroll: usize,
    /// The row `:help <word>` or a search landed on, drawn highlighted.
    pub mark: Option<usize>,
    /// What `n` / `N` look for: the last `/` search, or the `:help` word.
    pub query: Option<String>,
    /// A `/` search being typed, shown in the bottom border until Enter.
    pub typing: Option<String>,
    /// "not found" and the like, in the bottom border until the next key.
    pub status: Option<String>,
}

/// The categories, in tab order: a name, the other words that open it, and
/// its page. Configuration comes last and is built from the shipped config.
const CATEGORIES: &[(&str, &[&str], &str)] = &[
    ("Basics", &["", "basics", "help", "overview", "shortcuts", "cheatsheet", "cheat", "reference"], BASICS),
    ("Navigation", &["navigation", "navigate", "motions", "motion", "bindings", "keys", "keymap"], NAVIGATION),
    ("Editing", &["editing", "edit", "edition", "operators"], EDITING),
    ("Writing", &["writing", "writer", "verbs", "markdown", "format"], WRITING),
    ("View", &["view", "rendering", "render", "display"], VIEW),
    ("Files", &["files", "file", "panels", "commands", "command", "ex"], FILES),
    ("Config", &["config", "configuration", "settings", "options", "theme"], ""),
];

impl Help {
    /// Open help. A category's name (or another word for it) opens that tab.
    /// Any other word is looked up — a key, a `:command`, a setting, a section
    /// — and its tab opens at it, highlighted. A word found nowhere opens the
    /// first tab and says so in the bottom border.
    pub fn open(topic: &str) -> Help {
        let mut help = Help {
            cats: categories(),
            cat: 0,
            scroll: 0,
            mark: None,
            query: None,
            typing: None,
            status: None,
        };
        let word = topic.trim();
        if let Some(i) = category_of(word) {
            help.cat = i;
        } else if let Some((cat, row)) = help.find(word) {
            help.cat = cat;
            help.mark = Some(row);
            help.scroll = help.context(row);
            help.query = Some(word.to_string());
        } else {
            help.status = Some(format!("no help for {word:?} — / to search"));
        }
        help
    }

    /// Whether `:help <topic>` finds something: a category or a word.
    #[cfg(test)]
    pub fn knows(topic: &str) -> bool {
        Help::open(topic).status.is_none()
    }

    /// Every page as one text, for the tests that hold help and docs together.
    #[cfg(test)]
    pub fn all_text() -> String {
        categories()
            .iter()
            .flat_map(|c| std::iter::once(c.name.to_string()).chain(c.rows.iter().map(Row::plain)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn rows(&self) -> &[Row] {
        &self.cats[self.cat].rows
    }

    /// Where to scroll to show `row`: from its section's heading when that is
    /// close above it, so the match is seen in context; else a few rows up.
    fn context(&self, row: usize) -> usize {
        let rows = self.rows();
        (row.saturating_sub(12)..=row)
            .rev()
            .find(|&r| matches!(rows[r], Row::Title(..)))
            .unwrap_or(row.saturating_sub(2))
    }

    /// The next category (`Tab`) or the previous one, wrapping round.
    pub fn switch(&mut self, forward: bool) {
        let n = self.cats.len();
        self.show((self.cat + if forward { 1 } else { n - 1 }) % n);
    }

    /// Show category `i` from its top.
    pub fn show(&mut self, i: usize) {
        if i < self.cats.len() {
            self.cat = i;
            self.scroll = 0;
            self.mark = None;
        }
    }

    /// The next row containing `query` (any case) after the marked one — or
    /// before it with `back` — through every category, wrapping round. It
    /// becomes the mark, on its tab, at the top of the view; `false` when no
    /// page contains it.
    pub fn search(&mut self, query: &str, back: bool) -> bool {
        let q = query.to_lowercase();
        if q.is_empty() {
            return false;
        }
        // Every row of every category as one ring, starting where we are.
        let all: Vec<(usize, usize)> = self
            .cats
            .iter()
            .enumerate()
            .flat_map(|(c, cat)| (0..cat.rows.len()).map(move |r| (c, r)))
            .collect();
        let n = all.len();
        let here = all
            .iter()
            .position(|&(c, r)| c == self.cat && r == self.mark.unwrap_or(self.scroll))
            .unwrap_or(0);
        // Without a mark the search starts AT the top row rather than after it.
        let start = if self.mark.is_none() && !back { here + n - 1 } else { here };
        let hit = (1..=n)
            .map(|i| if back { all[(start + n - i) % n] } else { all[(start + i) % n] })
            .find(|&(c, r)| self.cats[c].rows[r].plain().to_lowercase().contains(&q));
        match hit {
            Some((c, r)) => {
                self.cat = c;
                self.mark = Some(r);
                self.scroll = self.context(r);
                true
            }
            None => false,
        }
    }

    /// Where `word` is documented. Tried in turn, each through every category
    /// before the next: a row that LISTS it as a key, command or setting, a
    /// section heading, then any mention of it as a whole word. `view` also
    /// finds `:view`, and `<leader>fe` finds `Space fe`. Basics comes last:
    /// it repeats keys whose home is elsewhere.
    fn find(&self, word: &str) -> Option<(usize, usize)> {
        if word.is_empty() {
            return None;
        }
        let word = word.replace("<leader>", "Space ");
        let lower = word.to_lowercase();
        let colon = format!(":{lower}");
        let lists = |row: &Row| match row {
            Row::Entry { keys, .. } if !keys.is_empty() => {
                let keys = keys.to_lowercase();
                if lower.contains(' ') {
                    return keys.contains(&lower);
                }
                // A setting is listed as `name = value`; `section.name` too.
                let setting = keys.split('=').next().unwrap_or("").trim().trim_matches('"');
                let short = lower.rsplit('.').next().unwrap_or(&lower);
                setting == lower
                    || setting == short && lower.contains('.')
                    || keys.split_whitespace().any(|k| k == lower || k == colon)
            }
            _ => false,
        };
        let heading = |row: &Row| match row {
            Row::Title(t, _) => {
                let t = t.to_lowercase();
                t == lower || has_word(&t, &lower)
            }
            _ => false,
        };
        let mentions = |row: &Row| {
            let p = row.plain().to_lowercase();
            has_word(&p, &lower) || has_word(&p, &colon)
        };
        let n = self.cats.len();
        let order: Vec<usize> = (1..n).chain([0]).collect();
        let stages: [&dyn Fn(&Row) -> bool; 3] = [&lists, &heading, &mentions];
        stages.iter().find_map(|matches| {
            order.iter().find_map(|&c| {
                self.cats[c].rows.iter().position(matches).map(|r| (c, r))
            })
        })
    }
}

/// The category a word names, if it names one.
fn category_of(word: &str) -> Option<usize> {
    let w = word.to_lowercase();
    CATEGORIES.iter().position(|(_, words, _)| words.contains(&w.as_str()))
}

/// `word` in `line` with no letter, digit or `_` either side of it.
fn has_word(line: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let inner = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(word).any(|(i, _)| {
        !line[..i].chars().next_back().is_some_and(inner)
            && !line[i + word.len()..].chars().next().is_some_and(inner)
    })
}

fn categories() -> Vec<Category> {
    CATEGORIES
        .iter()
        .map(|(name, _, page)| Category {
            name,
            rows: if page.is_empty() { config_rows() } else { parse(page) },
        })
        .collect()
}

// ------------------------------------------------------------------ layout

/// Lay a page's text form out as rows.
fn parse(page: &str) -> Vec<Row> {
    let mut out = Vec::new();
    let mut section: Vec<(String, String)> = Vec::new();
    for line in page.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if let Some(title) = line.strip_prefix("## ") {
            flush(&mut out, &mut section);
            if !out.is_empty() {
                out.push(Row::Blank);
            }
            let (t, n) = title.split_once(" — ").unwrap_or((title, ""));
            out.push(Row::Title(t.to_string(), n.to_string()));
        } else if let Some((keys, desc)) = line.split_once(" :: ") {
            section.push((keys.trim().to_string(), desc.trim().to_string()));
        } else if let Some(keys) = line.strip_suffix(" ::") {
            section.push((keys.trim().to_string(), String::new()));
        } else if let Some(text) = line.strip_prefix("> ") {
            // Checked after the entries, so `> < :: indent` is a key.
            flush(&mut out, &mut section);
            out.extend(wrap(text, WIDTH - 2).into_iter().map(Row::Text));
        } else {
            section.push((line.to_string(), String::new()));
        }
    }
    flush(&mut out, &mut section);
    out
}

/// Lay out the entries gathered since the last heading or paragraph, with
/// one key column for all of them.
fn flush(out: &mut Vec<Row>, section: &mut Vec<(String, String)>) {
    let width = section
        .iter()
        .map(|(k, _)| k.chars().count())
        .filter(|&w| w <= KEY_MAX)
        .max()
        .unwrap_or(0);
    let room = WIDTH.saturating_sub(width + 4).max(20);
    for (keys, desc) in section.drain(..) {
        let mut lines = wrap(&desc, room).into_iter();
        if keys.chars().count() > width {
            // Too wide for the column: the keys get the row to themselves.
            out.push(Row::Entry { keys, desc: String::new(), width });
        } else {
            let first = lines.next().unwrap_or_default();
            out.push(Row::Entry { keys, desc: first, width });
        }
        for more in lines {
            out.push(Row::Entry { keys: String::new(), desc: more, width });
        }
    }
}

/// Break `text` into lines of at most `width` characters, at spaces.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

// ------------------------------------------------------------ configuration

/// The Configuration tab: where config lives, then every section of every
/// shipped `.conf` file, each option with its default and its comment.
fn config_rows() -> Vec<Row> {
    let mut page = String::from(CONFIG_INTRO);
    for name in CONFIG_ORDER {
        let body = SHIPPED.iter().find(|(n, _)| n == name).map_or("", |(_, b)| b);
        page.push_str(&conf_page(name, body));
    }
    // Anything shipped that the order above forgot still shows, at the end.
    for (name, body) in SHIPPED {
        if !CONFIG_ORDER.contains(name) {
            page.push_str(&conf_page(name, body));
        }
    }
    parse(&page)
}

/// The order the shipped files appear in: the page first, then editing,
/// input, panels, glyphs, colours and keys.
const CONFIG_ORDER: &[&str] =
    &["layout.conf", "editor.conf", "input.conf", "tree.conf", "glyphs.conf", "theme.conf", "keys.conf"];

/// One shipped file in the page's text form. Its header (the comment block
/// before the first `[section]`) is left out: it explains the file, not the
/// options. A comment block directly above an option describes it; one
/// followed by a blank line is a paragraph of its own.
fn conf_page(file: &str, body: &str) -> String {
    let mut out = String::new();
    let mut pending: Vec<String> = Vec::new();
    let mut started = false;
    let mut last_was_entry = false;
    let flush = |out: &mut String, pending: &mut Vec<String>| {
        if !pending.is_empty() {
            out.push_str(&format!("> {}\n", pending.join(" ")));
            pending.clear();
        }
    };
    for raw in body.lines() {
        let line = raw.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            flush(&mut out, &mut pending);
            out.push_str(&format!("## [{section}] — {file}\n"));
            started = true;
            last_was_entry = false;
            continue;
        }
        if !started {
            continue;
        }
        if line.is_empty() {
            flush(&mut out, &mut pending);
            last_was_entry = false;
        } else if let Some(comment) = line.strip_prefix('#') {
            // Backticks mark code in a .conf comment; help has its own colours.
            let comment = comment.trim().replace('`', "");
            let comment = comment.as_str();
            // An indented comment under an option continues its description.
            if last_was_entry && raw.starts_with(char::is_whitespace) {
                out.pop(); // the newline
                out.push(' ');
                out.push_str(comment);
                out.push('\n');
            } else if looks_like_setting(comment) {
                // `# name = "gruvbox"`: shown commented out in the shipped file.
                flush(&mut out, &mut pending);
                // Whether it is in effect anyway (keys.conf lists built-ins
                // this way) is for the comment above it to say.
                let (setting, note) = split_comment(comment);
                let setting = setting
                    .split_once('=')
                    .map_or(setting.to_string(), |(k, v)| format!("{} = {}", k.trim(), v.trim()));
                let note = if note.is_empty() { String::new() } else { format!(" — {note}") };
                out.push_str(&format!("{setting} :: (commented out){note}\n"));
                last_was_entry = true;
            } else {
                last_was_entry = false;
                pending.push(comment.to_string());
            }
        } else {
            let (setting, comment) = split_comment(line);
            let comment = comment.replace('`', "");
            let setting = setting.split_once('=').map_or(setting.to_string(), |(k, v)| {
                format!("{} = {}", k.trim(), v.trim())
            });
            let mut desc = std::mem::take(&mut pending).join(" ");
            if !comment.is_empty() {
                if !desc.is_empty() {
                    desc.push(' ');
                }
                desc.push_str(&comment);
            }
            out.push_str(&format!("{setting} :: {desc}\n"));
            last_was_entry = true;
        }
    }
    flush(&mut out, &mut pending);
    out
}

/// Whether a comment is a setting commented out: `name = "gruvbox"`,
/// `"<M-j>" = "list_item_down"` — one token, then `=`, then a value.
fn looks_like_setting(comment: &str) -> bool {
    match comment.split_once(" = ") {
        Some((key, value)) => {
            let key = key.trim();
            !key.is_empty() && !key.contains(' ') && !value.trim().is_empty()
        }
        None => false,
    }
}

/// A config line's setting and its trailing `# comment`. A `#` inside quotes
/// is a colour, not a comment.
fn split_comment(line: &str) -> (&str, &str) {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return (line[..i].trim(), line[i + 1..].trim()),
            _ => {}
        }
    }
    (line.trim(), "")
}

const CONFIG_INTRO: &str = "
## Where config lives — first found wins
--config <path> :: a file or a directory
$SHOIN_CONFIG :: the same, from the environment
./shoin.conf :: a single file in the current directory
./shoin/ :: a directory of *.conf in the current directory
$XDG_CONFIG_HOME/shoin/ :: then the usual home, ~/.config/shoin/
> A directory's *.conf files are merged, so config splits by category: layout.conf, theme.conf, keys.conf. Every option is optional and falls back to the default shown below. Saving a file reloads it live; a mistake keeps the running config and says what was wrong.
shoin --init-config :: write an annotated copy of every file below to start from
shoin --print-config :: print the config in effect
:e! :reload :: re-read it now
> Every action name a [keys.*] binding can use, with its other spellings and default key, is listed in docs/reference/keys.md under Action names.
";

// ------------------------------------------------------------------- pages

const BASICS: &str = "
## Shoin — a Markdown editor with live preview
> The line the cursor is on shows its raw Markdown; every other line shows the result, markers hidden. Editing is modal, with Vim's keys.

## Modes
Normal :: move around and run operators. Esc always comes back here
Insert :: type: i a I A o O to start, Esc or jk to stop
Visual :: select: v by character, V by line, or drag with the mouse. A drag while typing selects too; an operator or Esc goes back to typing
Command :: : then a command, Enter to run it

## First keys
i :: start typing
Esc :: stop typing
:w :: save — Ctrl-s and Space w too
:q :: close this buffer; the last one quits
u Ctrl-r :: undo · redo
. :: repeat the last change

## Leader keys — Space, or [input] leader
Space hl :: this help
Space w Space q :: save · quit
Space fe Space fE :: file tree in this file's folder · over $HOME
Space ff Space fF :: find a file in this file's folder · over $HOME
Space fb :: switch buffer
Space sv Space ss :: split beside · below — again to close it
Space so :: keep only this pane
Space F :: cycle focus mode
Space t :: typewriter scrolling
Space z :: zen: all chrome on · off
Space c :: concealment on · off
Space n :: line numbers
Space a :: cycle text alignment
Space v :: read-only on · off, as :view
Space b Space i :: bold · italic, on a Visual selection
> These are the shipped keys.conf, built in. Rebind any of them under [keys.normal] and [keys.visual], or set one to \"none\" to remove it — see Config.

## Starting
shoin :: a blank page with the ways in drawn on it — [splash] art draws your own, or none
shoin notes.md :: open a file; it is created on the first :w
shoin -R notes.md :: open it read-only — see View
shoin -p notes.md :: print it as the editor shows it, and exit. --pager pages a long one through $PAGER
cat notes.md | shoin :: piped text, in an unnamed buffer: :w name.md saves it, :q just quits. shoin - reads stdin too
cat notes.md | shoin -p :: print piped text

## This help
Tab Shift-Tab :: next · previous category; 1 to 7 jump to one
j k :: scroll; d u half a page, g G top · bottom
/ :: search every category; n N next · previous match
:help <word> :: open at a key, command, setting or section: :help gb, :help view, :help measure, :help tree
:help [topic] :: open a category: basics, navigation, editing, writing, view, files, config
q Esc :: close
";

const NAVIGATION: &str = "
## Motions — take a count: 3w, 5j
h j k l :: left · down · up · right; the arrows too
w b e :: next word · previous · end of word
W B E :: the same, with words split by spaces only
0 ^ $ :: line start · first non-blank · line end
{ } :: previous · next paragraph
gg G :: first · last line; with a count, that line
H M L :: top · middle · bottom of the screen

## Along the line
f{c} t{c} :: to the next c · to just before it
F{c} T{c} :: the same, backward
; , :: repeat · reverse the last f or t

## Scrolling
Ctrl-d Ctrl-u :: half a page down · up
Ctrl-f Ctrl-b :: a page down · up
Ctrl-e Ctrl-y :: scroll the page a line; the cursor stays put until it would leave the screen
mouse wheel :: scrolls the page, three lines a notch

## Search
/ ? :: search forward · backward
n N :: next · previous match
* :: search the word under the cursor

## Links
gf :: follow the link under the cursor: [[note]], ![[note]], or the path in [text](note.md). A #Heading or #^block lands on it. A note that doesn't exist yet is created empty, so a link can come before its note; a name several notes answer to opens the finder
gx :: hand it to the desktop instead — a URL, a picture, a PDF. Never creates anything
Ctrl-^ Ctrl-6 :: back to the previous buffer — the way back out of a gf
";

const EDITING: &str = "
## Insert mode
i a :: type before · after the cursor
I A :: at the line's start · end
o O :: on a new line below · above
Esc Ctrl-c jk :: back to Normal. jk is input.escape_alias; \"\" turns it off
Ctrl-s :: save and keep typing; in Normal it saves, as :w
Enter :: continues a list marker or quote; on an empty item it ends the list (editor.auto_indent)
Tab Shift-Tab :: on a list item's line, indent · outdent it (editor.list_tab)

## Operators — then a motion or object, or doubled for the whole line
d c y :: delete · change · yank
dd cc yy :: the whole line
> < :: indent · outdent: >> <<, >ip
gu gU :: lowercase · uppercase: guiw, gUU
2d3w :: counts multiply — this deletes six words

## Quick edits
x X :: delete the character under · before the cursor
D C Y :: delete · change · yank to the end of the line
r{c} :: replace the character under the cursor
~ J :: toggle case · join the next line on
p P :: paste after · before
u Ctrl-r :: undo · redo
:u :un :und :undo :: undo, the : spelling of u
:red :redo :: redo, the : spelling of Ctrl-r
. :: repeat the last change

## Text objects — after an operator, or in Visual: diw, ca(, vip
iw aw :: inner · around word; iW aW split on spaces only
is as :: sentence
ip ap :: paragraph, by lines
i\" a\" :: inside · with the quotes; ' and ` too
i( a( :: inside · with the brackets: ( [ { <, b for () and B for {}. Brackets may span lines
il al :: a list item without · with its sub-items
aL :: every item at that level, with sub-items

## Visual
v V :: select by character · by line
d c y x :: operate on the selection
o :: swap the ends
> < u U :: indent · outdent · lowercase · uppercase
i{obj} a{obj} :: select a text object: viw, vap
move_lines_down move_lines_up :: move the selected lines down · up and keep them selected; a count moves further. Not on a key: bind them under [keys.visual], as \"J\" = \"move_lines_down\"

## Lists
Alt-j Alt-k :: move the item past the next · previous one, with its sub-items; a count moves it further. 1. 2. 3. lists are renumbered
>al <al :: indent · outdent the item with its sub-items
>aL <aL :: every item at that level
Tab Shift-Tab :: the same as >al <al, in Insert on the item's line
> Refused when it would break the list: indenting a first item, outdenting a top-level one, a top-level list four columns in. In tmux a quick Esc j can arrive as Alt-j: set escape-time to 10 or less. On macOS the terminal must send Option as Meta (Esc+).

## Registers
\"a{cmd} :: use register a: \"ayy \"ap \"add
\"A{cmd} :: an uppercase name appends to it
\"0 :: the last yank — a delete never overwrites it
\"1 to \"9 :: the delete ring, newest first
\"+ \"* :: the system clipboard: \"+yy \"+p
> :set clipboard sends plain y d p there too. Over SSH a copy reaches your clipboard by OSC 52 (the terminal must support it; in tmux, set-clipboard on). Paste from elsewhere with the terminal's own paste, Ctrl+Shift+V or Cmd+V: \"+p there has only Shoin's copy.
";

const WRITING: &str = "
## Inline — on the word under the cursor, or the Visual selection
gb :: toggle **bold**
gi :: toggle *italic*
gh :: toggle ==highlight==
gk :: toggle `code`
gl :: wrap as a [link](url), the cursor in the URL
Space b Space i :: bold · italic, on a Visual selection

## Blocks
g1 to g6 :: set the line's heading level
g0 :: strip the heading
gt :: toggle a task checkbox, - [ ] and - [x]; on a plain list item, adds one
ga :: align the table under the cursor: pad every cell. editor.table_align = \"auto\" does it on leaving Insert
gp :: start the next paragraph, in Insert
> Each is one undo step.

## Transclusion — ![[note]] embeds another file; [[note]] only links
![[note]] :: the whole file
![[note#Heading]] :: one section
![[note#^blockid]] :: one block
:embed [mode] :: how much to expand in place: none, as written (the default) · short, the target's text in a box · rec, and whatever it embeds · full, rec with no box, read as one document. A bare :embed toggles off and back
> An embed on the cursor's line shows its raw source, like **bold** does. Embedded text belongs to another file, so it can't be edited here.

## Images — ![[photo.png]]: png, jpg, gif, webp, bmp
> An image embed reserves a box the shape of the picture, captioned with its name and size. Terminals that draw pixels (kitty, Ghostty, WezTerm, iTerm2) show the picture in it; elsewhere the box and caption stand in for it. :export html carries it inside the page. SHOIN_IMAGE_PROTOCOL=kitty|iterm|none overrides the detection.
";

const VIEW: &str = "
## Toggles
Space c :: concealment on · off — :set conceal
Space F :: cycle focus: off · paragraph · sentence
:focus [mode] :: pick one; bare cycles
Space t :: typewriter scrolling — :typewriter, :tw
Space z :: zen: hide all chrome — :zen
Space n :: line numbers on · off
:number [mode] :: relative · absolute; bare toggles off and back to your last mode
Space a :: cycle how lines sit in the column
:align [mode] :: left · center · right · justified; bare cycles. Where the column itself sits is layout.align
:typewriter :tw :: typewriter scrolling
:zen :: hide all chrome

## Settings, live
:set <key> [on|off] :: conceal · typewriter · mouse · focus · numbers · code_syntax
:set measure=72 :: the text width in columns; :set measure reads it
:set line_spacing=1 :: blank rows between lines, 0 to 4

## Read-only — shoin -R, :view or Space v
:view :: read-only on · off: every line concealed, no caret
Space v :: the same on a key (toggle_view); in read-only too, to switch back
j k :: scroll a line
Ctrl-d Ctrl-u :: half a page
gg G :: top · bottom
/ ? n N :: search
q :: quit
i :: switch to editing
> An edit is refused with a message. Yanking, panels, gf and :w still work.
";

const FILES: &str = "
## Saving and quitting
:w :write [path] :: save, or save to a path
:w! :: save over a file that changed on disk, or over one :w <path> won't touch
:wq :x :: save and quit; :wq! even if the file changed
:q :q! :: close this buffer — the last one quits · the same, discarding changes
:Q :qa :: quit every buffer at once; :Q! :qa! discard changes
:set autosave [on|off] :: write modified named buffers on a timer. Off by default, and it keeps the guard of :w
:set autosave_interval=3 :: minutes between autosaves, 1 to 5

## When the file changes on disk
:diff :: merge against the file on disk: n p move between differences, Tab cycles this one (live · file · both), l f b set them all, w commits, q aborts. Nothing is written until w, and then it is one undo
:revert :revert! :: re-read the file from disk; ! discards unsaved changes
:set autoreload [on|off] :: take outside changes back automatically. On by default, and only ever on a buffer with nothing unsaved

## Export
:export [md|txt|html|pdf] [justify] :: write the finished document out, through a dialog with a name you can change. html is one self-contained page; pdf needs pandoc and a layout engine (typst is the smallest). justify stretches html and txt to fill the measure

## File tree — beside the page
Space fe Space fE :: open it in this file's folder · over $HOME; again to close. tree.root = \"cwd\" opens Space fe and Space ff where Shoin was launched instead; file_tree_cwd and find_file_cwd always do
j k :: move
l Enter o :: open a file · expand a folder
h :: collapse
g G R :: top · bottom · refresh
- = :: move the root up a level · into the selected folder
H :: show · hide dotfiles; remembered, tree.show_hidden to start with
a :: new file in this folder; end the name with / for a folder
r m d :: rename · move · delete. Delete asks first, and says how much a folder holds
Ctrl-w h l :: between the tree and the editor
q :: close the tree
> A prompt takes Ctrl-u to clear it and Esc to abandon it. Renaming or moving a file you have open carries the buffer with it.

## Fuzzy finder
Space ff Space fF :: find a file in this file's folder · over $HOME
type :: narrow the list
Ctrl-n Ctrl-p :: next · previous match; the arrows too
Ctrl-u :: clear the query
Enter Esc :: open · close

## Buffers — several files open at once
:e <path> :: open a file in a new buffer
Space fb :: switch buffer, in the finder
:ls :buffers :: list open buffers; ▸ marks this one
:b <name> :b <n> :: switch by name fragment or number
:bn :bp :: next · previous buffer, round the list
Ctrl-^ Ctrl-6 :: back to the previous buffer; twice lands where you started
:bd :bd! :: close this buffer; ! discards changes
:set clipboard [on|off] :: plain y d p use the system clipboard too, as Vim's unnamedplus. \"+ and \"* reach it either way. :set cb

## Panes
Space sv Space ss :: split beside · below — again to close it
Space so :: keep only this pane
:sp :vs :: split horizontally · vertically
:close :: close the focused pane
:only :: close every pane but this one
Ctrl-w v s :: split beside · below
Ctrl-w h j k l :: move between panes; the arrows too
Ctrl-w q o w :: close · keep only this · cycle
Ctrl-w > < + - :: wider · narrower · taller · shorter; takes a count
Ctrl-w = :: equal sizes again
";

#[cfg(test)]
mod tests {
    use super::*;

    /// The words a category answers to, old topic names included.
    #[test]
    fn topics_open_their_category() {
        for (topic, name) in [
            ("", "Basics"),
            ("shortcuts", "Basics"),
            ("bindings", "Navigation"),
            ("editing", "Editing"),
            ("writer", "Writing"),
            ("view", "View"),
            ("commands", "Files"),
            ("config", "Config"),
        ] {
            let h = Help::open(topic);
            assert_eq!(h.cats[h.cat].name, name, ":help {topic}");
            assert!(h.status.is_none());
        }
    }

    /// The row a word opens at, as plain text, and its category.
    fn landing(word: &str) -> (&'static str, String) {
        let h = Help::open(word);
        let line = h.mark.map(|i| h.rows()[i].plain()).unwrap_or_default();
        (h.cats[h.cat].name, line)
    }

    /// `:help <word>` opens at the row that documents it.
    #[test]
    fn a_word_opens_where_it_is_documented() {
        for (word, cat, expect) in [
            ("tree", "Files", "File tree"),
            ("finder", "Files", "Fuzzy finder"),
            ("registers", "Editing", "Registers"),
            ("gb", "Writing", "gb"),
            (":view", "View", ":view"),
            ("measure", "Config", "measure = 72"),
            ("layout.measure", "Config", "measure = 72"),
            ("n", "Navigation", "n N"),
            ("<leader>fe", "Files", "Space fe"),
            ("dd", "Editing", "dd cc yy"),
            ("background", "Config", "background"),
        ] {
            let (c, line) = landing(word);
            assert!(line.contains(expect), ":help {word} landed in {c} on {line:?}");
            assert_eq!(c, cat, ":help {word}: {line:?}");
        }
    }

    #[test]
    fn an_unknown_word_says_so() {
        let h = Help::open("zzqx");
        assert_eq!(h.cat, 0);
        assert!(h.status.as_deref().is_some_and(|s| s.contains("zzqx")));
        assert!(!Help::knows("zzqx"));
        assert!(Help::knows("gb"));
    }

    /// `/` and `n` / `N` go through every category, wrapping round.
    #[test]
    fn search_crosses_categories_and_wraps() {
        let mut h = Help::open("basics");
        assert!(h.search("register", false));
        let first = (h.cat, h.mark.unwrap());
        assert_eq!(h.cats[first.0].name, "Editing");
        assert!(h.search("register", false));
        assert!((h.cat, h.mark.unwrap()) > first, "n goes on");
        assert!(h.search("register", true));
        assert_eq!((h.cat, h.mark.unwrap()), first, "N comes back");
        assert!(!h.search("zzqx nowhere", false));
        assert_eq!((h.cat, h.mark.unwrap()), first, "a miss leaves the mark alone");
    }

    /// Every option of every shipped file is in the Configuration tab, with
    /// the `#` in a colour value read as part of the value.
    #[test]
    fn the_config_tab_lists_every_shipped_option() {
        let text = Help::all_text();
        for (name, body) in SHIPPED {
            let mut started = false;
            for line in body.lines().map(str::trim) {
                started |= line.starts_with('[');
                if !started || line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                    continue;
                }
                let key = line.split('=').next().unwrap().trim();
                assert!(text.contains(&format!("{key} = ")), "{name}: {key} is not in help");
            }
        }
        assert!(text.contains("background = \"#1a1b26\""));
    }

    /// No row outgrows the page: wrapping keeps descriptions and prose in it.
    #[test]
    fn rows_fit_the_page() {
        for cat in categories() {
            for row in &cat.rows {
                let p = row.plain();
                let long_keys = matches!(row, Row::Entry { keys, .. } if keys.chars().count() > KEY_MAX);
                assert!(long_keys || p.chars().count() <= WIDTH + 2, "{}: {p:?}", cat.name);
            }
        }
    }
}
