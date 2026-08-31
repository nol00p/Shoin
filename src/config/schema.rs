//! Serde types mirroring `shoin.conf`. SPEC.md §8.
//!
//! Every field has a default, so any subset of the file is valid.
//! `deny_unknown_fields` is deliberately NOT set — unknown keys warn instead,
//! for forward compatibility.

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub layout: LayoutConfig,
    pub status: StatusConfig,
    pub cursor: CursorConfig,
    pub editor: EditorConfig,
    pub input: InputConfig,
    pub markdown: MarkdownConfig,
    pub theme: ThemeConfig,
    pub glyphs: GlyphConfig,
    pub keys: KeysConfig,
    pub splash: SplashConfig,
    pub tree: TreeConfig,
    pub transclude: TranscludeConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    pub measure: u16,
    pub align: String,
    pub padding_top: u16,
    pub padding_bottom: u16,
    pub hanging_indent: bool,
    /// Blank rows drawn after each source line — the terminal's answer to line
    /// height. Rows that belong to no line; SPEC.md §6.
    pub line_spacing: u16,
    pub scroll_hint: bool,
    pub typewriter: bool,
    pub typewriter_anchor: f32,
    pub focus: String,
    /// Live preview. `false` renders every line raw with dimmed markers.
    pub conceal: bool,
    /// Reserve reveal width for the active line, trading horizontal slack for
    /// zero jitter while typing. SPEC.md §6.
    pub stable_gutter: bool,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            measure: 72,
            align: "center".into(),
            padding_top: 2,
            padding_bottom: 2,
            hanging_indent: true,
            line_spacing: 0,
            scroll_hint: true,
            typewriter: false,
            typewriter_anchor: 0.5,
            focus: "off".into(),
            conceal: true,
            stable_gutter: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct StatusConfig {
    pub enabled: bool,
    pub flash_ms: u64,
    pub show: Vec<String>,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            flash_ms: 1500,
            show: vec![
                "file".into(),
                "modified".into(),
                "position".into(),
                "words".into(),
            ],
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct CursorConfig {
    pub normal: String,
    pub insert: String,
    pub visual: String,
    pub blink: bool,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            normal: "block".into(),
            insert: "bar".into(),
            visual: "block".into(),
            blink: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct EditorConfig {
    pub tab_width: usize,
    pub expand_tab: bool,
    pub auto_indent: bool,
    pub auto_pair: bool,
    pub trim_on_save: bool,
    pub final_newline: String,
    /// Write every modified named buffer on a timer. OFF by default — see
    /// `fs::save`'s module doc for why that is not a shrug.
    pub autosave: bool,
    /// Minutes between autosaves, 1–5. Read only when `autosave` is on, and
    /// kept when it goes off so turning it back on remembers the number.
    pub autosave_interval: u8,
    /// Take a file's changes back when something else edits it. ON, unlike
    /// `autosave`: it fires only on a buffer with NOTHING unsaved, so there is
    /// nothing of the reader's to lose, and it is one undo step besides. A
    /// buffer that HAS unsaved work is never reloaded — it is flagged as a
    /// conflict for the reader to resolve.
    pub autoreload: bool,
    pub undo_coalesce_ms: u64,
    pub scroll_off: u16,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            tab_width: 4,
            expand_tab: true,
            auto_indent: true,
            // Off by default: prose is full of unmatched quotes and brackets.
            auto_pair: false,
            trim_on_save: false,
            final_newline: "preserve".into(),
            // Off, and 3 minutes when it goes on.
            autosave: false,
            autosave_interval: crate::fs::save::AutosaveInterval::DEFAULT,
            autoreload: true,
            undo_coalesce_ms: 400,
            scroll_off: 3,
        }
    }
}

/// `[tree]` — the file-tree pane.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct TreeConfig {
    /// Dotfiles listed from the start. `H` still toggles it live; this is only
    /// the state each session opens the tree with.
    pub show_hidden: bool,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub sequence_timeout_ms: u64,
    pub leader: String,
    pub escape_alias: String,
    /// Capture the mouse for click-to-position and wheel scroll. Off leaves the
    /// terminal's native text selection/copy alone.
    pub mouse: bool,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            sequence_timeout_ms: 500,
            leader: " ".into(),
            escape_alias: "jk".into(),
            mouse: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct MarkdownConfig {
    pub tags: bool,
    pub wiki_links: bool,
    pub highlight: bool,
    pub tables: bool,
    /// Syntax-highlight the body of a fenced block whose info string names a
    /// language shoin knows. SPEC.md §5.3.
    pub code_syntax: bool,
    pub plain_text_extensions: Vec<String>,
}

impl Default for MarkdownConfig {
    fn default() -> Self {
        Self {
            tags: true,
            wiki_links: true,
            highlight: true,
            tables: true,
            code_syntax: true,
            plain_text_extensions: vec![".txt".into()],
        }
    }
}

/// Raw color strings; resolved into `render::theme::Theme` after load.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    /// Load `~/.config/shoin/themes/<name>.toml` instead of the inline
    /// values below.
    pub name: Option<String>,
    /// All other `[theme]` keys, kept as strings for `parse_color`.
    #[serde(flatten)]
    pub colors: HashMap<String, toml::Value>,
    pub styles: HashMap<String, bool>,
}

/// Every glyph here is DRAWN by something. Fields for glyphs nothing renders
/// are worse than no config at all — they read as settings that silently do
/// nothing (`link_icon`, `image_icon` and friends were exactly that, removed
/// 2026-08-14), except where a doc comment names the SPEC section still owed.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct GlyphConfig {
    pub nerd_fonts: bool,
    /// The `- `/`* ` a concealed list item shows instead, alternating by
    /// nesting level. One cell each, or the item's text would shift.
    pub bullet: String,
    pub bullet_alt: String,
    pub task_todo: String,
    pub task_done: String,
    pub quote_bar: String,
    /// SPEC §5.4, drawn on concealed lines by `conceal::indent_guide_ops` and
    /// depth-colored by `indent::apply`. Empty turns the guides off.
    pub indent_guide: String,
    /// SPEC §5.3, hung in the left margin beside every row of a fenced
    /// block by `frame::render_fence_bar`. Empty turns the bar off.
    pub fence_bar: String,
    /// File-tree directory icons. The per-file-type icons are not configurable
    /// — there are a hundred of them; see `tree::file_icon`.
    pub folder: String,
    pub folder_open: String,
    pub modified: String,
    /// Drawn beside the filename by `frame::status_text` when the file changed
    /// on disk under a modified buffer. Empty turns the marker off.
    pub conflict: String,
    /// SPEC §6, drawn by `frame::render_scroll_hint` at the pane's right
    /// edge, and only while the document overflows the screen.
    pub scroll_hint: String,
    pub rule: String,
}

impl Default for GlyphConfig {
    fn default() -> Self {
        Self {
            nerd_fonts: true,
            // Typographic, not Nerd Font: these render in any decent font.
            bullet: "•".into(),
            bullet_alt: "◦".into(),
            // Portable ballot boxes so the box/tick shows without a Nerd Font.
            task_todo: "☐".into(),
            task_done: "☑".into(),
            quote_bar: "▎".into(),
            indent_guide: "│".into(),
            fence_bar: "▊".into(),
            // fa-folder / fa-folder-open, as escapes — see `tree::file_icon`.
            folder: "\u{f07b}".into(),
            folder_open: "\u{f07c}".into(),
            modified: "●".into(),
            // Portable warning sign, not a Nerd Font glyph: this is the one
            // marker a reader must not miss for want of a font.
            conflict: "⚠".into(),
            scroll_hint: "▐".into(),
            rule: "─".into(),
        }
    }
}

impl GlyphConfig {
    /// ASCII fallbacks when `nerd_fonts = false`.
    pub fn ascii() -> Self {
        Self {
            nerd_fonts: false,
            bullet: "*".into(),
            bullet_alt: "-".into(),
            task_todo: "[ ]".into(),
            task_done: "[x]".into(),
            quote_bar: "|".into(),
            indent_guide: "|".into(),
            fence_bar: "|".into(),
            // The tree falls back to plain arrows when `nerd_fonts` is off.
            folder: "".into(),
            folder_open: "".into(),
            modified: "*".into(),
            conflict: "!".into(),
            scroll_hint: "|".into(),
            rule: "-".into(),
        }
    }

    /// Apply the `nerd_fonts = false` fallback, once, after the config merges.
    ///
    /// `was_set` answers whether the user wrote that key themselves — asked of
    /// the merged TOML, because serde has already filled every field in by the
    /// time we see the struct, and a glyph deliberately set to the same string
    /// as the default is indistinguishable from an absent one.
    pub fn apply_ascii_fallback(&mut self, was_set: impl Fn(&str) -> bool) {
        if self.nerd_fonts {
            return;
        }
        let ascii = GlyphConfig::ascii();
        let fall_back = |key: &str, field: &mut String, becomes: String| {
            if !was_set(key) {
                *field = becomes;
            }
        };
        fall_back("bullet", &mut self.bullet, ascii.bullet);
        fall_back("bullet_alt", &mut self.bullet_alt, ascii.bullet_alt);
        fall_back("task_todo", &mut self.task_todo, ascii.task_todo);
        fall_back("task_done", &mut self.task_done, ascii.task_done);
        fall_back("quote_bar", &mut self.quote_bar, ascii.quote_bar);
        fall_back("indent_guide", &mut self.indent_guide, ascii.indent_guide);
        fall_back("fence_bar", &mut self.fence_bar, ascii.fence_bar);
        fall_back("folder", &mut self.folder, ascii.folder);
        fall_back("folder_open", &mut self.folder_open, ascii.folder_open);
        fall_back("modified", &mut self.modified, ascii.modified);
        fall_back("conflict", &mut self.conflict, ascii.conflict);
        fall_back("scroll_hint", &mut self.scroll_hint, ascii.scroll_hint);
        fall_back("rule", &mut self.rule, ascii.rule);
    }
}

/// SPEC.md §14.6. Every field here is live — this comment used to say the
/// section was "accepted and validated but ignored until the feature lands",
/// which stopped being true the day transclusion shipped.
///
/// `Clone + PartialEq` because `render::cache::StyleKey` holds a copy: these
/// settings reach an embed's EXPANSION, so changing one has to re-parse the
/// lines that carry an `![[…]]`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default)]
pub struct TranscludeConfig {
    /// How much of an `![[…]]` to expand inline: `none` (the default), `short`,
    /// `rec`, `full`. Named for the command that changes it — `:embed` — since
    /// a setting nobody can find is a setting nobody uses.
    ///
    /// A BOOLEAN is accepted too, and means `short`/`none`. This was a bool
    /// before the modes existed, and one stale value in one file should not
    /// refuse the entire config — the same leniency `[keys.*]` gets.
    #[serde(alias = "preview", deserialize_with = "bool_or_string")]
    pub embed: String,
    /// Search root for bare-name resolution.
    pub root: String,
    pub max_depth: u8,
    /// Demote embedded headings by nesting depth, so the output outline is
    /// coherent.
    pub heading_offset: u8,
    pub strip_frontmatter: bool,
    /// Draw the labelled box around embedded regions.
    pub border: bool,
}

/// Accept either a string or a boolean for a setting that used to be a flag.
fn bool_or_string<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Bool(bool),
        Text(String),
    }
    Ok(match Either::deserialize(d)? {
        Either::Bool(true) => "short".into(),
        Either::Bool(false) => "off".into(),
        Either::Text(s) => s,
    })
}

impl Default for TranscludeConfig {
    fn default() -> Self {
        Self {
            embed: "none".into(),
            root: ".".into(),
            max_depth: 8,
            heading_offset: 1,
            strip_frontmatter: true,
            border: true,
        }
    }
}

/// Empty by default: these are OVERRIDES, and every built-in binding stays
/// active unless a sequence here replaces it.
/// The start screen — what `shoin` with no file argument draws.
/// Empty by default, which means the built-in drawing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SplashConfig {
    /// Which drawing to use:
    ///   `""`      the built-in bonsai (default)
    ///   `"none"`  no drawing at all — just the hints
    ///   a path    a text file to draw instead
    ///
    /// A relative path is resolved against the config directory, so an
    /// `art.txt` beside your `.conf` files just works. `~` is expanded.
    pub art: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    pub normal: HashMap<String, String>,
    pub insert: HashMap<String, String>,
    pub visual: HashMap<String, String>,
}
