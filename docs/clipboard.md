# Clipboard

`"+y` copies to the system clipboard and `"+p` pastes from it (`"*` is the
same clipboard). Your terminal's own paste goes in exactly as it was copied.

## Using the system clipboard

To make plain `y`, `d` and `p` use the clipboard too, set `clipboard = true` in
`editor.conf`, or `:set clipboard` for the session. A named register such as
`"ayy` never touches it.

Shoin uses `pbcopy`/`pbpaste` on macOS and `wl-copy` or `xclip`/`xsel` on
Linux.

## Over SSH

Over SSH, or with none of those programs installed, a copy is sent to the
terminal as an OSC 52 escape, so it reaches the clipboard of the machine you
are sitting at. Pasting there falls back to Shoin's own last copy. To paste
text copied elsewhere, use your terminal's own paste: a terminal never lets
programs read your clipboard.

OSC 52 needs a terminal that supports it. kitty, WezTerm, Alacritty, foot,
Ghostty, iTerm2 and Windows Terminal do; GNOME Terminal and Konsole do not.
Inside tmux, add `set -g set-clipboard on` to `.tmux.conf`, or tmux swallows
the escape.

## Pasting from the terminal

Your terminal's paste (Cmd+V, Ctrl+Shift+V) arrives as one piece of text and
goes in exactly as copied, in Normal mode as much as Insert: no auto-indent, no
list continuation, and a `jk` in the text stays text. It is one undo step, and
`.` repeats it.

## Details

- If yanks and deletes feel slow with `clipboard` on,
  `clipboard_background = true` runs copies on a background thread. A burst of
  copies runs the clipboard program once, for the newest text.
- A clipboard program that hangs isn't tried again for the rest of the session.
- A failed paste after an OSC 52 copy falls back to Shoin's own copy.
