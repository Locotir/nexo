//! Telling a picture file by its first bytes (never its extension) and reading its size from the
//! header, so `read` can show it to a model without decoding it.

/// The largest picture `read` shows (the vendors take about 5 MB).
pub const MAX_BYTES: usize = 5_000_000;
/// The longest side, in pixels, of a picture `read` shows.
pub const MAX_SIDE: u32 = 2000;

/// What the header of a PNG, JPEG, GIF or WebP file says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Picture {
    pub media_type: &'static str,
    pub width: u32,
    pub height: u32,
}

impl Picture {
    /// `PNG`, `JPEG`, `GIF` or `WebP`.
    pub fn label(&self) -> &'static str {
        match self.media_type {
            "image/png" => "PNG",
            "image/jpeg" => "JPEG",
            "image/gif" => "GIF",
            _ => "WebP",
        }
    }
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes([*b.get(at)?, *b.get(at + 1)?])))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?])))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(*b.get(at)?) | u32::from(*b.get(at + 1)?) << 8 | u32::from(*b.get(at + 2)?) << 16)
}

/// The picture `bytes` hold, or None when they are not one (or the header is cut short).
pub fn sniff(bytes: &[u8]) -> Option<Picture> {
    let (media_type, (width, height)) = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let w = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
        ("image/png", (w, h))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        ("image/gif", (le16(bytes, 6)?, le16(bytes, 8)?))
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ("image/jpeg", jpeg_size(bytes)?)
    } else if bytes.len() >= 16 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        ("image/webp", webp_size(bytes)?)
    } else {
        return None;
    };
    (width > 0 && height > 0).then_some(Picture { media_type, width, height })
}

/// The size in the first start-of-frame segment.
fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        // standalone markers carry no length
        if marker == 0x01 || (0xD0..=0xD8).contains(&marker) {
            i += 2;
            continue;
        }
        let len = be16(b, i + 2)? as usize;
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return Some((be16(b, i + 7)?, be16(b, i + 5)?));
        }
        i += 2 + len;
    }
    None
}

fn webp_size(b: &[u8]) -> Option<(u32, u32)> {
    match b.get(12..16)? {
        b"VP8 " => Some((le16(b, 26)? & 0x3FFF, le16(b, 28)? & 0x3FFF)),
        b"VP8L" => {
            let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
            Some(((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
        }
        b"VP8X" => Some((le24(b, 24)? + 1, le24(b, 27)? + 1)),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A valid PNG of one colour, `w` by `h` (stored deflate blocks, so no compressor is needed).
    pub fn png(w: u32, h: u32) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut c = !0u32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 == 1 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
                }
            }
            !c
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend((data.len() as u32).to_be_bytes());
            let mut body = kind.to_vec();
            body.extend(data);
            out.extend(&body);
            out.extend(crc(&body).to_be_bytes());
        }
        let mut raw = Vec::new();
        for _ in 0..h {
            raw.push(0); // filter: none
            raw.extend(std::iter::repeat_n(0xFFu8, w as usize)); // 8-bit grey
        }
        let mut z = vec![0x78, 0x01];
        let mut blocks = raw.chunks(60_000).peekable();
        while let Some(b) = blocks.next() {
            z.push(u8::from(blocks.peek().is_none()));
            z.extend((b.len() as u16).to_le_bytes());
            z.extend((!(b.len() as u16)).to_le_bytes());
            z.extend(b);
        }
        let (mut a, mut s) = (1u32, 0u32);
        for &b in &raw {
            a = (a + u32::from(b)) % 65_521;
            s = (s + a) % 65_521;
        }
        z.extend(((s << 16) | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend(w.to_be_bytes());
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, 0, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn a_png_header_gives_its_size() {
        let p = sniff(&png(7, 3)).unwrap();
        assert_eq!((p.media_type, p.width, p.height, p.label()), ("image/png", 7, 3, "PNG"));
    }

    #[test]
    fn gif_jpeg_and_webp_headers_give_their_size() {
        let mut gif = b"GIF89a".to_vec();
        gif.extend([5, 0, 4, 0]);
        assert_eq!(sniff(&gif).map(|p| (p.width, p.height)), Some((5, 4)));
        // SOI, an APP0 segment to skip, then SOF0 (height 9, width 11)
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpg.extend([0xFF, 0xC0, 0x00, 0x0B, 8, 0, 9, 0, 11, 1, 1, 0x11, 0]);
        let p = sniff(&jpg).unwrap();
        assert_eq!((p.media_type, p.width, p.height), ("image/jpeg", 11, 9));
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        webp.extend([10, 0, 0, 0, 0, 0, 0, 0]); // chunk size and flags
        webp.extend([63, 0, 0, 31, 0, 0]); // width 64, height 32
        let p = sniff(&webp).unwrap();
        assert_eq!((p.media_type, p.width, p.height), ("image/webp", 64, 32));
        let mut lossless = b"RIFF\0\0\0\0WEBPVP8L".to_vec();
        lossless.extend([5, 0, 0, 0, 0x2F]);
        lossless.extend((99u32 | (49 << 14)).to_le_bytes()); // 100 by 50
        let p = sniff(&lossless).unwrap();
        assert_eq!((p.width, p.height), (100, 50));
    }

    #[test]
    fn what_is_not_a_picture_is_none() {
        assert_eq!(sniff(b"hello, this is text"), None);
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n"), None, "header cut short");
        assert_eq!(sniff(&[]), None);
    }
}
