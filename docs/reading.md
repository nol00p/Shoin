# Reading

Shoin is also a reader. It can print a note to the terminal, page it, read
one piped in from another command, or open it so it can't be changed. Each
shows the same page the editor does, with every line concealed.

<p align="center">
  <img src="media/reading.gif" alt="A note printed into the shell with shoin -p, a piped README paged through less, and a note opened read-only with shoin -R" width="800">
</p>

## Print a note

```sh
shoin -p notes.md
```

`-p` (long form `--render`) prints the page the editor would draw, laid out
for the terminal's width, and exits. Every line is concealed, so you read
the finished text, not the markers. Every `![[embed]]` is expanded in full,
pictures are drawn where the terminal can show them, and front matter is
left out.

A long note scrolls past. Add `--pager` to read it a screen at a time:

```sh
shoin -p notes.md --pager
```

A page taller than the terminal goes to `$PAGER`, or `less -R` when that's
unset. A page that fits is printed as usual. A pager can't show pictures, so
in a paged page each picture is drawn as an empty outline with its caption.

When the output isn't a terminal, or `NO_COLOR` is set, `-p` prints plain
text instead. That text is the same as
`shoin --export notes.md --format txt --stdout`:

```sh
shoin -p notes.md | grep keeper
```

## From a pipe

Text piped into Shoin is read like a file. With `-p` it is printed; without
it, it opens in the editor:

```sh
cat notes.md | shoin -p
curl -s https://example.com/README.md | shoin -p
git show HEAD:notes.md | shoin
shoin - < notes.md
```

`-` names stdin, as it does for most tools. Piped text opens as a buffer
with no name. `:w name.md` saves it under that name, and `:q` leaves without
asking, since the text still exists wherever it came from. Once you change
it, `:q` asks like it does for any file.

The editor takes its keys from the terminal, not the pipe. Links and embeds
resolve from the working directory, as on a blank page.

## Read-only

```sh
shoin -R notes.md
```

`-R` opens a page to read, not to edit. Every line is concealed, the
cursor's too, and there's no caret. The keys scroll the page, as in `less`:

| Key | Does |
|---|---|
| `j` `k` | scroll a line down / up |
| `Ctrl-d` `Ctrl-u` | half a page down / up |
| `gg` `G` | top / bottom |
| `/` `?` `n` `N` | search |
| `q` | quit |
| `i` | switch to editing |

Yanking, panels, `gf` and `:w` still work. Anything that would change the
text is refused with "read-only — press i to edit".

Opening read-only shows "read-only — q to quit, i to edit", so an `i`
straight away starts editing. After any other key, the first `i` is refused
with the message, and the next `i` edits. `:view` turns read-only on and off
from inside the editor.

## Details

- A bare `shoin -p` at a prompt, with nothing piped in, is an error rather
  than a wait for typed input. `shoin -p -` reads what you type, as `cat -`
  does.
- `$PAGER` is run through `sh`, so it can carry flags. `LESS=R` is set when
  `LESS` isn't, so colour reaches `less`.
- Text that isn't valid UTF-8, piped or from a file, is refused with an
  error, never converted.
- `-R` applies to every buffer opened after it too, by `:e`, `gf` or the
  file tree, until `:view` or `i` turns it off.
- In read-only mode `q` acts like `:q`: it closes the page, and refuses if
  there are unsaved changes. Once you've switched to editing, `q` no longer
  quits; use `:q`, or `:q!` to discard changes.
- Inside tmux no pictures are drawn, in `-p` or the editor. The boxes and
  captions stand in for them, as in a terminal that can't draw pixels.
- Pictures follow the editor's detection, and
  `SHOIN_IMAGE_PROTOCOL=kitty|iterm|none` overrides it. See
  [Notes](notes.md#pictures).
- The flags are in the [command-line reference](reference/cli.md), and the
  keys in the [key reference](reference/keys.md#read-only).
