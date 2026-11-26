# Mermaid previews

Shoin can preview closed `mermaid` fences as text diagrams in the terminal.
Flowcharts (`flowchart` and `graph`) and sequence diagrams
(`sequenceDiagram`) are the main preview examples. Class, state, ER, Gantt,
and pie diagrams were [smoke-tested](../examples/mermaid/families.md) in text
and PNG with Merman CLI 0.11.0.
Shoin passes other families through to Merman. Their output depends on the
installed Merman version.

## Enable previews

Download the prebuilt Merman CLI for your platform, unpack it, and put
`merman-cli` on your PATH. Shoin was tested with Merman CLI 0.11.0.
Merman CLI 0.9.4 and later accept the renderer flags Shoin uses. For example,
0.10.0 can render pie charts as PNGs, while pie text previews need 0.11.0.
[Merman CLI 0.11.0 downloads](https://github.com/Latias94/merman/releases/tag/v0.11.0#download-merman-cli-0110)

Add this setting to the existing `[markdown]` section of your trusted Shoin
config, usually in `~/.config/shoin/input.conf`.

```toml
[markdown]
mermaid_renderer = "merman-cli"
```

Use a bare name from PATH, an absolute executable path, or a `~/` path
under your home directory. Shoin warns about relative paths and leaves the
renderer off. It passes the fence body to the configured local executable.
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

At most 32 diagrams render at once, and split panes share that limit: each
pane gets an equal part of it (16 each with two panes), counted from the top
of the pane. Fences past a pane's part show their source until scrolling
brings them nearer its top. In read-only mode, a search that lands inside a
diagram shows its source with the match highlighted. Scrolling away, or Esc,
shows the diagram again.


## Show diagrams as images

Set `mermaid_graphics = true` in the same `[markdown]` section to prefer PNG
on terminals with Kitty or iTerm2 graphics support. Shoin uses text while the
image is being generated, when graphics are unavailable, or when the diagram
cannot fit as an image. The fenced source remains available if neither preview
works. Styled output sent to a pager uses text previews where they fit.
PNG diagrams have a white background, including on dark themes.

Mermaid previews also work inside expanded note embeds. Use `:embed full` to
enable expansion for the current session. Open the linked note to edit its
Mermaid source.

## Export diagrams

With `mermaid_renderer` configured, Shoin renders diagrams in HTML and PDF
exports. HTML embeds self-contained PNGs and works offline in
a browser. PDF uses the configured Merman CLI to create PNGs and passes them
to Pandoc. PDF still requires a Pandoc layout engine such as Typst.
You can keep editing while an export runs.

Failed diagrams stay as source. The export diagnostic gives the count and
the first affected line in the flattened Markdown. Markdown and plain text
exports always keep Mermaid source. Exported diagrams work independently of
terminal graphics support.

## Details

- Shoin checks PNG chunk types, CRCs, and size limits. The configured renderer
  can still produce corrupt compressed pixels with matching CRCs. Browsers,
  terminals, and PDF engines decode the pixels.
- Supporting SVG would require validating the XML, CSS, and resource
  references before embedding renderer output.
- The Phase 0 comparison with mermaid-ascii and mmdc remains open.
  The [stress fixtures](../examples/mermaid/stress.md) cover a cyclic branch,
  a long Unicode label, and nested sequence frames.
