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

/// A raw picture read from a format the vendors do not take (Netpbm or BMP), ready to be
/// written as a PNG.
#[derive(Debug, PartialEq, Eq)]
pub struct Raster {
    /// `PPM`, `PGM` or `BMP`.
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
    /// 8-bit samples, rows top to bottom: grey (1 per pixel) or RGB (3 per pixel).
    pixels: Vec<u8>,
    channels: u8,
}

impl Raster {
    /// The picture as PNG bytes.
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let mut out = std::io::Cursor::new(Vec::new());
        let (w, h, px) = (self.width, self.height, self.pixels.clone());
        let done = if self.channels == 1 {
            image::GrayImage::from_raw(w, h, px).map(|i| i.write_to(&mut out, image::ImageFormat::Png))
        } else {
            image::RgbImage::from_raw(w, h, px).map(|i| i.write_to(&mut out, image::ImageFormat::Png))
        };
        match done {
            Some(Ok(())) => Ok(out.into_inner()),
            Some(Err(e)) => Err(e.to_string()),
            None => Err("the pixel data does not match the size".into()),
        }
    }
}

/// A picture in Netpbm (P6 or P5) or uncompressed BMP form, decoded; None when the bytes are
/// not one, Some(Err) when it is one that cannot be decoded (the reason).
pub fn decode_raster(bytes: &[u8]) -> Option<Result<Raster, String>> {
    netpbm(bytes).or_else(|| bmp(bytes))
}

/// The next header number of a Netpbm file, after blanks and `#` comments.
fn pnm_number(b: &[u8], i: &mut usize) -> Option<u32> {
    loop {
        match b.get(*i)? {
            c if c.is_ascii_whitespace() => *i += 1,
            b'#' => {
                while *b.get(*i)? != b'\n' {
                    *i += 1;
                }
            }
            _ => break,
        }
    }
    let start = *i;
    while b.get(*i).is_some_and(u8::is_ascii_digit) {
        *i += 1;
    }
    if *i == start || *i - start > 9 {
        return None;
    }
    std::str::from_utf8(&b[start..*i]).ok()?.parse().ok()
}

/// Binary PPM (P6) and PGM (P5); 16-bit samples are brought down to 8.
fn netpbm(b: &[u8]) -> Option<Result<Raster, String>> {
    let (label, channels) = match b.get(..3)? {
        [b'P', b'6', c] if c.is_ascii_whitespace() => ("PPM", 3usize),
        [b'P', b'5', c] if c.is_ascii_whitespace() => ("PGM", 1),
        _ => return None,
    };
    let mut i = 2;
    let w = pnm_number(b, &mut i)?;
    let h = pnm_number(b, &mut i)?;
    let max = pnm_number(b, &mut i)?;
    if !b.get(i)?.is_ascii_whitespace() || w == 0 || h == 0 {
        return None;
    }
    let data = &b[i + 1..];
    if max == 0 || max > 65_535 {
        return Some(Err(format!("maxval {max}; only 1 to 65535 is supported")));
    }
    let wide = max > 255;
    let samples = u64::from(w) * u64::from(h) * channels as u64;
    if samples * (1 + u64::from(wide)) > data.len() as u64 {
        return Some(Err("the pixel data is cut short".into()));
    }
    let samples = samples as usize;
    let scale = |v: u32| (v * 255 + max / 2) / max;
    let pixels = if !wide && max == 255 {
        data[..samples].to_vec()
    } else if wide {
        data.as_chunks::<2>().0.iter().take(samples).map(|&c| scale(u32::from(u16::from_be_bytes(c))) as u8).collect()
    } else {
        data[..samples].iter().map(|&v| scale(u32::from(v).min(max)) as u8).collect()
    };
    Some(Ok(Raster { label, width: w, height: h, pixels, channels: channels as u8 }))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Uncompressed 24 and 32-bit BMP, bottom-up or top-down.
fn bmp(b: &[u8]) -> Option<Result<Raster, String>> {
    if !b.starts_with(b"BM") || !matches!(le32(b, 14)?, 12 | 40 | 52 | 56 | 64 | 108 | 124) {
        return None;
    }
    if le32(b, 14)? == 12 {
        return Some(Err("the old OS/2 header is not supported".into()));
    }
    let (w, h) = (le32(b, 18)? as i32, le32(b, 22)? as i32);
    let (bpp, compression) = (le16(b, 28)?, le32(b, 30)?);
    if le16(b, 26)? != 1 || w <= 0 || h == 0 || h == i32::MIN {
        return None;
    }
    if !matches!(bpp, 24 | 32) || compression != 0 {
        return Some(Err(format!(
            "{bpp}-bit with compression {compression}; only uncompressed 24 and 32-bit is supported"
        )));
    }
    let (width, height, top_down) = (w as u32, h.unsigned_abs(), h < 0);
    let stride = (u64::from(width) * u64::from(bpp)).div_ceil(32) * 4;
    let start = le32(b, 10)? as usize;
    let need = stride * u64::from(height);
    if b.len().checked_sub(start).is_none_or(|have| (have as u64) < need) {
        return Some(Err("the pixel data is cut short".into()));
    }
    let bytes_px = (bpp / 8) as usize;
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height as usize {
        let row = if top_down { y } else { height as usize - 1 - y };
        let line = &b[start + row * stride as usize..][..width as usize * bytes_px];
        for px in line.chunks_exact(bytes_px) {
            pixels.extend([px[2], px[1], px[0]]);
        }
    }
    Some(Ok(Raster { label: "BMP", width, height, pixels, channels: 3 }))
}

/// A picture format `read` cannot decode, by its first bytes: its name and the extension to
/// convert from.
pub fn foreign(b: &[u8]) -> Option<(&'static str, &'static str)> {
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        Some(("TIFF", "tiff"))
    } else if b.starts_with(&[0, 0, 1, 0]) && le16(b, 4).is_some_and(|n| n > 0) && b.len() >= 22 {
        Some(("ICO", "ico"))
    } else if b.get(4..8) == Some(b"ftyp") {
        match b.get(8..12)? {
            b"avif" | b"avis" => Some(("AVIF", "avif")),
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1" => Some(("HEIC", "heic")),
            _ => None,
        }
    } else {
        None
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

    /// The 8-bit RGB pixels of a PNG, and its size.
    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).unwrap().to_rgb8();
        (img.width(), img.height(), img.into_raw())
    }

    fn p6(w: u32, h: u32, pixels: &[u8]) -> Vec<u8> {
        let mut f = format!("P6\n# a comment\n{w} {h}\n255\n").into_bytes();
        f.extend(pixels);
        f
    }

    #[test]
    fn a_ppm_converts_to_a_png_with_the_same_pixels() {
        let px: Vec<u8> = (0..2 * 3 * 3).map(|i| (i * 7) as u8).collect();
        let r = decode_raster(&p6(2, 3, &px)).unwrap().unwrap();
        assert_eq!((r.label, r.width, r.height), ("PPM", 2, 3));
        let png = r.to_png().unwrap();
        assert_eq!(sniff(&png).map(|p| (p.media_type, p.width, p.height)), Some(("image/png", 2, 3)));
        assert_eq!(decode(&png), (2, 3, px));
    }

    #[test]
    fn a_pgm_converts_and_a_16_bit_one_comes_down_to_8() {
        let mut f = b"P5 3 1 255\n".to_vec();
        f.extend([0, 128, 255]);
        let r = decode_raster(&f).unwrap().unwrap();
        assert_eq!(r.label, "PGM");
        assert_eq!(decode(&r.to_png().unwrap()), (3, 1, vec![0, 0, 0, 128, 128, 128, 255, 255, 255]));
        let mut wide = b"P5\n2 1\n65535\n".to_vec();
        wide.extend([0xFF, 0xFF, 0x00, 0x00]);
        let r = decode_raster(&wide).unwrap().unwrap();
        assert_eq!(decode(&r.to_png().unwrap()).2, vec![255, 255, 255, 0, 0, 0]);
        let mut low = b"P5 2 1 15\n".to_vec();
        low.extend([15, 0]);
        assert_eq!(decode(&decode_raster(&low).unwrap().unwrap().to_png().unwrap()).2, vec![255, 255, 255, 0, 0, 0]);
    }

    #[test]
    fn a_broken_netpbm_is_an_error_and_text_is_not_one() {
        assert!(decode_raster(&p6(4, 4, &[0; 5])).unwrap().unwrap_err().contains("cut short"));
        assert!(decode_raster(b"P6 2 2 0\n").unwrap().is_err());
        assert_eq!(decode_raster(b"P6 is a format name"), None);
        assert_eq!(decode_raster(b"P3\n1 1\n255\n0 0 0\n"), None);
    }

    /// A 24-bit BMP of `w` by `h`; `rows` are top to bottom RGB, stored bottom-up unless `top_down`.
    fn bmp24(w: u32, h: u32, rows: &[Vec<u8>], top_down: bool) -> Vec<u8> {
        let stride = (w * 3).div_ceil(4) * 4;
        let mut f = b"BM".to_vec();
        f.extend((54 + stride * h).to_le_bytes());
        f.extend([0; 4]);
        f.extend(54u32.to_le_bytes());
        f.extend(40u32.to_le_bytes());
        f.extend(w.to_le_bytes());
        f.extend((if top_down { -(h as i32) } else { h as i32 }).to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(24u16.to_le_bytes());
        f.extend([0; 24]); // compression 0, size, resolution, colours
        let order: Vec<&Vec<u8>> = if top_down { rows.iter().collect() } else { rows.iter().rev().collect() };
        for row in order {
            let mut line: Vec<u8> = row.chunks(3).flat_map(|p| [p[2], p[1], p[0]]).collect();
            line.resize(stride as usize, 0);
            f.extend(line);
        }
        f
    }

    #[test]
    fn a_bmp_converts_bottom_up_and_top_down() {
        let rows = vec![vec![255, 0, 0, 0, 255, 0], vec![0, 0, 255, 9, 8, 7]]; // width 2 pads each row
        for top_down in [false, true] {
            let r = decode_raster(&bmp24(2, 2, &rows, top_down)).unwrap().unwrap();
            assert_eq!((r.label, r.width, r.height), ("BMP", 2, 2));
            assert_eq!(decode(&r.to_png().unwrap()), (2, 2, rows.concat()), "top_down {top_down}");
        }
    }

    #[test]
    fn a_32_bit_bmp_drops_its_alpha_and_other_bmps_are_errors() {
        let mut f = bmp24(1, 1, &[vec![1, 2, 3]], true);
        f[28] = 32;
        f.truncate(54);
        f.extend([3, 2, 1, 0]);
        assert_eq!(decode(&decode_raster(&f).unwrap().unwrap().to_png().unwrap()), (1, 1, vec![1, 2, 3]));
        f[28] = 8;
        assert!(decode_raster(&f).unwrap().unwrap_err().contains("8-bit"));
        assert_eq!(decode_raster(b"BM and then some text, not a picture at all"), None);
    }

    #[test]
    fn tiff_ico_avif_and_heic_are_named() {
        assert_eq!(foreign(b"II*\0\x08\0\0\0"), Some(("TIFF", "tiff")));
        assert_eq!(foreign(b"MM\0*\0\0\0\x08"), Some(("TIFF", "tiff")));
        let mut ico = vec![0, 0, 1, 0, 1, 0];
        ico.resize(22, 0);
        assert_eq!(foreign(&ico), Some(("ICO", "ico")));
        assert_eq!(foreign(b"\0\0\0\x18ftypavif\0\0\0\0"), Some(("AVIF", "avif")));
        assert_eq!(foreign(b"\0\0\0\x18ftypheic\0\0\0\0"), Some(("HEIC", "heic")));
        assert_eq!(foreign(b"\0\0\0\x18ftypmp42\0\0\0\0"), None);
        assert_eq!(foreign(b"plain text"), None);
    }
}
