# Art

The start screen's built-in bonsai lives in `src/render/splash.rs`. It is
**attribution-free** — confirmed by its author, 2026-08-19 — which is why it
ships as the default.

`bonsai-alternate.txt` is a second, original bonsai composed for this project in
the same ASCII idiom. It is not used; it is kept because it exists and cost
something to make, and because it is a ready drop-in if the default is ever
replaced. It shares no row with the one that ships, and the longest run of
characters common to the two is 22 spaces.

## Using your own

You do not have to edit the source. Point `[splash] art` at any text file:

```toml
[splash]
art = "art.txt"     # relative paths resolve against your config directory
```

`"none"` leaves just the hints, and a file that cannot be read falls back to the
bonsai rather than to an empty screen.
