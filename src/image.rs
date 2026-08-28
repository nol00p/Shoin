//! Images: recognising them, measuring them, and — where the terminal allows
//! it — drawing the actual pixels.
//!
//! An image reaches the editor the same way a note does, through `![[…]]`
//! (SPEC.md §14.2). What changes is what the embed expands INTO: not the
//! target's text, but a block of reserved rows the size the picture should
//! occupy. Everything in this module is pure — a path in, numbers and bytes
//! out — so the whole feature is testable without a terminal that can show a
//! picture, which is the half no test could ever check.
//!
//! Three deliberate limits:
//!
//!   * Dimensions are read from the file HEADER, never by decoding the image.
//!     A few dozen bytes answer the only question rendering asks (what shape
//!     is it), and a decoder would be a dependency plus a decompression bomb
//!     in the render path.
//!   * The bytes are sent to the terminal verbatim, in the file's own format.
//!     Both protocols below take PNG/JPEG/GIF as they are.
//!   * A file that is not one of the formats here is not an image, even if it
//!     ends in `.png`. The header is the authority.

use std::path::Path;

/// Extensions that get looked at as images. The header decides whether one
/// really is; this list only decides what is worth opening.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// True when a path names something worth probing as an image.
pub fn looks_like_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

/// What a header says: the pixel size, and whether the bytes can go to a
/// terminal as they are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    /// PNG, JPEG and GIF are what both graphics protocols accept verbatim.
    /// A WEBP or BMP still measures — it just cannot be sent as pixels.
    pub sendable: bool,
}

/// Read the pixel dimensions out of an image header.
///
/// Returns `None` for anything unrecognised, which the caller shows as a
/// placeholder rather than an error: a file that is not an image is a broken
/// link, and `link.rs` has already said so by then.
pub fn probe(bytes: &[u8]) -> Option<Info> {
    png(bytes).or_else(|| gif(bytes)).or_else(|| jpeg(bytes)).or_else(|| bmp(bytes)).or_else(|| webp(bytes))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// PNG: an 8-byte signature, then an IHDR chunk whose first two fields are the
/// dimensions. Fixed offsets, so there is nothing to walk.
fn png(b: &[u8]) -> Option<Info> {
    const SIG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if b.len() < 24 || !b.starts_with(SIG) || &b[12..16] != b"IHDR" {
        return None;
    }
    Some(Info { width: be32(&b[16..20]), height: be32(&b[20..24]), sendable: true })
}

/// GIF: `GIF87a`/`GIF89a` then two little-endian u16s.
fn gif(b: &[u8]) -> Option<Info> {
    if b.len() < 10 || (!b.starts_with(b"GIF87a") && !b.starts_with(b"GIF89a")) {
        return None;
    }
    let w = u16::from_le_bytes([b[6], b[7]]) as u32;
    let h = u16::from_le_bytes([b[8], b[9]]) as u32;
    Some(Info { width: w, height: h, sendable: true })
}

/// JPEG: a marker walk. Only the SOF markers carry the size, and they are not
/// at a fixed offset, so the segment lengths have to be followed.
///
/// SOF4 (`C4`), SOF8 (`C8`) and SOF12 (`CC`) are excluded ON PURPOSE — they are
/// Huffman/arithmetic tables and extensions that happen to sit in the same
/// numeric range, not frame headers.
fn jpeg(b: &[u8]) -> Option<Info> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        // Standalone markers carry no length to skip.
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
            return Some(Info { width: w, height: h, sendable: true });
        }
        i += 2 + len.max(2);
    }
    None
}

/// BMP: a `BM` signature, then a 40-byte info header with signed dimensions —
/// the height is negative for a top-down bitmap, hence the `unsigned_abs`.
fn bmp(b: &[u8]) -> Option<Info> {
    if b.len() < 26 || !b.starts_with(b"BM") {
        return None;
    }
    let w = le32(&b[18..22]) as i32;
    let h = le32(&b[22..26]) as i32;
    Some(Info { width: w.unsigned_abs(), height: h.unsigned_abs(), sendable: false })
}

/// WEBP: `RIFF….WEBP`, then one of three chunk layouts. Only the simple
/// lossy (`VP8 `) and lossless (`VP8L`) forms are read; an extended `VP8X`
/// file reports its canvas size, which is the one the header states outright.
fn webp(b: &[u8]) -> Option<Info> {
    if b.len() < 30 || !b.starts_with(b"RIFF") || &b[8..12] != b"WEBP" {
        return None;
    }
    let (w, h) = match &b[12..16] {
        b"VP8X" => {
            let w = 1 + (b[24] as u32 | (b[25] as u32) << 8 | (b[26] as u32) << 16);
            let h = 1 + (b[27] as u32 | (b[28] as u32) << 8 | (b[29] as u32) << 16);
            (w, h)
        }
        b"VP8L" => {
            let bits = le32(&b[21..25]);
            (1 + (bits & 0x3FFF), 1 + ((bits >> 14) & 0x3FFF))
        }
        b"VP8 " => {
            let w = u16::from_le_bytes([b[26], b[27]]) as u32 & 0x3FFF;
            let h = u16::from_le_bytes([b[28], b[29]]) as u32 & 0x3FFF;
            (w, h)
        }
        _ => return None,
    };
    Some(Info { width: w, height: h, sendable: false })
}

// ---------------------------------------------------------------- terminals

/// How this terminal can be asked to draw pixels, if at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// Kitty's graphics protocol — kitty, Ghostty, WezTerm, Konsole.
    Kitty,
    /// iTerm2's inline-image escape — iTerm2, WezTerm, VSCode's terminal.
    Iterm2,
    /// Nothing. The placeholder is all there is, and it is drawn either way.
    None,
}

/// Read the protocol out of the environment.
///
/// Env vars rather than a capability QUERY on purpose: a query writes to the
/// terminal and waits for an answer, and a terminal that does not recognise it
/// leaves the reply in the input stream to be typed into the buffer. That is a
/// bad trade for a feature whose fallback is a perfectly good placeholder.
pub fn detect() -> Protocol {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    let term = var("TERM");
    let program = var("TERM_PROGRAM");

    // An explicit override wins: the env is a guess, and a reader who knows
    // better should be able to say so.
    match var("SHOIN_IMAGE_PROTOCOL").to_ascii_lowercase().as_str() {
        "kitty" => return Protocol::Kitty,
        "iterm" | "iterm2" => return Protocol::Iterm2,
        "none" | "off" => return Protocol::None,
        _ => {}
    }
    // Multiplexers pass none of this through reliably, and a half-drawn image
    // inside a pane is worse than a placeholder.
    if !var("TMUX").is_empty() || term.starts_with("screen") {
        return Protocol::None;
    }
    if !var("KITTY_WINDOW_ID").is_empty() || term.contains("kitty") || term.contains("ghostty") {
        return Protocol::Kitty;
    }
    if program == "WezTerm" || program == "iTerm.app" || program == "vscode" {
        return Protocol::Iterm2;
    }
    Protocol::None
}

// ------------------------------------------------------------------ sizing

/// How many cells a picture should occupy, fitted into `max_cols` × `max_rows`
/// with its aspect ratio kept.
///
/// A terminal cell is roughly twice as tall as it is wide, so the height in
/// ROWS is half what the pixel ratio would suggest. `CELL_ASPECT` is that
/// factor, and it is the one number here that is a guess — terminals do not
/// agree, and none of them will say.
pub const CELL_ASPECT: f32 = 2.1;

pub fn fit(info: Info, max_cols: u16, max_rows: u16) -> (u16, u16) {
    if info.width == 0 || info.height == 0 || max_cols == 0 || max_rows == 0 {
        return (0, 0);
    }
    let ratio = info.height as f32 / info.width as f32;
    let cols = max_cols;
    let rows = ((cols as f32 * ratio) / CELL_ASPECT).round().max(1.0) as u16;
    if rows <= max_rows {
        return (cols, rows);
    }
    // Too tall for the space: pin the height instead and take the width that
    // keeps the shape.
    let rows = max_rows;
    let cols = ((rows as f32 * CELL_ASPECT) / ratio).round().max(1.0) as u16;
    (cols.min(max_cols), rows)
}

// ------------------------------------------------------------------ base64

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding. Both protocols want the file's bytes this
/// way, and a dependency for twenty lines would be a poor trade.
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

// --------------------------------------------------------------- sequences

/// The escape sequence that draws `bytes` in a `cols` × `rows` box at the
/// cursor's current position. Empty when the terminal cannot draw pixels.
///
/// Neither sequence moves the cursor afterwards in a way worth relying on, so
/// the caller positions the cursor before every image and again after.
pub fn draw(protocol: Protocol, bytes: &[u8], cols: u16, rows: u16) -> String {
    if cols == 0 || rows == 0 {
        return String::new();
    }
    let data = base64(bytes);
    match protocol {
        Protocol::None => String::new(),
        // OSC 1337. `inline=1` says draw it here rather than downloading it;
        // the cell sizes carry the units, which is why they are not bare
        // numbers.
        Protocol::Iterm2 => format!(
            "\x1b]1337;File=inline=1;width={cols};height={rows};preserveAspectRatio=1:{data}\x07"
        ),
        // APC _G. `a=T` transmits AND displays, `f=100` says the payload is a
        // whole image file rather than raw pixels, `c`/`r` are the cell box,
        // and `C=1` tells kitty NOT to move the cursor afterwards — without it
        // the caret ends up under the picture and the next frame draws from
        // there. The payload is chunked at 4096 base64 bytes, with `m=1` on
        // every chunk but the last — kitty's own limit, not ours.
        Protocol::Kitty => {
            let mut out = String::new();
            let chunks: Vec<&str> = data.as_bytes().chunks(4096).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
            let last = chunks.len().saturating_sub(1);
            for (i, chunk) in chunks.iter().enumerate() {
                let more = u8::from(i != last);
                if i == 0 {
                    out.push_str(&format!("\x1b_Ga=T,f=100,C=1,c={cols},r={rows},m={more};{chunk}\x1b\\"));
                } else {
                    out.push_str(&format!("\x1b_Gm={more};{chunk}\x1b\\"));
                }
            }
            out
        }
    }
}

/// Erase every image this session has drawn. Kitty images are objects that
/// outlive the cells they were drawn over, so a scroll would otherwise leave
/// them hanging; iTerm2 images are just cell contents, which the next frame
/// overwrites by itself.
pub fn clear(protocol: Protocol) -> String {
    match protocol {
        Protocol::Kitty => "\x1b_Ga=d,d=A\x1b\\".to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real 1×1 PNG, byte for byte — the smallest honest fixture there is.
    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89,
    ];

    #[test]
    fn png_dimensions_come_from_the_header() {
        assert_eq!(probe(PNG_1X1), Some(Info { width: 1, height: 1, sendable: true }));

        // A wider one, built by patching IHDR — the point is the offsets.
        let mut wide = PNG_1X1.to_vec();
        wide[16..20].copy_from_slice(&800u32.to_be_bytes());
        wide[20..24].copy_from_slice(&600u32.to_be_bytes());
        assert_eq!(probe(&wide), Some(Info { width: 800, height: 600, sendable: true }));
    }

    #[test]
    fn the_other_formats_measure_too() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x20, 0x00, 0x10, 0x00]); // 32 x 16
        assert_eq!(probe(&gif).map(|i| (i.width, i.height)), Some((32, 16)));

        // JPEG: SOI, an APP0 segment to walk PAST, then SOF0 with the size.
        let jpg = [
            0xFF, 0xD8, // SOI
            0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, // APP0, length 4
            0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0x2C, 0x02, 0x58, // SOF0: 300 x 600
            0x00, 0x00,
        ];
        assert_eq!(probe(&jpg).map(|i| (i.width, i.height)), Some((600, 300)));

        // A BMP measures but must not be sent as pixels.
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&[0u8; 16]);
        bmp.extend_from_slice(&64u32.to_le_bytes());
        bmp.extend_from_slice(&(-32i32).to_le_bytes()); // top-down
        let info = probe(&bmp).unwrap();
        assert_eq!((info.width, info.height), (64, 32));
        assert!(!info.sendable, "no protocol takes a BMP verbatim");
    }

    #[test]
    fn garbage_is_not_an_image() {
        assert_eq!(probe(b""), None);
        assert_eq!(probe(b"# a markdown file\n"), None);
        assert_eq!(probe(&[0xFF, 0xD8]), None, "a JPEG that stops before its header");
    }

    /// A truncated file must not panic — the header walk is the one place
    /// where a hostile file meets fixed offsets.
    #[test]
    fn every_truncation_of_a_header_is_survivable() {
        for n in 0..PNG_1X1.len() {
            let _ = probe(&PNG_1X1[..n]);
        }
        let jpg = [0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0x2C, 0x02, 0x58];
        for n in 0..jpg.len() {
            let _ = probe(&jpg[..n]);
        }
    }

    #[test]
    fn fitting_keeps_the_shape_and_the_bounds() {
        let wide = Info { width: 800, height: 400, sendable: true };
        let (cols, rows) = fit(wide, 60, 40);
        assert_eq!(cols, 60, "a picture takes the width it is given");
        assert_eq!(rows, 14, "…and half the pixel ratio, because cells are tall");

        // Tall enough to hit the row ceiling: the WIDTH gives way instead.
        let tall = Info { width: 100, height: 1000, sendable: true };
        let (cols, rows) = fit(tall, 60, 20);
        assert_eq!(rows, 20);
        assert!(cols < 60, "narrowed to keep the shape, got {cols}");

        assert_eq!(fit(Info { width: 0, height: 0, sendable: true }, 60, 20), (0, 0));
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xFF, 0xFE, 0xFD]), "//79", "the top of the alphabet");
    }

    #[test]
    fn the_sequences_say_what_the_protocols_expect() {
        let iterm = draw(Protocol::Iterm2, b"foo", 10, 5);
        assert!(iterm.starts_with("\x1b]1337;File=inline=1;"));
        assert!(iterm.contains("width=10;height=5"));
        assert!(iterm.ends_with('\x07'));
        assert!(iterm.contains("Zm9v"), "the payload is base64");

        let kitty = draw(Protocol::Kitty, b"foo", 10, 5);
        assert!(kitty.starts_with("\x1b_Ga=T,f=100,C=1,c=10,r=5,m=0;"));
        assert!(kitty.contains("C=1"), "the cursor must stay where the editor put it");
        assert!(kitty.ends_with("\x1b\\"));

        assert_eq!(draw(Protocol::None, b"foo", 10, 5), "", "no protocol, no bytes");
        assert_eq!(draw(Protocol::Kitty, b"foo", 0, 5), "", "and nothing for an empty box");
    }

    /// Kitty caps a chunk at 4096 base64 bytes, and every chunk but the last
    /// has to say more is coming.
    #[test]
    fn a_big_image_is_chunked_for_kitty() {
        let big = vec![0u8; 9000]; // 12000 base64 bytes → three chunks
        let seq = draw(Protocol::Kitty, &big, 10, 5);
        assert_eq!(seq.matches("\x1b_G").count(), 3, "three chunks");
        assert_eq!(seq.matches("m=1").count(), 2, "the first two say 'more'");
        assert!(seq.ends_with("m=0;\x1b\\") || seq.contains("m=0;"), "the last says 'done'");
    }

    #[test]
    fn only_image_extensions_are_worth_opening() {
        assert!(looks_like_image(Path::new("a/b/photo.PNG")), "case does not matter");
        assert!(looks_like_image(Path::new("x.jpeg")));
        assert!(!looks_like_image(Path::new("notes.md")));
        assert!(!looks_like_image(Path::new("noext")));
    }
}
