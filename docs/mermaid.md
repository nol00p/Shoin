# Mermaid previews

Shoin can preview closed `mermaid` fences as text diagrams in the terminal.
Text previews support flowcharts (`flowchart` and `graph`) and sequence
diagrams (`sequenceDiagram`). Mermaid syntax support depends on your Merman CLI
version.

## Enable previews

Install Merman separately and make its `merman-cli`
executable available on your PATH. Use a release with text output support.
[Merman releases](https://github.com/Latias94/merman/releases)

Add this setting to the existing `[markdown]` section of your trusted Shoin
config, usually in `~/.config/shoin/input.conf`.

```toml
[markdown]
mermaid_renderer = "merman-cli"
```

You can also use the absolute path to the executable. The value is a single
executable name or path. Shoin passes the fence body to that local executable.
Omit the key or set it to `""` to turn previews off, which is the default.
Shoin accepts this setting from user config or a path selected explicitly with
`--config` or `SHOIN_CONFIG`. A config discovered automatically in the working
directory cannot enable an executable.

## Try a preview

Open [the demo](../examples/mermaid/README.md) in read-only mode from the Shoin
checkout. The explicit config path selects your user config even when the
checkout has its own `shoin/` directory.

```sh
shoin --config "${XDG_CONFIG_HOME:-$HOME/.config}/shoin" -R examples/mermaid/README.md
```

In the editor, moving the cursor into a fence reveals its entire source.
With concealment off, or when a diagram cannot render or fit, Shoin shows the
fenced source. If Shoin cannot find the configured executable, it shows an
installation hint at the fence. Styled `--render` output waits briefly for
previews. Redirected plain output uses the Mermaid source.


## Show diagrams as images

Set `mermaid_graphics = true` in the same `[markdown]` section to prefer PNG
on terminals with Kitty or iTerm2 graphics support. Shoin uses text while the
image is being generated, when graphics are unavailable, or when the diagram
cannot fit as an image. The fenced source remains available if neither preview
works. Styled output sent to a pager uses text previews where they fit.

Mermaid previews also work inside expanded note embeds. Use `:embed full` to
enable expansion for the current session. Open the linked note to edit its
Mermaid source. Graphics support depends on your Merman version
and includes flowcharts, sequence, class, state, ER, Gantt, and pie diagrams.

## Export diagrams

With `mermaid_renderer` configured, Shoin renders diagrams in HTML and PDF
exports. HTML embeds self-contained PNGs and works offline in
a browser. PDF uses the configured Merman CLI to create PNGs and passes them
to Pandoc. PDF still requires a Pandoc layout engine such as Typst.

A diagram that cannot render stays as source and adds an export diagnostic.
Markdown and plain text exports always keep Mermaid source. Exported diagrams
work independently of terminal graphics support.
