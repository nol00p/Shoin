//! Fixtures for tests that start with an in-memory document.

use crate::app::App;
use crate::config::Config;
use crate::render::markdown::block::BlockCache;
use crate::text::cursor::Cursor;

pub(crate) fn app_with(text: &str) -> App {
    app_with_config(text, Config::default())
}

pub(crate) fn app_with_config(text: &str, config: Config) -> App {
    let mut app = App::new(config, None, None).unwrap();
    app.buffer.insert_exact(Cursor::new(0, 0), text);
    app.buffer.cursor = Cursor::new(0, 0);
    app.blocks = BlockCache::build(&app.buffer, app.config.markdown.math, app.config.editor.tab_width);
    app
}

/// Treat the fixture text as the starting document for undo and redo.
pub(crate) fn app_with_clean_history(text: &str) -> App {
    let mut app = app_with(text);
    app.buffer.history = Default::default();
    app
}

/// A PNG `width`×`height` pixels as far as anything reading its header
/// can tell, with valid chunk CRCs: a real 1×1 image's pixel data under a
/// header that says otherwise. Enough for the Mermaid renderer's checks and
/// for sizing a picture; never decoded.
pub(crate) fn sized_png(width: u32, height: u32) -> Vec<u8> {
    fn crc(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let sum = crc(&out[start..]);
        out.extend_from_slice(&sum.to_be_bytes());
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = width.to_be_bytes().to_vec();
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &[0x78, 0x9c, 0x63, 0xf8, 0x0f, 0x04, 0x00, 0x09, 0xfb, 0x03, 0xfd]);
    chunk(&mut out, b"IEND", &[]);
    out
}
