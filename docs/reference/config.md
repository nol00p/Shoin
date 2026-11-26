# Configuration defaults

Use these settings in the project's `./shoin.conf` or split them across
`*.conf` files in a config directory. Shoin applies these defaults to unset
settings. If you set `glyphs.nerd_fonts = false`, unset glyphs use their ASCII
fallbacks in place of the listed Nerd Font defaults. Explicitly set glyphs
keep their configured values. See [Configuration](../configuration.md) for
where the editor looks for config and [Keys](keys.md) for the default bindings.

```toml
[layout]
measure           = 72
align             = "center"
text_align        = "left"
padding_top       = 2
padding_bottom    = 2
hanging_indent    = true
line_spacing      = 0
scroll_hint       = true
typewriter        = false
typewriter_anchor = 0.5
focus             = "off"
numbers           = "off"
conceal           = true
stable_gutter     = true

[status]
enabled   = true
flash_ms  = 1500
show      = ["file", "modified", "position", "words"]

[hints]
enabled   = true

[splash]
art = ""

[editor]
tab_width         = 4
expand_tab        = true
auto_indent       = true
auto_pair         = false
trim_on_save      = false
final_newline     = "preserve"
autosave          = false
autosave_interval = 3
autoreload        = true
clipboard         = false
clipboard_background = false
undo_coalesce_ms  = 400
scroll_off        = 3
list_renumber     = true
list_tab          = true
table_align       = "auto"

[cursor]
normal = "block"
insert = "bar"
visual = "block"
blink  = false

[input]
sequence_timeout_ms = 500
leader              = " "
escape_alias        = "jk"
mouse               = true

[markdown]
tags       = true
wiki_links = true
highlight  = true
tables     = true
math       = true
code_syntax = true
mermaid_renderer = ""
mermaid_graphics = false     # prefer PNG on terminals with image support
plain_text_extensions = [".txt"]
export_follows_settings = false

[transclude]
embed             = "none"
root              = "."
max_depth         = 8
heading_offset    = 1
strip_frontmatter = true
border            = true

[tree]
show_hidden = false
close_on_open = false
root = "file"

[glyphs]
nerd_fonts     = true
bullet         = "•"
bullet_alt     = "◦"
task_todo      = "☐"
task_done      = "☑"
quote_bar      = "▎"
indent_guide   = "│"
fence_bar      = "▊"
folder         = ""
folder_open    = ""
modified       = "●"
conflict       = "⚠"
scroll_hint    = "▐"
rule           = "─"

[theme]
background   = "#1a1b26"
text         = "#c0caf5"
text_dim     = "#565f89"
cursor       = "#c0caf5"
cursor_line  = "none"
selection    = "#283457"
search_match = "#e0af68"
heading_1 = "#f7768e"
heading_2 = "#9ece6a"
heading_3 = "#e0af68"
heading_4 = "#7aa2f7"
heading_5 = "#bb9af7"
heading_6 = "#7dcfff"
bold          = "#ff9e64"
italic        = "#c0caf5"
strikethrough = "#565f89"
highlight_bg  = "#3d59a1"
code       = "#9ece6a"
code_bg    = "#24283b"
fence_bar  = "#414868"
syntax_keyword  = "#bb9af7"
syntax_type     = "#7dcfff"
syntax_string   = "#9ece6a"
syntax_literal  = "#ff9e64"
syntax_comment  = "#565f89"
syntax_function = "#7aa2f7"
syntax_punct    = "#9aa5ce"
diff_add        = "#9ece6a"
diff_remove     = "#f7768e"
diff_change     = "#e0af68"
link       = "#7aa2f7"
wiki_link  = "#bb9af7"
tag        = "#7dcfff"
tag_bg     = "#283457"
quote        = "#9aa5ce"
quote_bar    = "#414868"
list_bullet  = "#7aa2f7"
task_done    = "#565f89"
rule         = "#414868"
table_border = "#414868"
indent_colors = ["#414868", "#565f89", "#7aa2f7", "#bb9af7"]
status_fg = "#565f89"
status_bg = "#16161e"
error     = "#f7768e"

[theme.styles]
heading_bold   = true
heading_italic = false
bold_attr      = true
italic_attr    = true
link_underline = true
dim_markers    = true
```

## Details

- Leave `theme.name` unset for the built-in Tokyo Night theme. Set it to
  load a theme from `$XDG_CONFIG_HOME/shoin/themes/<name>.toml`. If
  `XDG_CONFIG_HOME` is unset, Shoin uses `~/.config/shoin/themes/<name>.toml`.
- On 16- and 256-color terminals, Shoin maps theme colors to the terminal
  palette. Indexed fallbacks never change a palette colour you set; an RGB
  colour you set is still kept visible when Shoin converts it to the
  palette. The selection and search matches also set their own text
  color, as Neovim's defaults do: black on white for the selection and
  black on bright yellow for search matches when the theme's colors need
  replacing. Text on a `==highlight==` in the highlight's own slot, such
  as a heading, is drawn in black or white. Truecolor terminals use the
  configured colors.
- The [shipped configuration files](../../shoin/) explain each setting
  alongside its default.
