//! Expanding an `![[…]]` line for LIVE PREVIEW. SPEC.md §14.3.
//!
//! One simplification is worth stating up front, because it removes most of the
//! machinery §14.5 anticipated.
//!
//! An embed occupies exactly ONE rope line — the `![[…]]` itself — however many
//! screen rows it draws. And by SPEC §2 the active line always renders raw, so
//! the cursor is on the `![[…]]` source whenever it is anywhere near the embed.
//! The cursor therefore cannot get *inside* expanded content. Three of §14.3's
//! requirements fall out of that for free rather than needing code:
//!
//!   * embedded regions are read-only — there is nothing there to edit;
//!   * motions traverse an embed as a single unit — it *is* a single line;
//!   * "attempting to edit inside one" cannot arise.
//!
//! `Buffer::readonly_ranges` stays empty, and stays where it is: it costs one
//! branch and it is the escape hatch if embeds ever become editable.

use std::path::Path;

use super::link::Link;
use super::Mode;
use crate::config::schema::TranscludeConfig;
use crate::render::markdown::block::{classify, BlockKind, Carry};

/// A picture an embed reserved room for. The bytes ride along already
/// base64-encoded, because the terminal wants them that way on every frame it
/// paints and the encode is not worth repeating at 60 Hz.
pub struct Picture {
    /// The box, in cells. The pixels are fitted into exactly this.
    pub cols: u16,
    pub rows: u16,
    /// Ready to hand to `image::draw`, or empty for a format no protocol takes.
    pub payload: Vec<u8>,
}

/// One screen row of an expanded embed.
pub enum Row {
    /// `╭ label ─────` — only when `transclude.border`.
    Top(String),
    /// A line of the target, with the block kind it was classified under.
    Body(String, BlockKind),
    /// `╰───────────`.
    Bottom,
    /// The target could not be resolved or read. Never silently empty
    /// (SPEC §14.2).
    Error(String),
    /// The FIRST row of a picture: it carries everything the painter needs, and
    /// the rows after it are `Reserved` blanks holding the space open. Split in
    /// two so the painter has exactly one row to key on — the top-left corner
    /// of the box, which is where the terminal wants the cursor.
    Image(Picture),
    /// A row of the picture's box. Drawn as spaces: whatever the terminal
    /// paints over them is the picture.
    Reserved,
    /// `photo.png · 800×600` under the picture, in the dim ink.
    Caption(String),
}

/// An expanded embed: what the `![[…]]` line draws instead of itself.
pub struct Expansion {
    pub rows: Vec<Row>,
}

impl Expansion {
    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

/// Expand one link against the filesystem.
///
/// Only ONE level deep, deliberately: a nested `![[…]]` inside an embed stays
/// as its own text on screen. Preview is for reading what you are composing,
/// and recursing on every keystroke would put an unbounded file walk in the
/// render path. `:export` is the recursive one.
pub fn expand(
    link: &Link,
    from: &Path,
    cfg: &TranscludeConfig,
    measure: u16,
    mode: Mode,
) -> Expansion {
    let label = link.label();

    // An image resolves like a note but expands into a PICTURE, so it forks
    // before the text path — `flatten_link` would read the bytes as prose.
    let root = super::compile::search_root(from, cfg);
    if let Ok(target) = super::link::resolve(link, from, &root) {
        if crate::image::looks_like_image(&target) {
            return picture(&target, label, cfg, measure);
        }
    }

    // One shared path with `:export` — resolve, strip, extract, demote, and in
    // `long`/`full` recurse — so the preview cannot drift from the output
    // again. It did once: an embedded `# H1` showed at level one on screen and
    // came out demoted in the file.
    let body = match super::compile::flatten_link(link, from, cfg, mode.recurses()) {
        Ok(b) => b,
        Err(e) => return one_error(label, e.to_string(), measure),
    };

    let framed = cfg.border && mode.framed();
    let mut rows = Vec::new();
    if framed {
        rows.push(Row::Top(label));
    }
    // Classified as a document in its own right, so a fence opened inside the
    // embed closes inside it and cannot leak into the composition.
    let mut carry = Carry::None;
    // A closed box costs four columns — `\u{2502} ` on the left and ` \u{2502}` on the
    // right — and a wrapped row has to fit between them.
    let inner = measure.saturating_sub(if cfg.border { 4 } else { 0 }).max(8);
    for (i, line) in body.lines().enumerate() {
        let (kind, next) = classify(line, &carry, i == 0);
        carry = next;
        // Wrapped here rather than at draw time: each wrapped row has to be a
        // row the cache can count, or the scroll arithmetic goes wrong.
        for (a, b) in crate::render::layout::wrap_line(line, inner) {
            let piece: String = line.chars().skip(a).take(b - a).collect();
            rows.push(Row::Body(piece, kind.clone()));
        }
    }
    if rows.is_empty() || matches!(rows.last(), Some(Row::Top(_))) {
        rows.push(Row::Body(String::new(), BlockKind::Blank));
    }
    if framed {
        rows.push(Row::Bottom);
    }
    Expansion { rows }
}

/// A picture is never taller than this, however tall its pixels are. A
/// portrait photograph would otherwise push the rest of the composition off the
/// screen, and an embed is a thing you read PAST.
const MAX_PICTURE_ROWS: u16 = 20;

/// Reserve room for an image and describe it.
///
/// The bytes are read once, here, rather than at paint time: this runs when the
/// line's cache entry is rebuilt, and painting runs every frame.
fn picture(path: &Path, label: String, cfg: &TranscludeConfig, measure: u16) -> Expansion {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return one_error(label, e.to_string(), measure),
    };
    let Some(info) = crate::image::probe(&bytes) else {
        return one_error(label, "not an image this build can read".into(), measure);
    };
    let inner = measure.saturating_sub(if cfg.border { 4 } else { 0 }).max(8);
    let (cols, rows) = crate::image::fit(info, inner, MAX_PICTURE_ROWS);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| label.clone());

    let mut out = vec![Row::Image(Picture {
        cols,
        rows,
        // A format no protocol accepts still gets its box and its caption —
        // it just never gets pixels, so there is nothing to send.
        payload: if info.sendable { bytes } else { Vec::new() },
    })];
    out.extend((1..rows).map(|_| Row::Reserved));
    out.push(Row::Caption(format!("{name} · {}×{}", info.width, info.height)));
    Expansion { rows: out }
}

/// An unresolved embed, wrapped to the measure. A path long enough to overflow
/// is exactly the case where the message matters, so it must not be the case
/// where the message is cut off.
fn one_error(label: String, why: String, measure: u16) -> Expansion {
    let msg = format!("![[{label}]] — {why}");
    let rows = crate::render::layout::wrap_line(&msg, measure.max(8))
        .into_iter()
        .map(|(a, b)| Row::Error(msg.chars().skip(a).take(b - a).collect()))
        .collect();
    Expansion { rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn vault(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = N.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("shoin-pv-{tag}-{t}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn body_text(e: &Expansion) -> Vec<String> {
        e.rows
            .iter()
            .filter_map(|r| match r {
                Row::Body(t, _) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn expands_a_target_with_a_labelled_border() {
        let d = vault("border");
        std::fs::write(d.join("frag.md"), "one\ntwo\n").unwrap();
        let link = Link::parse("frag").unwrap();

        let e = expand(&link, &d.join("note.md"), &TranscludeConfig::default(), 72, Mode::Short);
        assert!(matches!(e.rows.first(), Some(Row::Top(l)) if l == "frag"));
        assert!(matches!(e.rows.last(), Some(Row::Bottom)));
        assert_eq!(body_text(&e), vec!["one", "two"]);

        // …and without one when the setting says so.
        let cfg = TranscludeConfig { border: false, ..Default::default() };
        let e = expand(&link, &d.join("note.md"), &cfg, 72, Mode::Short);
        assert!(matches!(e.rows.first(), Some(Row::Body(_, _))));
        std::fs::remove_dir_all(&d).ok();
    }

    /// A missing target is an error ROW, never an empty expansion — the same
    /// rule `:export` follows, for the same reason.
    #[test]
    fn an_unresolved_embed_says_so_on_screen() {
        let d = vault("missing");
        let link = Link::parse("nope").unwrap();
        let e = expand(&link, &d.join("note.md"), &TranscludeConfig::default(), 72, Mode::Short);
        assert_eq!(e.len(), 1, "short enough to fit one row");
        match &e.rows[0] {
            Row::Error(msg) => assert!(msg.contains("nope"), "{msg}"),
            _ => panic!("expected an error row"),
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// The ladder is none · short · rec · full, and `none` is what the editor
    /// did before preview existed. `off` still works — it was the first name.
    #[test]
    fn the_mode_names_read_as_a_ladder() {
        assert_eq!(Mode::parse("none"), Some(Mode::Off));
        assert_eq!(Mode::parse("off"), Some(Mode::Off));
        assert_eq!(Mode::parse("short"), Some(Mode::Short));
        assert_eq!(Mode::parse("rec"), Some(Mode::Rec));
        assert_eq!(Mode::parse("long"), Some(Mode::Rec), "the obvious word works");
        assert_eq!(Mode::parse("full"), Some(Mode::Full));
        assert_eq!(Mode::parse("sideways"), None);

        // The name shown back is the ladder's, whichever synonym was typed.
        assert_eq!(Mode::Off.name(), "none");
        assert_eq!(Mode::Rec.name(), "rec");

        assert!(!Mode::Off.is_on());
        assert!(Mode::Short.is_on() && !Mode::Short.recurses());
        assert!(Mode::Rec.recurses() && Mode::Rec.framed());
        assert!(Mode::Full.recurses() && !Mode::Full.framed());
        // `none` is the default: an editor should not pull in other people's
        // files until asked.
        assert_eq!(Mode::default(), Mode::Off);
        assert_eq!(
            Mode::parse(&TranscludeConfig::default().embed),
            Some(Mode::Off)
        );
    }

    /// The three on-modes are a ladder: `short` stops at the target, `rec`
    /// follows the target's own embeds, and `full` is `rec` with the frame off.
    #[test]
    fn the_modes_show_progressively_more() {
        let d = vault("modes");
        std::fs::write(d.join("inner.md"), "the innermost text\n").unwrap();
        std::fs::write(d.join("frag.md"), "outer text\n\n![[inner]]\n").unwrap();
        let link = Link::parse("frag").unwrap();
        let cfg = TranscludeConfig::default();
        let from = d.join("note.md");

        let short = expand(&link, &from, &cfg, 72, Mode::Short);
        let body = body_text(&short).join("\n");
        assert!(body.contains("outer text"));
        assert!(!body.contains("the innermost text"), "short stops at the target");
        assert!(body.contains("![[inner]]"), "…leaving the nested link as text");
        assert!(matches!(short.rows.first(), Some(Row::Top(_))), "short is framed");

        let rec = expand(&link, &from, &cfg, 72, Mode::Rec);
        let body = body_text(&rec).join("\n");
        assert!(body.contains("outer text"));
        assert!(body.contains("the innermost text"), "rec follows the nested embed");
        assert!(!body.contains("![[inner]]"), "…and consumes the link");
        assert!(matches!(rec.rows.first(), Some(Row::Top(_))), "rec is framed too");

        let full = expand(&link, &from, &cfg, 72, Mode::Full);
        let body = body_text(&full).join("\n");
        assert!(body.contains("the innermost text"), "full recurses like rec");
        assert!(
            !full.rows.iter().any(|r| matches!(r, Row::Top(_) | Row::Bottom)),
            "…but drops the frame"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// `full` is meant to be a live view of what `:export` writes, so the two
    /// must actually agree.
    #[test]
    fn full_matches_what_compile_would_write() {
        let d = vault("fullcmp");
        std::fs::write(d.join("inner.md"), "# Inner\n\ninner body\n").unwrap();
        std::fs::write(d.join("frag.md"), "# Outer\n\nouter body\n\n![[inner]]\n").unwrap();
        let from = d.join("note.md");
        std::fs::write(&from, "![[frag]]\n").unwrap();
        let cfg = TranscludeConfig::default();

        let shown = body_text(&expand(
            &Link::parse("frag").unwrap(),
            &from,
            &cfg,
            72,
            Mode::Full,
        ))
        .join("\n");
        let compiled = crate::transclude::compile::compile(&from, &cfg).unwrap();
        for line in shown.lines().filter(|l| !l.trim().is_empty()) {
            assert!(
                compiled.contains(line.trim()),
                "full shows {line:?}, which the compiled file does not"
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// A preview must be a preview of the OUTPUT, not of the source. Two things
    /// used to differ and both made it lie: an embedded `# H1` showed at level
    /// one on screen and came out demoted in the compiled file, and a block id
    /// showed on screen and was stripped from the file.
    #[test]
    fn preview_shapes_its_content_the_way_compile_will() {
        let d = vault("agree");
        std::fs::write(d.join("frag.md"), "# Frag\n\nbody with an anchor ^abc\n").unwrap();
        let link = Link::parse("frag").unwrap();
        let cfg = TranscludeConfig::default();
        let from = d.join("note.md");

        let e = expand(&link, &from, &cfg, 72, Mode::Short);
        let body = body_text(&e).join("\n");
        assert!(body.contains("## Frag"), "demoted by heading_offset, as compile does");
        assert!(!body.contains("^abc"), "block ids go, as compile does");
        assert!(body.contains("body with an anchor"), "…and the prose stays");

        // The two really do agree, not just each look right on their own.
        std::fs::write(&from, "![[frag]]\n").unwrap();
        let compiled = crate::transclude::compile::compile(&from, &cfg).unwrap();
        for line in body.lines().filter(|l| !l.trim().is_empty()) {
            assert!(
                compiled.contains(line.trim()),
                "preview shows {line:?}, which the compiled file does not"
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// The embed is classified as its own document: a fence it opens closes
    /// inside it, and cannot swallow the composition below.
    #[test]
    fn an_embedded_fence_is_contained() {
        let d = vault("fence");
        std::fs::write(d.join("frag.md"), "```rust\nlet x = 1;\n```\nafter\n").unwrap();
        let link = Link::parse("frag").unwrap();
        let e = expand(&link, &d.join("note.md"), &TranscludeConfig::default(), 72, Mode::Short);

        let kinds: Vec<&BlockKind> = e
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Body(_, k) => Some(k),
                _ => None,
            })
            .collect();
        assert!(matches!(kinds[0], BlockKind::FenceOpen(_)));
        assert!(matches!(kinds[1], BlockKind::FenceBody { .. }));
        assert!(matches!(kinds[2], BlockKind::FenceClose));
        assert!(
            matches!(kinds[3], BlockKind::Paragraph),
            "the fence closed inside the embed"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// An empty target still draws something, so a border never collapses onto
    /// itself.
    #[test]
    fn an_empty_target_still_has_a_body_row() {
        let d = vault("empty");
        std::fs::write(d.join("frag.md"), "").unwrap();
        let link = Link::parse("frag").unwrap();
        let e = expand(&link, &d.join("note.md"), &TranscludeConfig::default(), 72, Mode::Short);
        assert_eq!(e.len(), 3, "top, one blank body, bottom");
        std::fs::remove_dir_all(&d).ok();
    }

    /// A real 1x1 PNG. `probe` reads the header, so the fixture has to be one.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89,
    ];

    /// `![[photo.png]]` reserves a BOX the shape of the picture, and captions
    /// it — it does not paste the bytes in as text.
    #[test]
    fn an_image_embed_reserves_room_and_says_what_it_is() {
        let d = vault("image");
        let mut png = PNG.to_vec();
        png[16..20].copy_from_slice(&400u32.to_be_bytes());
        png[20..24].copy_from_slice(&200u32.to_be_bytes());
        std::fs::write(d.join("photo.png"), &png).unwrap();
        let from = d.join("comp.md");

        let cfg = TranscludeConfig::default();
        let e = expand(&Link::parse("photo.png").unwrap(), &from, &cfg, 60, Mode::Full);

        let first = e.rows.first().expect("a row");
        let Row::Image(pic) = first else {
            panic!("the first row carries the picture");
        };
        assert!(pic.cols > 0 && pic.rows > 0, "a box with room in it");
        assert!(!pic.payload.is_empty(), "a PNG goes to the terminal verbatim");
        assert_eq!(
            e.rows.iter().filter(|r| matches!(r, Row::Reserved)).count(),
            pic.rows as usize - 1,
            "the rows after the first hold the box open"
        );
        match e.rows.last() {
            Some(Row::Caption(c)) => {
                assert!(c.contains("photo.png"), "got {c:?}");
                assert!(c.contains("400×200"), "the caption states the pixels: {c:?}");
            }
            _ => panic!("a picture is captioned"),
        }
        assert!(
            !e.rows.iter().any(|r| matches!(r, Row::Body(..))),
            "no bytes were pasted in as prose"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// A file that ends in `.png` and is not one says so, rather than drawing
    /// an empty box or a screen of binary.
    #[test]
    fn a_file_that_only_claims_to_be_an_image_is_an_error() {
        let d = vault("notimage");
        std::fs::write(d.join("fake.png"), b"# actually markdown\n").unwrap();
        let from = d.join("comp.md");
        let e = expand(
            &Link::parse("fake.png").unwrap(),
            &from,
            &TranscludeConfig::default(),
            60,
            Mode::Full,
        );
        assert!(matches!(e.rows.first(), Some(Row::Error(_))), "an error, not a box");
        std::fs::remove_dir_all(&d).ok();
    }

}
