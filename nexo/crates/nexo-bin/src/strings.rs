//! ASCII and UTF-16LE strings in the data sections.

use crate::Image;

/// Shortest run kept (characters).
pub const MIN_LEN: usize = 4;
const MAX_STRINGS: usize = 1_000_000;
/// Longest piece kept as one string (characters); a longer run is split.
const MAX_TEXT: usize = 4096;

#[derive(Debug, Clone)]
pub struct Str {
    pub addr: u64,
    /// Bytes the string takes in the file.
    pub byte_len: u64,
    pub wide: bool,
    pub chars: usize,
    pub text: String,
}

fn ascii_ok(b: u8) -> bool {
    (0x20..=0x7e).contains(&b) || b == b'\t'
}

pub(crate) fn scan(img: &Image) -> Vec<Str> {
    let mut out = Vec::new();
    for sec in img.sections.iter().filter(|s| s.alloc && !s.exec && s.file_size > 0) {
        let data = img.section_bytes(sec);
        scan_ascii(data, sec.addr, &mut out);
        scan_wide(data, sec.addr, &mut out);
        if out.len() >= MAX_STRINGS {
            break;
        }
    }
    out.sort_by_key(|s| (s.addr, s.wide));
    out.truncate(MAX_STRINGS);
    out
}

fn scan_ascii(data: &[u8], base: u64, out: &mut Vec<Str>) {
    let mut i = 0;
    while i < data.len() && out.len() < MAX_STRINGS {
        if !ascii_ok(data[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < data.len() && ascii_ok(data[i]) {
            i += 1;
        }
        if i - start >= MIN_LEN {
            // a very long run (Rust packs string literals together) is kept in pieces
            for (k, piece) in data[start..i].chunks(MAX_TEXT).enumerate() {
                let text = piece.iter().map(|&b| b as char).collect();
                let addr = base + (start + k * MAX_TEXT) as u64;
                out.push(Str { addr, byte_len: piece.len() as u64, wide: false, chars: piece.len(), text });
            }
        }
    }
}

fn scan_wide(data: &[u8], base: u64, out: &mut Vec<Str>) {
    let mut i = 0;
    while i + 1 < data.len() && out.len() < MAX_STRINGS {
        if !(ascii_ok(data[i]) && data[i + 1] == 0) {
            i += 1;
            continue;
        }
        let start = i;
        // at most MAX_TEXT characters are held at once: a longer run is written out in pieces
        let mut chars = String::new();
        let mut piece_start = start;
        let mut total = 0usize;
        let mut pieces: Vec<Str> = Vec::new();
        let flush = |chars: &mut String, at: usize, pieces: &mut Vec<Str>| {
            let n = chars.len();
            pieces.push(Str {
                addr: base + at as u64,
                byte_len: 2 * n as u64,
                wide: true,
                chars: n,
                text: std::mem::take(chars),
            });
        };
        while i + 1 < data.len() && ascii_ok(data[i]) && data[i + 1] == 0 {
            if chars.len() == MAX_TEXT {
                flush(&mut chars, piece_start, &mut pieces);
                piece_start = i;
            }
            chars.push(data[i] as char);
            total += 1;
            i += 2;
        }
        if total >= MIN_LEN {
            if !chars.is_empty() {
                flush(&mut chars, piece_start, &mut pieces);
            }
            out.extend(pieces);
        } else {
            // a short run: move on by one byte so an odd-aligned string is not skipped
            i = start + 1;
        }
    }
}

/// The text of a string for one output line: control characters escaped, cut to `max` characters.
pub fn shorten(text: &str, max: usize) -> String {
    let mut out = String::new();
    for (n, c) in text.chars().enumerate() {
        if n == max {
            out.push_str("...");
            break;
        }
        match c {
            '\t' => out.push_str("\\t"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out
}
