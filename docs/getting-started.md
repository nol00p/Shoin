# Getting started

<p align="center">
  <img src="media/hero.gif" alt="Shoin's splash screen, the file tree, and a note opened from it" width="800">
</p>

Install Shoin and open a file. No config is needed. `i` to type, `Esc` to
stop, `:w` to save, `:q` to quit.

## Install

Shoin runs on macOS and Linux, on x86_64 or ARM.

**macOS**, with [Homebrew](https://brew.sh):

```sh
brew install nol00p/tap/shoin
```

**macOS or Linux**, with Cargo (Rust 1.88 or newer), from crates.io:

```sh
cargo install shoin
```

Or from a clone:

```sh
git clone https://github.com/nol00p/shoin
cd shoin
cargo install --path .
```

**macOS or Linux**, with the installer, which downloads a prebuilt binary
from the [latest release](https://github.com/nol00p/shoin/releases/latest):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/nol00p/shoin/releases/latest/download/shoin-installer.sh | sh
```

The release page also has the plain `.tar.xz` archives, with a SHA-256
checksum for each, if you'd rather unpack the binary yourself.

Homebrew puts `shoin` on your `PATH` for you. Cargo and the installer put it
in `~/.cargo/bin`, which needs to be on your `PATH`. To try it without
installing, `cargo run -- notes.md` works from a clone.

Shoin needs no config. The leader bindings — `Space fe` for the file tree,
`Space ff` for the finder, and the rest — are built in. To change settings,
write an annotated copy of the defaults:

```sh
shoin --init-config          # → ~/.config/shoin/*.conf
```

It never overwrites a config you already have unless you add `--force`.
([Why it isn't automatic](design.md#the-config-is-one-command-not-automatic).)

## Requirements

| | |
|---|---|
| **OS** | macOS or Linux, x86_64 or ARM — Windows is not supported yet |
| **Rust** | 1.88 or newer, only to build from source |
| **Terminal** | truecolor (24-bit) support |
| **Font** | a [Nerd Font](https://www.nerdfonts.com/) — or set `glyphs.nerd_fonts = false` |
| **PDF export only** | `pandoc` and a layout engine (`brew install pandoc typst`) |

Markdown, text and HTML export need nothing beyond the binary.

## First run

```sh
shoin notes.md      # open a file (created on the first :w if it doesn't exist)
shoin               # a blank page, with a few ways in drawn on it
```

Started with no file, Shoin shows a bonsai and the ways in: the file tree, how
to start writing, how to name the file, `:help`, and how to leave. The picture
is drawn, not typed. The buffer underneath is empty, so the first key you press
is the first character of the file, and the picture is gone that same frame.

Then, in order: `i` to type, `Esc` (or `jk`) to stop, `:w` to save, `:q` to
quit. `:help` explains everything else, and `Space hl` opens one page listing
every key.

Just reading? `shoin -p notes.md` prints a note as the editor shows it, and
`shoin -R notes.md` opens it read-only. See [Reading](reading.md).

## Where next

- [Live preview](live-preview.md), the one idea the editor is built on
- [Editing](editing.md) and [Writing](writing.md) for the keys
- [Notes](notes.md) to link and compose files
- [Reading](reading.md) to print, page or read a note without editing it
