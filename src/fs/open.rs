//! Opening files. SPEC.md §10.
//!
//! UTF-8 is required. Invalid bytes are REFUSED with a clear message rather
//! than lossily converted — the user is about to overwrite this file, and a
//! silent replacement-character substitution would destroy data on save.

use std::path::Path;

use anyhow::{anyhow, Result};
use ropey::Rope;

use crate::text::buffer::{LineEnding, Syntax};

pub struct Loaded {
    pub rope: Rope,
    pub line_ending: LineEnding,
    pub final_newline: bool,
    pub syntax: Syntax,
}

pub fn load(path: &Path, plain_text_exts: &[String]) -> Result<Loaded> {
    let bytes = std::fs::read(path)?;

    let text = String::from_utf8(bytes).map_err(|e| {
        let pos = e.utf8_error().valid_up_to();
        anyhow!(
            "{} is not valid UTF-8 (first bad byte at offset {}). \
             Shoin refuses to open it rather than corrupt it on save.",
            path.display(),
            pos
        )
    })?;

    let line_ending = detect_line_ending(&text);
    let final_newline = text.ends_with('\n');

    // The rope always holds \n only; the original ending is reapplied on save.
    let normalized = if line_ending == LineEnding::Crlf {
        text.replace("\r\n", "\n")
    } else {
        text
    };

    Ok(Loaded {
        rope: Rope::from_str(&normalized),
        line_ending,
        final_newline,
        syntax: syntax_for(path, plain_text_exts),
    })
}

/// Majority wins. A file with mixed endings is normalized on save to whichever
/// dominated on load.
pub fn detect_line_ending(text: &str) -> LineEnding {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    // `lf` counts the \n inside every \r\n too, so bare LFs are lf - crlf.
    if crlf > lf.saturating_sub(crlf) {
        LineEnding::Crlf
    } else {
        LineEnding::Lf
    }
}

/// `.md`/`.markdown` → full Markdown; configured plain-text extensions →
/// reduced ruleset; anything else → no styling.
pub fn syntax_for(path: &Path, plain_text_exts: &[String]) -> Syntax {
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();

    if ext == ".md" || ext == ".markdown" {
        Syntax::Markdown
    } else if plain_text_exts.iter().any(|e| e.eq_ignore_ascii_case(&ext)) {
        Syntax::PlainProse
    } else {
        Syntax::None
    }
}
