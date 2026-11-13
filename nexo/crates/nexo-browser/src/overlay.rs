//! Pure-Rust drawing on RGBA frames for the GIF export: click circles, drag arrows, label boxes, a progress bar and
//! a watermark. Text is the 8x8 bitmap font of the `font8x8` crate (MIT), scaled by whole pixels.

use font8x8::legacy::BASIC_LEGACY;

pub(crate) type Rgb = [u8; 3];

pub(crate) const ORANGE: Rgb = [255, 120, 0];
pub(crate) const RED: Rgb = [230, 30, 30];
const BLACK: Rgb = [0, 0, 0];
const WHITE: Rgb = [255, 255, 255];

/// An RGBA8 picture drawn on in place.
pub(crate) struct Canvas<'a> {
    w: u32,
    h: u32,
    px: &'a mut [u8],
}

impl<'a> Canvas<'a> {
    /// `None` when the buffer is not `w * h * 4` bytes.
    pub(crate) fn new(w: u32, h: u32, px: &'a mut [u8]) -> Option<Self> {
        (px.len() as u64 == u64::from(w) * u64::from(h) * 4).then_some(Canvas { w, h, px })
    }

    /// Size of strokes and text relative to a 400 px picture, so overlays look alike on a phone and a desktop frame.
    pub(crate) fn unit(&self) -> f32 {
        (((self.w as f32) * (self.h as f32)).sqrt() / 400.0).max(1.0)
    }

    /// Whole-pixel scale of the bitmap font.
    fn font_scale(&self) -> u32 {
        self.unit().round().max(1.0) as u32
    }

    /// Mixes `c` into one pixel with weight `a` (0 to 1).
    fn blend(&mut self, x: i32, y: i32, c: Rgb, a: f32) {
        if a <= 0.0 || x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let i = (y as usize * self.w as usize + x as usize) * 4;
        let a = a.min(1.0);
        for (k, v) in c.iter().enumerate() {
            self.px[i + k] = (f32::from(self.px[i + k]) * (1.0 - a) + f32::from(*v) * a).round() as u8;
        }
        self.px[i + 3] = 255;
    }

    /// Paints every pixel of the box whose centre `cover` gives a share of (0 to 1), times `alpha`.
    fn paint(&mut self, (x0, y0, x1, y1): (f32, f32, f32, f32), c: Rgb, alpha: f32, cover: impl Fn(f32, f32) -> f32) {
        let (xa, xb) = ((x0.floor() as i32).max(0), (x1.ceil() as i32).min(self.w as i32 - 1));
        let (ya, yb) = ((y0.floor() as i32).max(0), (y1.ceil() as i32).min(self.h as i32 - 1));
        for y in ya..=yb {
            for x in xa..=xb {
                let share = cover(x as f32 + 0.5, y as f32 + 0.5).clamp(0.0, 1.0);
                self.blend(x, y, c, share * alpha);
            }
        }
    }

    pub(crate) fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb, alpha: f32) {
        self.paint((x as f32, y as f32, (x + w - 1) as f32, (y + h - 1) as f32), c, alpha, |px, py| {
            if px >= x as f32 && px < (x + w) as f32 && py >= y as f32 && py < (y + h) as f32 {
                1.0
            } else {
                0.0
            }
        });
    }

    /// A filled circle with a soft edge.
    pub(crate) fn disc(&mut self, cx: f32, cy: f32, r: f32, c: Rgb, alpha: f32) {
        self.paint((cx - r - 1.0, cy - r - 1.0, cx + r + 1.0, cy + r + 1.0), c, alpha, |px, py| {
            r + 0.5 - (px - cx).hypot(py - cy)
        });
    }

    /// A circle outline `thick` wide, centred on radius `r`.
    pub(crate) fn ring(&mut self, cx: f32, cy: f32, r: f32, thick: f32, c: Rgb, alpha: f32) {
        let reach = r + thick;
        self.paint((cx - reach, cy - reach, cx + reach, cy + reach), c, alpha, |px, py| {
            thick / 2.0 + 0.5 - ((px - cx).hypot(py - cy) - r).abs()
        });
    }

    /// A straight stroke `thick` wide with round ends.
    pub(crate) fn line(&mut self, (x0, y0): (f32, f32), (x1, y1): (f32, f32), thick: f32, c: Rgb, alpha: f32) {
        let pad = thick + 1.0;
        let bounds = (x0.min(x1) - pad, y0.min(y1) - pad, x0.max(x1) + pad, y0.max(y1) + pad);
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = dx * dx + dy * dy;
        self.paint(bounds, c, alpha, |px, py| {
            let t = if len2 == 0.0 { 0.0 } else { (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0) };
            thick / 2.0 + 0.5 - (px - (x0 + t * dx)).hypot(py - (y0 + t * dy))
        });
    }

    /// A stroke from `from` to `to` with a two-barbed head at `to`.
    pub(crate) fn arrow(&mut self, from: (f32, f32), to: (f32, f32), thick: f32, head: f32, c: Rgb, alpha: f32) {
        self.line(from, to, thick, c, alpha);
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let len = dx.hypot(dy);
        if len < 1.0 {
            return;
        }
        // The barbs point back along the stroke, 28 degrees to either side.
        let (ux, uy) = (-dx / len, -dy / len);
        let (sin, cos) = 0.4695_f32.sin_cos();
        for s in [sin, -sin] {
            let (bx, by) = (ux * cos - uy * s, ux * s + uy * cos);
            self.line(to, (to.0 + bx * head, to.1 + by * head), thick, c, alpha);
        }
        self.disc(from.0, from.1, thick, c, alpha);
    }

    /// Width in pixels of `text` at whole-pixel `scale`.
    fn text_width(text: &str, scale: u32) -> i32 {
        (text.chars().count() as u32 * 8 * scale) as i32
    }

    /// Draws `text` with its top-left corner at `(x, y)`; characters the font lacks show as `?`.
    fn text(&mut self, x: i32, y: i32, text: &str, scale: u32, c: Rgb, alpha: f32) {
        for (i, ch) in text.chars().enumerate() {
            let glyph = BASIC_LEGACY[if ch.is_ascii() && ch >= ' ' { ch as usize } else { '?' as usize }];
            let gx = x + (i as u32 * 8 * scale) as i32;
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..8 {
                    if bits >> col & 1 == 1 {
                        let (px, py) = (gx + (col * scale) as i32, y + (row as u32 * scale) as i32);
                        self.fill_rect(px, py, scale as i32, scale as i32, c, alpha);
                    }
                }
            }
        }
    }

    /// An orange circle at a click; `phase` 0 is the press (filled), 1 the ring that spreads after it.
    pub(crate) fn click_mark(&mut self, x: f32, y: f32, phase: u8) {
        let u = self.unit();
        if phase == 0 {
            self.disc(x, y, 14.0 * u, ORANGE, 0.35);
            self.ring(x, y, 14.0 * u, 3.0 * u, ORANGE, 0.95);
        } else {
            self.ring(x, y, 22.0 * u, 2.5 * u, ORANGE, 0.7);
        }
    }

    /// A red arrow from the start of a drag to its end.
    pub(crate) fn drag_mark(&mut self, from: (f32, f32), to: (f32, f32)) {
        let u = self.unit();
        self.arrow(from, to, 3.0 * u, 14.0 * u, RED, 0.95);
    }

    /// Black label boxes with white text, stacked upward from the bottom centre of the frame (the last text is the
    /// lowest). Texts too long for the width are cut with `...`.
    pub(crate) fn labels(&mut self, texts: &[&str]) {
        let (scale, u) = (self.font_scale(), self.unit());
        let pad = (2 * scale) as i32;
        let gap = (2.0 * u) as i32;
        let margin = (6.0 * u) as i32;
        let fit = ((self.w as i32 - 2 * margin - 2 * pad) / (8 * scale as i32)).max(1) as usize;
        let mut bottom = self.h as i32 - self.bar_height() - margin;
        for text in texts.iter().rev() {
            let shown = if text.chars().count() > fit {
                let keep = fit.saturating_sub(3);
                format!("{}...", text.chars().take(keep).collect::<String>())
            } else {
                (*text).to_string()
            };
            let (bw, bh) = (Self::text_width(&shown, scale) + 2 * pad, (8 * scale) as i32 + 2 * pad);
            let x = ((self.w as i32 - bw) / 2).max(0);
            let y = bottom - bh;
            self.fill_rect(x, y, bw, bh, BLACK, 0.82);
            self.text(x + pad, y + pad, &shown, scale, WHITE, 1.0);
            bottom = y - gap;
        }
    }

    fn bar_height(&self) -> i32 {
        (3.0 * self.unit()).round().max(3.0) as i32
    }

    /// An orange bar along the bottom edge, `done` (0 to 1) of the width long.
    pub(crate) fn progress(&mut self, done: f64) {
        let bar = self.bar_height();
        let len = (f64::from(self.w) * done.clamp(0.0, 1.0)).round() as i32;
        self.fill_rect(0, self.h as i32 - bar, len, bar, ORANGE, 1.0);
    }

    /// A small `nexo` mark in the top-right corner.
    pub(crate) fn watermark(&mut self) {
        let scale = self.font_scale().saturating_sub(1).max(1);
        let pad = scale as i32 * 2;
        let (bw, bh) = (Self::text_width("nexo", scale) + 2 * pad, (8 * scale) as i32 + 2 * pad);
        let x = self.w as i32 - bw - pad;
        self.fill_rect(x, pad, bw, bh, BLACK, 0.45);
        self.text(x + pad, pad + pad, "nexo", scale, WHITE, 0.9);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(w: u32, h: u32) -> Vec<u8> {
        vec![255; (w * h * 4) as usize]
    }

    fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2]]
    }

    fn near(a: [u8; 3], b: Rgb, tol: i32) -> bool {
        a.iter().zip(b).all(|(p, q)| (i32::from(*p) - i32::from(q)).abs() <= tol)
    }

    #[test]
    fn the_buffer_must_match_the_size() {
        let mut short = vec![0; 10];
        assert!(Canvas::new(4, 4, &mut short).is_none());
    }

    #[test]
    fn a_click_is_an_orange_ring_over_a_tinted_disc() {
        let (w, h) = (200, 150);
        let mut px = white(w, h);
        let mut c = Canvas::new(w, h, &mut px).unwrap();
        c.click_mark(100.0, 75.0, 0);
        // On the ring (radius 14 at unit 1): solid orange.
        assert!(near(at(&px, w, 114, 75), ORANGE, 20), "{:?}", at(&px, w, 114, 75));
        // Inside the disc: a light orange tint, not white and not solid.
        let inside = at(&px, w, 100, 75);
        assert!(inside[0] > 240 && inside[1] < 230 && inside[2] < 200 && inside[2] > 100, "{inside:?}");
        // Far away: untouched.
        assert_eq!(at(&px, w, 10, 10), [255, 255, 255]);
        // The spreading ring of phase 1 sits further out and leaves the disc centre alone.
        let mut px = white(w, h);
        Canvas::new(w, h, &mut px).unwrap().click_mark(100.0, 75.0, 1);
        assert!(near(at(&px, w, 122, 75), ORANGE, 90), "{:?}", at(&px, w, 122, 75));
        assert_eq!(at(&px, w, 100, 75), [255, 255, 255]);
    }

    #[test]
    fn a_drag_is_a_red_arrow_with_a_head() {
        let (w, h) = (200, 150);
        let mut px = white(w, h);
        let mut c = Canvas::new(w, h, &mut px).unwrap();
        c.drag_mark((20.0, 75.0), (180.0, 75.0));
        // On the shaft.
        assert!(near(at(&px, w, 100, 75), RED, 25), "{:?}", at(&px, w, 100, 75));
        // On a barb: behind the tip and off the shaft.
        let barb = (0..h).filter(|&y| y != 75).any(|y| near(at(&px, w, 172, y), RED, 60));
        assert!(barb, "no barb near the tip");
        // Nothing beyond the tip or off the line far from it.
        assert_eq!(at(&px, w, 100, 20), [255, 255, 255]);
        assert_eq!(at(&px, w, 195, 75), [255, 255, 255]);
    }

    #[test]
    fn a_label_is_white_text_on_a_black_box_at_the_bottom() {
        let (w, h) = (200, 150);
        let mut px = white(w, h);
        let mut c = Canvas::new(w, h, &mut px).unwrap();
        c.labels(&["click Submit"]);
        let dark: Vec<(u32, u32)> =
            (100..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| at(&px, w, x, y)[0] < 60).collect();
        assert!(dark.len() > 300, "the box has {} dark pixels", dark.len());
        let (x0, x1) = (dark.iter().map(|p| p.0).min().unwrap(), dark.iter().map(|p| p.0).max().unwrap());
        let (y0, y1) = (dark.iter().map(|p| p.1).min().unwrap(), dark.iter().map(|p| p.1).max().unwrap());
        // Inside the box there are white text pixels.
        let text = (y0..=y1)
            .flat_map(|y| (x0..=x1).map(move |x| (x, y)))
            .filter(|&(x, y)| at(&px, w, x, y) == [255, 255, 255])
            .count();
        assert!(text > 20, "{text} text pixels");
        // The box is centred.
        assert!(((x0 + x1) as i32 - w as i32).abs() <= 2);
        // The top of the frame is untouched.
        assert_eq!(at(&px, w, 100, 10), [255, 255, 255]);
    }

    #[test]
    fn long_labels_are_cut_and_stay_inside_the_frame() {
        let (w, h) = (120, 90);
        let mut px = white(w, h);
        let mut c = Canvas::new(w, h, &mut px).unwrap();
        c.labels(&["a very long label that cannot possibly fit in this small frame", "second ünïcode"]);
        // Two stacked boxes, none wider than the frame: the corners above the stack stay white.
        assert_eq!(at(&px, w, 0, 0), [255, 255, 255]);
        assert!(at(&px, w, w / 2, h - 12)[0] < 100 || at(&px, w, w / 2, h - 14)[0] < 100);
    }

    #[test]
    fn the_progress_bar_grows_from_the_left() {
        let (w, h) = (200, 150);
        let mut px = white(w, h);
        Canvas::new(w, h, &mut px).unwrap().progress(0.5);
        assert!(near(at(&px, w, 5, h - 2), ORANGE, 0));
        assert!(near(at(&px, w, 99, h - 2), ORANGE, 0));
        assert_eq!(at(&px, w, 101, h - 2), [255, 255, 255]);
        assert_eq!(at(&px, w, 5, h - 20), [255, 255, 255]);
    }

    #[test]
    fn the_watermark_marks_the_top_right() {
        let (w, h) = (200, 150);
        let mut px = white(w, h);
        Canvas::new(w, h, &mut px).unwrap().watermark();
        let marked = (0..20).any(|y| (150..200).any(|x| at(&px, w, x, y) != [255, 255, 255]));
        assert!(marked);
        assert_eq!(at(&px, w, 10, 10), [255, 255, 255]);
        assert_eq!(at(&px, w, 100, 140), [255, 255, 255]);
    }
}
