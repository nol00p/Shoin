//! Theme and color resolution.
//!
//! This is the ONLY module that inspects `COLORTERM`/`TERM`. Colors are
//! authored as truecolor and downgraded here: 24-bit → nearest 256 index →
//! nearest of 16, depending on terminal capability.

use anyhow::Result;

use ratatui::style::{Color as RtColor, Modifier, Style as RtStyle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Rgb(u8, u8, u8),
    Indexed(u8),
    /// Terminal default / transparent.
    None,
}

impl Color {
    /// Lower to a ratatui color. `None` maps to `Reset` (the terminal default).
    pub fn to_ratatui(self) -> RtColor {
        match self {
            Color::Rgb(r, g, b) => RtColor::Rgb(r, g, b),
            Color::Indexed(i) => RtColor::Indexed(i),
            Color::None => RtColor::Reset,
        }
    }

    /// The color as CSS, for HTML export. `None` has no color of its own and
    /// answers `None` so the caller can leave the property out.
    ///
    /// An indexed color is resolved through the xterm palette rather than
    /// emitted as a slot number, because a PAGE has no slots: the whole point
    /// of the terminal's 16 is that the terminal decides what they look like,
    /// and a browser cannot ask it. This matters more than it sounds — a theme
    /// `adapt`ed for a 256-color terminal is entirely indexed, and exporting
    /// from one would otherwise produce a page with no colors at all.
    pub fn to_css(self) -> Option<String> {
        let (r, g, b) = match self {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(i) => xterm_rgb(i),
            Color::None => return None,
        };
        Some(format!("#{r:02x}{g:02x}{b:02x}"))
    }
}

/// The RGB an xterm palette index stands for: the 16 system colors as xterm
/// defines them, then the 6×6×6 cube, then the 24-step grey ramp.
fn xterm_rgb(i: u8) -> (u8, u8, u8) {
    const SYSTEM: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00), (0x80, 0x00, 0x00), (0x00, 0x80, 0x00), (0x80, 0x80, 0x00),
        (0x00, 0x00, 0x80), (0x80, 0x00, 0x80), (0x00, 0x80, 0x80), (0xc0, 0xc0, 0xc0),
        (0x80, 0x80, 0x80), (0xff, 0x00, 0x00), (0x00, 0xff, 0x00), (0xff, 0xff, 0x00),
        (0x00, 0x00, 0xff), (0xff, 0x00, 0xff), (0x00, 0xff, 0xff), (0xff, 0xff, 0xff),
    ];
    match i {
        0..=15 => SYSTEM[i as usize],
        16..=231 => {
            let n = i - 16;
            let step = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (step(n / 36), step((n / 6) % 6), step(n % 6))
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            (v, v, v)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attrs {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub dim: bool,
    pub reverse: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

impl Style {
    /// A foreground-only style over the terminal default background.
    pub const fn fg(fg: Color) -> Self {
        Style {
            fg,
            bg: Color::None,
            attrs: Attrs {
                bold: false,
                italic: false,
                underline: false,
                strikethrough: false,
                dim: false,
                reverse: false,
            },
        }
    }

    pub fn to_ratatui(self) -> RtStyle {
        let mut s = RtStyle::default().fg(self.fg.to_ratatui());
        if self.bg != Color::None {
            s = s.bg(self.bg.to_ratatui());
        }
        let a = self.attrs;
        let mut m = Modifier::empty();
        if a.bold {
            m |= Modifier::BOLD;
        }
        if a.italic {
            m |= Modifier::ITALIC;
        }
        if a.underline {
            m |= Modifier::UNDERLINED;
        }
        if a.strikethrough {
            m |= Modifier::CROSSED_OUT;
        }
        if a.dim {
            m |= Modifier::DIM;
        }
        if a.reverse {
            m |= Modifier::REVERSED;
        }
        s.add_modifier(m)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Indexed256,
    Basic16,
}

#[derive(Clone, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub text: Color,
    pub text_dim: Color,
    pub cursor: Color,
    /// `Color::None` disables the current-line highlight entirely.
    pub cursor_line: Color,
    pub selection: Color,
    pub search_match: Color,
    /// The foreground drawn ON a selection (Visual, its line-break cell, the
    /// `:s///c` match, a selected tree or finder row) and on a search match.
    /// `None` keeps each glyph's own colour, which is what the authored
    /// truecolour theme does. [`Theme::adapt`] sets both on 16- and
    /// 256-colour terminals, as Neovim's cterm defaults give `Visual`
    /// (`ctermfg=0 ctermbg=15`) and `Search` (`ctermfg=0 ctermbg=11`) a
    /// foreground of their own: once colours collapse onto slots, a span's
    /// own colour (bold, a link, code) can be the overlay's. Not `[theme]`
    /// keys: they follow from `selection` and `search_match`.
    pub selection_fg: Color,
    pub search_match_fg: Color,
    /// The foreground a glyph takes on an inline `==highlight==` when its own
    /// colour (a heading's, bold's, a link's) is the highlight's slot.
    /// `None` in truecolour, where nothing is replaced; [`Theme::adapt`] sets
    /// it on 16- and 256-colour terminals, as for [`Theme::selection_fg`].
    pub highlight_fg: Color,
    /// The status bar's message and error foregrounds. `None` in truecolour,
    /// where a message keeps the terminal's own foreground and an error is
    /// ANSI red; on 16- and 256-colour terminals [`Theme::adapt`] sets them
    /// to `text` and `error`, which it keeps off the bar's background.
    pub message_fg: Color,
    pub message_error_fg: Color,

    pub headings: [Color; 6],

    pub bold: Color,
    pub italic: Color,
    pub strikethrough: Color,
    pub highlight_bg: Color,

    pub code: Color,
    pub code_bg: Color,
    pub fence_bar: Color,

    /// Syntax highlighting inside a fenced block. Code the
    /// lexer does not classify keeps `text`, not `code` — a highlighted body
    /// reads as code because of the slab it sits on, and painting its ordinary
    /// identifiers green would leave nothing for the strings.
    pub syntax_keyword: Color,
    pub syntax_type: Color,
    pub syntax_string: Color,
    /// Numbers and language constants (`true`, `nil`, `None`).
    pub syntax_literal: Color,
    pub syntax_comment: Color,
    pub syntax_function: Color,
    /// Operators and delimiters, which should recede rather than shout.
    pub syntax_punct: Color,

    /// The conflict diff view. Whole-line foregrounds, the way
    /// `git diff` colours them, rather than background tints: a background
    /// wash fights the concealment colours the left pane is already using.
    pub diff_add: Color,
    pub diff_remove: Color,
    /// Both sides have a line here and they differ — the commonest row, so it
    /// gets the colour that reads as "look at this" rather than as good or bad.
    pub diff_change: Color,

    pub link: Color,
    pub wiki_link: Color,
    pub tag: Color,
    pub tag_bg: Color,

    pub quote: Color,
    pub quote_bar: Color,
    pub list_bullet: Color,
    pub task_done: Color,
    pub rule: Color,
    pub table_border: Color,

    pub indent_colors: Vec<Color>,

    pub status_fg: Color,
    pub status_bg: Color,
    pub error: Color,

    /// The `[theme]` color keys the user set to something other than the
    /// built-in default, in the config or a theme file. [`Theme::adapt`]
    /// preserves their palette choices and checks converted RGB for contrast.
    pub explicit: std::collections::BTreeSet<String>,

    pub dim_markers: bool,
    pub heading_bold: bool,
    /// Headings in italic as well as bold. Off by default: at heading weight it
    /// is a lot of slant, and not every terminal font has a real italic.
    pub heading_italic: bool,
    pub bold_attr: bool,
    pub italic_attr: bool,
    pub link_underline: bool,
}

impl Default for Theme {
    /// The built-in dark palette. Authored in truecolor; `adapt` (build-order 8)
    /// downgrades it for lesser terminals, and a `[theme]` config overrides it.
    fn default() -> Self {
        const fn rgb(r: u8, g: u8, b: u8) -> Color {
            Color::Rgb(r, g, b)
        }
        // Tokyo Night (Night). Override any of these via `[theme]` in the config.
        //
        // THE PALETTE these are drawn from — the 16 ANSI slots Tokyo Night
        // defines, which is the authority for anything that should read as a
        // terminal color:
        //   0 black #15161e   1 red     #f7768e   2 green   #9ece6a
        //   3 yellow #e0af68  4 blue    #7aa2f7   5 magenta #bb9af7
        //   6 cyan  #7dcfff   7 white   #a9b1d6   8 br.black #414868
        //   9-15 repeat 1-7 (Tokyo Night does not brighten them)
        //
        // Roles OFF that palette are Tokyo Night's UI colors, which have no
        // slot and must not be forced into one: bg #1a1b26, fg #c0caf5,
        // comment #565f89, selection #283457, orange #ff9e64,
        // bg_highlight #24283b, blue0 #3d59a1, fg_dark #9aa5ce. Slot 0 in
        // particular is the TERMINAL's black and is darker than the editor
        // background it would replace.
        Theme {
            background: rgb(0x1a, 0x1b, 0x26),
            text: rgb(0xc0, 0xca, 0xf5),
            text_dim: rgb(0x56, 0x5f, 0x89),
            cursor: rgb(0xc0, 0xca, 0xf5),
            cursor_line: Color::None,
            selection: rgb(0x28, 0x34, 0x57),
            search_match: rgb(0xe0, 0xaf, 0x68),
            selection_fg: Color::None,
            search_match_fg: Color::None,
            highlight_fg: Color::None,
            message_fg: Color::None,
            message_error_fg: Color::None,

            // The six terminal hues in order — red, green, yellow, blue,
            // magenta, cyan — so a heading's level is told by WHICH color it
            // is, not by a shade of one. The old ramp ran blue -> cyan -> teal
            // -> green, where levels 2 and 3 were hard to tell apart.
            headings: [
                rgb(0xf7, 0x76, 0x8e), // coral
                rgb(0x9e, 0xce, 0x6a), // apple green
                rgb(0xe0, 0xaf, 0x68), // sand
                rgb(0x7a, 0xa2, 0xf7), // cornflower
                rgb(0xbb, 0x9a, 0xf7), // lavender
                rgb(0x7d, 0xcf, 0xff), // sky
            ],

            bold: rgb(0xff, 0x9e, 0x64),
            italic: rgb(0xc0, 0xca, 0xf5),
            strikethrough: rgb(0x56, 0x5f, 0x89),
            highlight_bg: rgb(0x3d, 0x59, 0xa1),

            code: rgb(0x9e, 0xce, 0x6a),
            code_bg: rgb(0x24, 0x28, 0x3b),
            fence_bar: rgb(0x41, 0x48, 0x68),

            syntax_keyword: rgb(0xbb, 0x9a, 0xf7),  // lavender
            syntax_type: rgb(0x7d, 0xcf, 0xff),     // sky
            syntax_string: rgb(0x9e, 0xce, 0x6a),   // apple green
            syntax_literal: rgb(0xff, 0x9e, 0x64),  // orange
            syntax_comment: rgb(0x56, 0x5f, 0x89),  // comment
            syntax_function: rgb(0x7a, 0xa2, 0xf7), // cornflower
            syntax_punct: rgb(0x9a, 0xa5, 0xce),    // fg_dark

            diff_add: rgb(0x9e, 0xce, 0x6a),        // apple green
            diff_remove: rgb(0xf7, 0x76, 0x8e),     // red
            diff_change: rgb(0xe0, 0xaf, 0x68),     // yellow

            link: rgb(0x7a, 0xa2, 0xf7),
            wiki_link: rgb(0xbb, 0x9a, 0xf7),
            tag: rgb(0x7d, 0xcf, 0xff),
            tag_bg: rgb(0x28, 0x34, 0x57),

            quote: rgb(0x9a, 0xa5, 0xce),
            quote_bar: rgb(0x41, 0x48, 0x68),
            list_bullet: rgb(0x7a, 0xa2, 0xf7),
            task_done: rgb(0x56, 0x5f, 0x89),
            rule: rgb(0x41, 0x48, 0x68),
            table_border: rgb(0x41, 0x48, 0x68),

            indent_colors: vec![
                rgb(0x41, 0x48, 0x68),
                rgb(0x56, 0x5f, 0x89),
                rgb(0x7a, 0xa2, 0xf7),
                rgb(0xbb, 0x9a, 0xf7),
            ],

            status_fg: rgb(0x56, 0x5f, 0x89),
            // Tokyo Night's bg_dark, its own statusline background: a shade
            // under the page, so the bottom row reads as a bar, quietly.
            status_bg: rgb(0x16, 0x16, 0x1e),
            error: rgb(0xf7, 0x76, 0x8e),

            explicit: std::collections::BTreeSet::new(),

            dim_markers: true,
            heading_bold: true,
            heading_italic: false,
            bold_attr: true,
            italic_attr: true,
            link_underline: true,
        }
    }
}

impl Theme {
    /// `s` as a selected row or cell draws it: the selection background,
    /// and over a 16- or 256-colour terminal its own foreground
    /// ([`Theme::selection_fg`]).
    pub fn selected(&self, s: RtStyle) -> RtStyle {
        let s = s.bg(self.selection.to_ratatui());
        if self.selection_fg == Color::None { s } else { s.fg(self.selection_fg.to_ratatui()) }
    }

    /// Resolve a `[theme]` config section into a full theme: start from a base
    /// (a named theme file, or the built-in default), overlay the config's
    /// colors and style flags, then downgrade for the terminal.
    pub fn from_config(cfg: &crate::config::schema::ThemeConfig) -> Result<Theme> {
        let mut theme = Theme::authored(cfg)?;
        theme.adapt(detect_depth());
        Ok(theme)
    }

    /// The theme AS AUTHORED, before any downgrade for the terminal.
    ///
    /// HTML export wants this one: a page has no color depth to accommodate, so
    /// exporting from a 256-color terminal should still produce the colors the
    /// theme actually names rather than the terminal's nearest approximations
    /// to them.
    pub fn authored(cfg: &crate::config::schema::ThemeConfig) -> Result<Theme> {
        let mut theme = match &cfg.name {
            Some(name) => Theme::load_named(name).unwrap_or_default(),
            None => Theme::default(),
        };
        theme.apply_colors(&cfg.colors)?;
        for (key, on) in &cfg.styles {
            theme.set_style(key, *on);
        }
        Ok(theme)
    }

    /// Load a named theme from `~/.config/shoin/themes/<name>.toml`. The
    /// file is a flat table of the same color/style keys as `[theme]`.
    pub fn load_named(name: &str) -> Result<Theme> {
        let dir = std::env::var("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
            .map_err(|_| anyhow::anyhow!("no config dir for theme {name}"))?;
        let path = dir
            .join("shoin")
            .join("themes")
            .join(format!("{name}.toml"));
        let text = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("theme {name}: {e}"))?;
        let cfg: crate::config::schema::ThemeConfig = toml::from_str(&text)?;
        // A named file cannot itself reference another name — start from default.
        let mut theme = Theme::default();
        theme.apply_colors(&cfg.colors)?;
        for (key, on) in &cfg.styles {
            theme.set_style(key, *on);
        }
        Ok(theme)
    }

    fn apply_colors(&mut self, colors: &std::collections::HashMap<String, toml::Value>) -> Result<()> {
        let mut defaults = Theme::default();
        for (key, value) in colors {
            if key == "indent_colors" {
                if let Some(arr) = value.as_array() {
                    let mut out = Vec::new();
                    for v in arr {
                        out.push(value_to_color(v)?);
                    }
                    if !out.is_empty() {
                        if out != defaults.indent_colors {
                            self.explicit.insert(key.clone());
                        } else {
                            self.explicit.remove(key);
                        }
                        self.indent_colors = out;
                    }
                }
                continue;
            }
            let color = value_to_color(value)?;
            if let Some(slot) = self.color_mut(key) {
                *slot = color;
                // The shipped theme.conf (`--init-config`) writes out every
                // default: a color is the user's own only if it differs.
                if defaults.color_mut(key).is_some_and(|d| *d != color) {
                    self.explicit.insert(key.clone());
                } else {
                    self.explicit.remove(key);
                }
            }
        }
        Ok(())
    }

    /// The color a `[theme]` key names; `None` for a key it doesn't know.
    /// Unknown keys are ignored, for forward compatibility.
    fn color_mut(&mut self, key: &str) -> Option<&mut Color> {
        match key {
            "background" => Some(&mut self.background),
            "text" => Some(&mut self.text),
            "text_dim" => Some(&mut self.text_dim),
            "cursor" => Some(&mut self.cursor),
            "cursor_line" => Some(&mut self.cursor_line),
            "selection" => Some(&mut self.selection),
            "search_match" => Some(&mut self.search_match),
            "heading_1" => Some(&mut self.headings[0]),
            "heading_2" => Some(&mut self.headings[1]),
            "heading_3" => Some(&mut self.headings[2]),
            "heading_4" => Some(&mut self.headings[3]),
            "heading_5" => Some(&mut self.headings[4]),
            "heading_6" => Some(&mut self.headings[5]),
            "bold" => Some(&mut self.bold),
            "italic" => Some(&mut self.italic),
            "strikethrough" => Some(&mut self.strikethrough),
            "highlight_bg" => Some(&mut self.highlight_bg),
            "code" => Some(&mut self.code),
            "code_bg" => Some(&mut self.code_bg),
            "fence_bar" => Some(&mut self.fence_bar),
            "syntax_keyword" => Some(&mut self.syntax_keyword),
            "syntax_type" => Some(&mut self.syntax_type),
            "syntax_string" => Some(&mut self.syntax_string),
            "syntax_literal" => Some(&mut self.syntax_literal),
            "syntax_comment" => Some(&mut self.syntax_comment),
            "syntax_function" => Some(&mut self.syntax_function),
            "syntax_punct" => Some(&mut self.syntax_punct),
            "diff_add" => Some(&mut self.diff_add),
            "diff_remove" => Some(&mut self.diff_remove),
            "diff_change" => Some(&mut self.diff_change),
            "link" => Some(&mut self.link),
            "wiki_link" => Some(&mut self.wiki_link),
            "tag" => Some(&mut self.tag),
            "tag_bg" => Some(&mut self.tag_bg),
            "quote" => Some(&mut self.quote),
            "quote_bar" => Some(&mut self.quote_bar),
            "list_bullet" => Some(&mut self.list_bullet),
            "task_done" => Some(&mut self.task_done),
            "rule" => Some(&mut self.rule),
            "table_border" => Some(&mut self.table_border),
            "status_fg" => Some(&mut self.status_fg),
            "status_bg" => Some(&mut self.status_bg),
            "error" => Some(&mut self.error),
            _ => None,
        }
    }

    fn set_style(&mut self, key: &str, on: bool) {
        match key {
            "dim_markers" => self.dim_markers = on,
            "heading_bold" => self.heading_bold = on,
            "heading_italic" => self.heading_italic = on,
            "bold_attr" => self.bold_attr = on,
            "italic_attr" => self.italic_attr = on,
            "link_underline" => self.link_underline = on,
            _ => {}
        }
    }

    /// Downgrade every color to what the terminal can actually show.
    pub fn adapt(&mut self, depth: ColorDepth) {
        if depth == ColorDepth::TrueColor {
            return;
        }
        let explicit = std::mem::take(&mut self.explicit);
        let explicit_palette: std::collections::BTreeSet<_> = explicit.iter()
            .filter(|key| self.color_mut(key).is_some_and(|c| !matches!(c, Color::Rgb(..))))
            .map(String::as_str)
            .collect();
        macro_rules! down {
            ($($f:ident),*) => { $( self.$f = self.$f.downgrade(depth); )* };
        }
        down!(
            background, text, text_dim, cursor, cursor_line, selection, search_match, bold,
            italic, strikethrough, highlight_bg, code, code_bg, fence_bar, syntax_keyword,
            syntax_type, syntax_string, syntax_literal, syntax_comment, syntax_function,
            syntax_punct, diff_add, diff_remove, diff_change, link, wiki_link, tag, tag_bg,
            quote, quote_bar, list_bullet, task_done,
            rule, table_border, status_fg, status_bg, error
        );
        for h in self.headings.iter_mut() {
            *h = h.downgrade(depth);
        }
        // Colours are compared by palette SLOT. A terminal decides what each
        // slot looks like (slot 0 and slot 16 are both "black" only in
        // xterm's own palette), so two slots are two colours, and one slot is
        // one colour whatever RGB it was converted from.
        //
        // Preserve the user's palette choices. An RGB colour's palette slot
        // comes from conversion, so it still goes through the contrast guard.
        let guard = |key: &str, c: &mut Color, against: &[Color], preferred: [u8; 2]| {
            if !explicit_palette.contains(key) {
                *c = c.with_indexed_contrast(against, preferred);
            }
        };
        let page = self.background;
        guard("text", &mut self.text, &[page], [15, 0]);
        guard("text_dim", &mut self.text_dim, &[page], [8, 7]);
        // Everything else drawn straight on the page.
        for (key, color) in [
            ("cursor", &mut self.cursor), ("bold", &mut self.bold), ("italic", &mut self.italic),
            ("strikethrough", &mut self.strikethrough), ("fence_bar", &mut self.fence_bar),
            ("diff_add", &mut self.diff_add), ("diff_remove", &mut self.diff_remove),
            ("diff_change", &mut self.diff_change), ("link", &mut self.link),
            ("wiki_link", &mut self.wiki_link), ("quote", &mut self.quote),
            ("quote_bar", &mut self.quote_bar), ("list_bullet", &mut self.list_bullet),
            ("task_done", &mut self.task_done), ("rule", &mut self.rule),
            ("table_border", &mut self.table_border), ("error", &mut self.error),
        ] {
            guard(key, color, &[page], [15, 0]);
        }
        for (key, color) in ["heading_1", "heading_2", "heading_3", "heading_4", "heading_5", "heading_6"]
            .into_iter()
            .zip(&mut self.headings)
        {
            guard(key, color, &[page], [15, 0]);
        }
        let explicit_indents = explicit.contains("indent_colors");
        for color in &mut self.indent_colors {
            let keep = explicit_indents && !matches!(color, Color::Rgb(..));
            *color = color.downgrade(depth);
            if !keep {
                *color = color.with_indexed_contrast(&[page], [15, 0]);
            }
        }
        let prose = [page, self.text, self.text_dim];

        // Backgrounds, each against every colour the renderer draws on it.
        // Default CursorLine has no ctermbg. Use an ANSI grey for a configured
        // cursor line, following the grey used by Neovim's GUI highlight.
        guard("cursor_line", &mut self.cursor_line, &prose, [8, 7]);
        // Other glyphs on a highlight are seen to at render time, through
        // `highlight_fg`.
        guard("highlight_bg", &mut self.highlight_bg, &[page, self.text], [11, 3]);
        // A link label can hold inline code or a tag: the link colour on the
        // code or tag background.
        let links = [self.link, self.wiki_link];
        guard("code_bg", &mut self.code_bg, &[&[page, self.text, self.code][..], &links].concat(), [8, 7]);
        guard("tag_bg", &mut self.tag_bg, &[&[page, self.tag][..], &links].concat(), [8, 7]);
        // The bar draws hint keys in `list_bullet`, what they do in
        // `text_dim`, a message in `text` and an error in `error`.
        guard(
            "status_bg",
            &mut self.status_bg,
            &[page, self.status_fg, self.text, self.text_dim, self.list_bullet, self.error],
            [8, 7],
        );

        let effective_bg = |bg| if bg == Color::None { page } else { bg };
        guard("tag", &mut self.tag, &[effective_bg(self.tag_bg)], [15, 0]);
        guard("code", &mut self.code, &[effective_bg(self.code_bg)], [15, 0]);
        guard("status_fg", &mut self.status_fg, &[effective_bg(self.status_bg)], [15, 0]);
        let slab = effective_bg(self.code_bg);
        for (key, color) in [
            ("syntax_keyword", &mut self.syntax_keyword), ("syntax_type", &mut self.syntax_type),
            ("syntax_string", &mut self.syntax_string), ("syntax_literal", &mut self.syntax_literal),
            ("syntax_comment", &mut self.syntax_comment), ("syntax_function", &mut self.syntax_function),
            ("syntax_punct", &mut self.syntax_punct),
        ] {
            guard(key, color, &[slab], [15, 0]);
        }

        // The overlays carry their own foreground, as Neovim's cterm
        // `Visual` and `Search` do, so their backgrounds need only differ
        // from the backgrounds they are painted over. The fallbacks are
        // Neovim's: white (15) for Visual, bright yellow (11) for Search.
        // The `:s///c` match is drawn in the selection's pair over the
        // search highlights (Neovim's `CurSearch` and `Search` are one cterm
        // pair, which would not tell it apart), so the selection must also
        // differ from the search colour.
        let under = [page, self.code_bg, self.tag_bg, self.highlight_bg, self.cursor_line];
        guard("search_match", &mut self.search_match, &under, [11, 3]);
        guard("selection", &mut self.selection, &[&under[..], &[self.search_match]].concat(), [15, 7]);
        self.selection_fg = self.selection.indexed_overlay_fg();
        self.search_match_fg = self.search_match.indexed_overlay_fg();
        self.highlight_fg = self.highlight_bg.indexed_overlay_fg();
        // Status messages in theme colours, which `status_bg` avoids: the
        // terminal's own foreground could be the bar's slot.
        self.message_fg = self.text;
        self.message_error_fg = self.error;
        self.explicit = explicit;
    }
}

impl Color {
    fn downgrade(self, depth: ColorDepth) -> Color {
        match (self, depth) {
            (Color::Rgb(r, g, b), ColorDepth::Indexed256) => Color::Indexed(rgb_to_256(r, g, b)),
            (Color::Rgb(r, g, b), ColorDepth::Basic16) => Color::Indexed(rgb_to_16(r, g, b)),
            (Color::Indexed(i @ 16..), ColorDepth::Basic16) => {
                let (r, g, b) = xterm_rgb(i);
                Color::Indexed(rgb_to_16(r, g, b))
            }
            _ => self,
        }
    }

    /// This slot if no colour in `against` is the same slot; else the first
    /// of `preferred`, then of a fixed order, that is none of them.
    fn with_indexed_contrast(self, against: &[Color], preferred: [u8; 2]) -> Color {
        let Color::Indexed(index) = self else { return self };
        let distinct = |index| !against.contains(&Color::Indexed(index));
        if distinct(index) {
            return self;
        }
        preferred.into_iter().chain([8, 7, 15, 0]).chain(0..16)
            .find(|&index| distinct(index))
            .map_or(self, Color::Indexed)
    }
}

impl Color {
    /// The foreground an overlay in this background slot draws its text in:
    /// black (0), as Neovim's cterm `Visual` and `Search` do, unless the
    /// slot is a dark one, then bright white (15). The 16 ANSI slots are
    /// judged by convention, not by RGB, since the terminal sets those:
    /// 0 to 6 and 8 are the dark ones. The 256-colour cube and grey ramp are
    /// judged by their xterm values. A transparent overlay keeps the text's
    /// own colour.
    fn indexed_overlay_fg(self) -> Color {
        let Color::Indexed(i) = self else { return Color::None };
        let dark = match i {
            0..=6 | 8 => true,
            7 | 9..=15 => false,
            _ => {
                let (r, g, b) = xterm_rgb(i);
                (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000 < 128
            }
        };
        Color::Indexed(if dark { 15 } else { 0 })
    }
}

/// A `toml` scalar as a color: a string goes through `parse_color`; a bare
/// integer is a 256-color index.
fn value_to_color(v: &toml::Value) -> Result<Color> {
    if let Some(s) = v.as_str() {
        return parse_color(s);
    }
    if let Some(n) = v.as_integer() {
        if (0..=255).contains(&n) {
            return Ok(Color::Indexed(n as u8));
        }
    }
    anyhow::bail!("not a color: {v}")
}

/// Inspect `COLORTERM` then `TERM`. The single environment check in the program.
pub fn detect_depth() -> ColorDepth {
    if let Ok(ct) = std::env::var("COLORTERM") {
        let ct = ct.to_ascii_lowercase();
        if ct.contains("truecolor") || ct.contains("24bit") {
            return ColorDepth::TrueColor;
        }
    }
    match std::env::var("TERM") {
        Ok(term) if term.ends_with("-16color") => ColorDepth::Basic16,
        Ok(term) if term.contains("256") || term.contains("direct") => ColorDepth::Indexed256,
        Ok(term) if !term.is_empty() && term != "dumb" => ColorDepth::Indexed256,
        _ => ColorDepth::Basic16,
    }
}

/// `"#rrggbb"` | `"#rgb"` | `"0".."255"` | `"bright_black"` | `"none"`
pub fn parse_color(s: &str) -> Result<Color> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("default") {
        return Ok(Color::None);
    }
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex(hex);
    }
    if let Ok(n) = s.parse::<u8>() {
        return Ok(Color::Indexed(n));
    }
    if let Some(idx) = named_color(s) {
        return Ok(Color::Indexed(idx));
    }
    anyhow::bail!("invalid color: {s:?}")
}

fn parse_hex(h: &str) -> Result<Color> {
    let byte = |s: &str| u8::from_str_radix(s, 16);
    match h.len() {
        6 => Ok(Color::Rgb(byte(&h[0..2])?, byte(&h[2..4])?, byte(&h[4..6])?)),
        3 => {
            let dup = |c: &str| byte(&format!("{c}{c}"));
            Ok(Color::Rgb(dup(&h[0..1])?, dup(&h[1..2])?, dup(&h[2..3])?))
        }
        _ => anyhow::bail!("invalid hex color: #{h}"),
    }
}

/// The 16 ANSI color names -> palette index.
fn named_color(s: &str) -> Option<u8> {
    let base = |n: &str| match n {
        "black" => Some(0),
        "red" => Some(1),
        "green" => Some(2),
        "yellow" => Some(3),
        "blue" => Some(4),
        "magenta" => Some(5),
        "cyan" => Some(6),
        "white" | "grey" | "gray" => Some(7),
        _ => None,
    };
    let s = s.to_ascii_lowercase();
    if let Some(rest) = s.strip_prefix("bright_") {
        return base(rest).map(|n| n + 8);
    }
    base(&s)
}

/// Nearest xterm 256-color index for an RGB triple (6×6×6 cube + grayscale ramp).
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        return match r {
            0..=7 => 16,
            248..=255 => 231,
            v => 232 + ((v as u16 - 8) * 24 / 247) as u8,
        };
    }
    let q = |c: u8| c as u16 * 5 / 255;
    (16 + 36 * q(r) + 6 * q(g) + q(b)) as u8
}

/// Nearest of the 16 ANSI colors: primary bits by channel, plus a bright bit.
///
/// A channel counts when it is lit (over 100) and at least half the
/// brightest one, so a hue keeps its name: sand `#e0af68` is yellow, not
/// white because its blue passes 100, and coral `#f7768e` is magenta.
fn rgb_to_16(r: u8, g: u8, b: u8) -> u8 {
    let max = u16::from(r.max(g).max(b));
    let lit = |c: u8| c > 100 && 2 * u16::from(c) >= max;
    let mut idx = 0u8;
    if lit(r) {
        idx |= 1;
    }
    if lit(g) {
        idx |= 2;
    }
    if lit(b) {
        idx |= 4;
    }
    let bright = if r.max(g).max(b) > 170 { 8 } else { 0 };
    idx + bright
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ThemeConfig;

    #[test]
    fn parse_color_forms() {
        assert_eq!(parse_color("#7aa2f7").unwrap(), Color::Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(parse_color("#abc").unwrap(), Color::Rgb(0xaa, 0xbb, 0xcc));
        assert_eq!(parse_color("none").unwrap(), Color::None);
        assert_eq!(parse_color("42").unwrap(), Color::Indexed(42));
        assert_eq!(parse_color("red").unwrap(), Color::Indexed(1));
        assert_eq!(parse_color("bright_black").unwrap(), Color::Indexed(8));
        assert!(parse_color("#12").is_err());
        assert!(parse_color("chartreuse").is_err());
    }

    /// The shipped `shoin/theme.conf` IS the built-in default written out, and
    /// this is what keeps the two from drifting.
    ///
    /// They had already drifted: `selection` and `tag_bg` read `#283457` in the
    /// config and `#282e44` in `Theme::default()`. Running from the repo loads
    /// `./shoin/*.conf`, so the config wins there and the code default wins
    /// everywhere else — the two answers never meet, and nobody notices.
    #[test]
    fn the_shipped_theme_matches_the_built_in_default() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shoin/theme.conf");
        let text = std::fs::read_to_string(&path).expect("shoin/theme.conf ships with the repo");
        // Through the real config path, so the `[theme]` section is picked out
        // the way the editor picks it out — parsing the file straight into a
        // `ThemeConfig` silently flattens the section name into a color key,
        // and every assertion below it passes without checking anything.
        let cfg = crate::config::parse(&text).expect("theme.conf parses").theme;

        // One key at a time FIRST, so a mismatch names the role. The wholesale
        // check below would fire on the same drift, but only to say that two
        // palettes differ somewhere.
        for (key, value) in &cfg.colors {
            if key == "indent_colors" {
                continue;
            }
            let mut one = Theme::default();
            *one.color_mut(key).unwrap() = value_to_color(value).unwrap();
            assert!(
                one == Theme::default(),
                "theme.conf `{key}` = {value} differs from Theme::default()"
            );
        }

        // Then wholesale, which is what catches a role the file OMITS — the
        // per-key loop can only check the keys that are there.
        // AUTHORED, not `from_config`: the latter ends with `adapt(detect_depth())`,
        // so it answers with the palette the DEVELOPER'S terminal can show while
        // `Theme::default()` is always the authored one. On a 16-color terminal
        // — which is what a CI runner is, with no COLORTERM and no TERM — every
        // role downgrades onto an indexed slot and the two sides cannot match.
        // The question this test asks is about two authored palettes; the
        // terminal has no business in the answer.
        assert!(
            Theme::authored(&cfg).unwrap() == Theme::default(),
            "shoin/theme.conf and Theme::default() disagree — change both, or neither"
        );
    }

    /// Overlaying is `authored`'s job — `from_config` is `authored` plus a
    /// downgrade for the terminal — so this asks `authored` and gets the same
    /// answer on every machine.
    ///
    /// It used to ask `from_config` and wrap the color assertions in
    /// `if detect_depth() == TrueColor`, which meant they simply did not run on
    /// a 256-color terminal, while the unguarded `background` line compared an
    /// ADAPTED color against an unadapted `Theme::default()` and failed outright
    /// anywhere below truecolor. A test that skips itself on one machine and
    /// fails on another is worse than no test.
    #[test]
    fn authored_overlays_colors_and_styles() {
        let toml = r##"
            text = "#010203"
            heading_1 = "#0a0b0c"
            indent_colors = ["#111111", "#222222"]
            [styles]
            heading_bold = false
        "##;
        let cfg: ThemeConfig = toml::from_str(toml).unwrap();
        let theme = Theme::authored(&cfg).unwrap();
        assert_eq!(theme.text, Color::Rgb(1, 2, 3));
        assert_eq!(theme.headings[0], Color::Rgb(0x0a, 0x0b, 0x0c));
        assert_eq!(theme.indent_colors.len(), 2);
        assert!(!theme.heading_bold);
        // A key we didn't set keeps the default.
        assert_eq!(theme.background, Theme::default().background);
    }

    /// …and `from_config` is exactly that, downgraded — which is the half the
    /// test above deliberately leaves out, pinned here to a depth rather than
    /// read from the environment.
    #[test]
    fn from_config_is_authored_then_adapted() {
        let cfg = ThemeConfig::default();
        let mut expected = Theme::authored(&cfg).unwrap();
        expected.adapt(detect_depth());
        assert!(Theme::from_config(&cfg).unwrap() == expected);
    }

    #[test]
    fn built_in_theme_has_distinct_indexed_colors() {
        // Tokyo Night (Night) is the sole built-in theme. theme.conf mirrors it.
        for depth in [ColorDepth::Basic16, ColorDepth::Indexed256] {
            let mut t = Theme::default();
            t.adapt(depth);
            for (name, color) in [
                ("selection", t.selection), ("search_match", t.search_match),
                ("cursor_line", t.cursor_line), ("highlight_bg", t.highlight_bg),
                ("code_bg", t.code_bg), ("tag_bg", t.tag_bg), ("status_bg", t.status_bg),
                ("text", t.text), ("text_dim", t.text_dim),
            ] {
                if color != Color::None {
                    assert_ne!(color, t.background, "Tokyo Night {depth:?} {name}");
                }
            }
            for (fg, bg) in [
                (t.selection_fg, t.selection), (t.search_match_fg, t.search_match),
                (t.text, t.highlight_bg), (t.text, t.code_bg), (t.code, t.code_bg),
                (t.link, t.code_bg), (t.link, t.tag_bg), (t.tag, t.tag_bg),
                (t.status_fg, t.status_bg), (t.text, t.status_bg), (t.text_dim, t.status_bg),
                (t.list_bullet, t.status_bg), (t.error, t.status_bg),
            ] {
                assert_ne!(fg, Color::None, "Tokyo Night {depth:?}: a foreground is set");
                assert_ne!(fg, bg, "Tokyo Night {depth:?} foreground on {bg:?}");
            }
            // The overlays differ from the backgrounds they are painted
            // over, and the `:s` match (the selection's pair) from search.
            for bg in [t.background, t.code_bg, t.tag_bg, t.highlight_bg, t.search_match] {
                assert_ne!(t.selection, bg, "Tokyo Night {depth:?} selection");
            }
        }
        // Neovim's cterm pairs: Visual black on white, Search black on yellow.
        let mut t = Theme::default();
        t.adapt(ColorDepth::Basic16);
        assert_eq!((t.selection_fg, t.selection), (Color::Indexed(0), Color::Indexed(15)));
        assert_eq!((t.search_match_fg, t.search_match), (Color::Indexed(0), Color::Indexed(11)));
        // Truecolor keeps each glyph's own colour under an overlay.
        let t = Theme::default();
        assert_eq!((t.selection_fg, t.search_match_fg), (Color::None, Color::None));
    }

    #[test]
    fn custom_theme_keeps_palette_colors_and_repairs_converted_rgb() {
        let cfg = crate::config::parse(r##"
            [theme]
            background = "#010101"
            text_dim = "black"
            search_match = "bright_cyan"
            cursor_line = "#040404"
            indent_colors = [0, 16, "#040404"]
            text = "#c0caf5"
        "##).unwrap().theme;
        let authored = Theme::authored(&cfg).unwrap();
        let mut truecolor = authored.clone();
        truecolor.adapt(ColorDepth::TrueColor);
        assert!(truecolor == authored);
        // `text` is set to its default, as the shipped theme.conf does: it is
        // still the built-in theme's.
        let set: Vec<_> = authored.explicit.iter().map(String::as_str).collect();
        assert_eq!(set, ["background", "cursor_line", "indent_colors", "search_match", "text_dim"]);

        for depth in [ColorDepth::Basic16, ColorDepth::Indexed256] {
            let mut t = authored.clone();
            t.adapt(depth);
            let basic = depth == ColorDepth::Basic16;
            assert_eq!(t.background, Color::Indexed(if basic { 0 } else { 16 }));
            assert_eq!(t.text_dim, Color::Indexed(0));
            assert_ne!(t.cursor_line, t.background);
            assert_eq!(t.search_match, Color::Indexed(14));
            assert_eq!(&t.indent_colors[..2], &[Color::Indexed(0), t.background]);
            assert_ne!(t.indent_colors[2], t.background);
            // Left to the built-in theme: kept off the page and the cursor
            // line.
            for (name, c) in [("code_bg", t.code_bg), ("selection", t.selection), ("text", t.text)] {
                assert_ne!(c, t.background, "{depth:?} {name}");
            }
            assert_ne!(t.selection, t.search_match);
            assert_ne!(t.selection_fg, t.selection);
        }
    }

    #[test]
    fn basic16_adapts_extended_palette_indices() {
        let mut t = Theme { selection: Color::Indexed(16), ..Default::default() };
        t.adapt(ColorDepth::Basic16);
        assert_eq!(t.selection, Color::Indexed(15));
        assert_eq!(t.cursor_line, Color::None);
    }

    #[test]
    fn adapt_downgrades_rgb() {
        let mut t = Theme {
            background: Color::Indexed(15),
            text: Color::Rgb(0, 0, 0),
            ..Default::default()
        };
        t.adapt(ColorDepth::Basic16);
        assert_eq!(t.text, Color::Indexed(0)); // black
        let mut t = Theme { link: Color::Rgb(0, 0, 255), ..Default::default() };
        t.adapt(ColorDepth::Basic16);
        assert_eq!(t.link, Color::Indexed(12)); // bright blue (4 | 8)
    }
}
