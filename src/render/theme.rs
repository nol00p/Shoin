//! Theme and color resolution. SPEC.md §8.1.
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

    pub headings: [Color; 6],

    pub bold: Color,
    pub italic: Color,
    pub strikethrough: Color,
    pub highlight_bg: Color,

    pub code: Color,
    pub code_bg: Color,
    pub fence_bar: Color,

    /// Syntax highlighting inside a fenced block (SPEC.md §5.3). Code the
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
            status_bg: Color::None,
            error: rgb(0xf7, 0x76, 0x8e),

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
    /// Resolve a `[theme]` config section into a full theme: start from a base
    /// (a named theme file, or the built-in default), overlay the config's
    /// colors and style flags, then downgrade for the terminal. SPEC.md §8.1.
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
        for (key, value) in colors {
            if key == "indent_colors" {
                if let Some(arr) = value.as_array() {
                    let mut out = Vec::new();
                    for v in arr {
                        out.push(value_to_color(v)?);
                    }
                    if !out.is_empty() {
                        self.indent_colors = out;
                    }
                }
                continue;
            }
            let color = value_to_color(value)?;
            self.set_color(key, color);
        }
        Ok(())
    }

    /// Assign a color by its `[theme]` key name. Unknown keys are ignored, for
    /// forward compatibility (SPEC.md §8).
    fn set_color(&mut self, key: &str, c: Color) {
        match key {
            "background" => self.background = c,
            "text" => self.text = c,
            "text_dim" => self.text_dim = c,
            "cursor" => self.cursor = c,
            "cursor_line" => self.cursor_line = c,
            "selection" => self.selection = c,
            "search_match" => self.search_match = c,
            "heading_1" => self.headings[0] = c,
            "heading_2" => self.headings[1] = c,
            "heading_3" => self.headings[2] = c,
            "heading_4" => self.headings[3] = c,
            "heading_5" => self.headings[4] = c,
            "heading_6" => self.headings[5] = c,
            "bold" => self.bold = c,
            "italic" => self.italic = c,
            "strikethrough" => self.strikethrough = c,
            "highlight_bg" => self.highlight_bg = c,
            "code" => self.code = c,
            "code_bg" => self.code_bg = c,
            "fence_bar" => self.fence_bar = c,
            "syntax_keyword" => self.syntax_keyword = c,
            "syntax_type" => self.syntax_type = c,
            "syntax_string" => self.syntax_string = c,
            "syntax_literal" => self.syntax_literal = c,
            "syntax_comment" => self.syntax_comment = c,
            "syntax_function" => self.syntax_function = c,
            "syntax_punct" => self.syntax_punct = c,
            "link" => self.link = c,
            "wiki_link" => self.wiki_link = c,
            "tag" => self.tag = c,
            "tag_bg" => self.tag_bg = c,
            "quote" => self.quote = c,
            "quote_bar" => self.quote_bar = c,
            "list_bullet" => self.list_bullet = c,
            "task_done" => self.task_done = c,
            "rule" => self.rule = c,
            "table_border" => self.table_border = c,
            "status_fg" => self.status_fg = c,
            "status_bg" => self.status_bg = c,
            "error" => self.error = c,
            _ => {}
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
        macro_rules! down {
            ($($f:ident),*) => { $( self.$f = self.$f.downgrade(depth); )* };
        }
        down!(
            background, text, text_dim, cursor, cursor_line, selection, search_match, bold,
            italic, strikethrough, highlight_bg, code, code_bg, fence_bar, syntax_keyword,
            syntax_type, syntax_string, syntax_literal, syntax_comment, syntax_function,
            syntax_punct, link, wiki_link, tag, tag_bg, quote, quote_bar, list_bullet, task_done,
            rule, table_border, status_fg, status_bg, error
        );
        for h in self.headings.iter_mut() {
            *h = h.downgrade(depth);
        }
        for c in self.indent_colors.iter_mut() {
            *c = c.downgrade(depth);
        }
    }
}

impl Color {
    fn downgrade(self, depth: ColorDepth) -> Color {
        match (self, depth) {
            (Color::Rgb(r, g, b), ColorDepth::Indexed256) => Color::Indexed(rgb_to_256(r, g, b)),
            (Color::Rgb(r, g, b), ColorDepth::Basic16) => Color::Indexed(rgb_to_16(r, g, b)),
            _ => self,
        }
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
fn rgb_to_16(r: u8, g: u8, b: u8) -> u8 {
    let mut idx = 0u8;
    if r > 100 {
        idx |= 1;
    }
    if g > 100 {
        idx |= 2;
    }
    if b > 100 {
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
            one.set_color(key, value_to_color(value).unwrap());
            assert!(
                one == Theme::default(),
                "theme.conf `{key}` = {value} differs from Theme::default()"
            );
        }

        // Then wholesale, which is what catches a role the file OMITS — the
        // per-key loop can only check the keys that are there.
        assert!(
            Theme::from_config(&cfg).unwrap() == Theme::default(),
            "shoin/theme.conf and Theme::default() disagree — change both, or neither"
        );
    }

    #[test]
    fn from_config_overlays_colors_and_styles() {
        let toml = r##"
            text = "#010203"
            heading_1 = "#0a0b0c"
            indent_colors = ["#111111", "#222222"]
            [styles]
            heading_bold = false
        "##;
        let cfg: ThemeConfig = toml::from_str(toml).unwrap();
        let theme = Theme::from_config(&cfg).unwrap();
        // Overlaid over the default, in a truecolor test run.
        if detect_depth() == ColorDepth::TrueColor {
            assert_eq!(theme.text, Color::Rgb(1, 2, 3));
            assert_eq!(theme.headings[0], Color::Rgb(0x0a, 0x0b, 0x0c));
            assert_eq!(theme.indent_colors.len(), 2);
        }
        assert!(!theme.heading_bold);
        // A key we didn't set keeps the default.
        assert_eq!(theme.background, Theme::default().background);
    }

    #[test]
    fn adapt_downgrades_rgb() {
        let mut t = Theme { text: Color::Rgb(0, 0, 0), ..Default::default() };
        t.adapt(ColorDepth::Basic16);
        assert_eq!(t.text, Color::Indexed(0)); // black
        let mut t = Theme { link: Color::Rgb(0, 0, 255), ..Default::default() };
        t.adapt(ColorDepth::Basic16);
        assert_eq!(t.link, Color::Indexed(12)); // bright blue (4 | 8)
    }
}
