//! Scrollbars that answer the mouse: where the thumb is, what a click hits,
//! which offset a dragged thumb stands for, and how the bar is drawn.
//!
//! A bar is one column `len` rows tall. The thumb's size is the share of the
//! content in view; its position is the share of the scroll done. The hit
//! area is two columns wide (the bar and the one to its left), which makes the
//! one-cell bar easy to catch.

use ratatui::buffer::Buffer;
use ratatui::style::{Modifier, Style};

use crate::theme::{mix, Theme};

/// What a bar scrolls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Transcript,
    /// The body of the dialog or window on top.
    Modal,
}

/// A scrollbar as drawn: its column, its first row and height, and the
/// scroll it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub target: Target,
    pub x: u16,
    pub y: u16,
    pub len: u16,
    /// Lines of content, lines in view, first line in view.
    pub content: usize,
    pub viewport: usize,
    pub offset: usize,
}

/// What a click on a bar hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The thumb, `grab` rows below its top.
    Thumb { grab: u16 },
    /// The track above the thumb.
    Before,
    /// The track below the thumb.
    After,
}

/// A scrollbar being dragged: the thumb is held `grab` rows below its top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drag {
    pub target: Target,
    pub grab: u16,
}

impl Bar {
    pub fn max_offset(&self) -> usize {
        self.content.saturating_sub(self.viewport)
    }

    /// There is something to scroll and room to show it.
    pub fn scrollable(&self) -> bool {
        self.len > 0 && self.max_offset() > 0
    }

    /// The thumb: its first row (from the bar's top) and its height.
    pub fn thumb(&self) -> (u16, u16) {
        if self.len == 0 {
            return (0, 0);
        }
        if self.content == 0 || self.viewport >= self.content {
            return (0, self.len);
        }
        let len = self.len as f64;
        let size = (len * self.viewport as f64 / self.content as f64).round().clamp(1.0, len) as u16;
        let range = self.len - size;
        let max = self.max_offset();
        let start = (self.offset.min(max) as f64 * range as f64 / max as f64).round() as u16;
        (start.min(range), size)
    }

    /// The offset that puts the thumb's top at row `top` of the bar (any row,
    /// even above or below it: the result is clamped to the scroll's range).
    pub fn offset_for(&self, top: i32) -> usize {
        let (_, size) = self.thumb();
        let range = self.len.saturating_sub(size) as i32;
        if range == 0 {
            return self.offset.min(self.max_offset());
        }
        let top = top.clamp(0, range);
        (top as f64 * self.max_offset() as f64 / range as f64).round() as usize
    }

    /// Whether `(x, y)` is on the bar's hit area: its column, the one to its
    /// left, and its rows.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        (x == self.x || x.checked_add(1) == Some(self.x)) && y >= self.y && y < self.y.saturating_add(self.len)
    }

    /// What a click at `(x, y)` hits, when the bar can scroll.
    pub fn hit(&self, x: u16, y: u16) -> Option<Hit> {
        if !self.scrollable() || !self.contains(x, y) {
            return None;
        }
        let row = y - self.y;
        let (start, size) = self.thumb();
        Some(if row < start {
            Hit::Before
        } else if row >= start + size {
            Hit::After
        } else {
            Hit::Thumb { grab: row - start }
        })
    }

    /// The button went down at `(x, y)`: the offset to go to and where the
    /// thumb is held. On the thumb nothing moves and the grab keeps its place;
    /// on the track the thumb centres on the pointer and is held there, so the
    /// same press can go on to drag it.
    pub fn press(&self, x: u16, y: u16) -> Option<(usize, u16)> {
        match self.hit(x, y)? {
            Hit::Thumb { grab } => Some((self.offset.min(self.max_offset()), grab)),
            Hit::Before | Hit::After => {
                let (_, size) = self.thumb();
                let grab = size / 2;
                let row = (y - self.y) as i32;
                Some((self.offset_for(row - grab as i32), grab))
            }
        }
    }

    /// The pointer is at row `y` with the thumb held `grab` rows below its top.
    pub fn drag(&self, y: u16, grab: u16) -> usize {
        self.offset_for(y as i32 - self.y as i32 - grab as i32)
    }
}

/// How a bar looks this frame.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Look {
    /// Under the pointer or held: wider and brighter.
    pub hot: bool,
    /// 0..1: something arrived out of sight.
    pub pulse: f32,
    /// Draw a faint track (else the column is left as it is, e.g. a border).
    pub track: bool,
}

/// The thumb's style and its symbols (the bar's column, the column to its left when hot).
pub fn thumb_style(theme: &Theme, look: Look) -> (Style, &'static str, Option<&'static str>) {
    if theme.mono {
        return if look.hot {
            (Style::default().add_modifier(Modifier::BOLD), "█", Some("▐"))
        } else if look.pulse > 0.5 {
            (Style::default().add_modifier(Modifier::BOLD), "█", None)
        } else {
            (Style::default(), "┃", None)
        };
    }
    if look.hot {
        (Style::default().fg(theme.accent), "█", Some("▐"))
    } else {
        (Style::default().fg(mix(theme.border, theme.accent, look.pulse)), "┃", None)
    }
}

/// Draws `bar` into `buf` (cells outside the buffer are skipped).
pub fn draw(buf: &mut Buffer, bar: &Bar, theme: &Theme, look: Look) {
    if !bar.scrollable() {
        return;
    }
    let (start, size) = bar.thumb();
    let (style, sym, side) = thumb_style(theme, look);
    for row in 0..bar.len {
        let y = bar.y + row;
        let on = row >= start && row < start + size;
        if on {
            if let Some(c) = buf.cell_mut((bar.x, y)) {
                c.set_symbol(sym).set_style(style);
            }
            if let (Some(s), Some(x)) = (side, bar.x.checked_sub(1)) {
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_symbol(s).set_style(style);
                }
            }
        } else if look.track {
            if let Some(c) = buf.cell_mut((bar.x, y)) {
                let track = if look.hot { theme.dim } else { theme.dim2 };
                let st =
                    if theme.mono { Style::default().add_modifier(Modifier::DIM) } else { Style::default().fg(track) };
                c.set_symbol("│").set_style(st);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn bar(len: u16, content: usize, viewport: usize, offset: usize) -> Bar {
        Bar { target: Target::Transcript, x: 10, y: 2, len, content, viewport, offset }
    }

    #[test]
    fn the_thumb_is_the_share_in_view_at_the_share_scrolled() {
        // 100 lines, 20 in view, a bar of 20 rows: a thumb of 4
        assert_eq!(bar(20, 100, 20, 0).thumb(), (0, 4));
        assert_eq!(bar(20, 100, 20, 80).thumb(), (16, 4), "at the end the thumb touches the bottom");
        assert_eq!(bar(20, 100, 20, 40).thumb(), (8, 4));
        // an offset past the end is drawn at the end
        assert_eq!(bar(20, 100, 20, 999).thumb(), (16, 4));
        // huge content still leaves a thumb of one row
        assert_eq!(bar(10, 1_000_000, 10, 0).thumb().1, 1);
    }

    #[test]
    fn content_shorter_than_the_view_does_not_scroll() {
        let b = bar(10, 5, 10, 0);
        assert!(!b.scrollable());
        assert_eq!(b.thumb(), (0, 10));
        assert_eq!(b.hit(10, 3), None);
        assert_eq!(b.press(10, 3), None);
        assert_eq!(b.offset_for(5), 0);
        assert!(!bar(0, 100, 10, 0).scrollable(), "no rows, no bar");
        assert_eq!(bar(0, 100, 10, 0).thumb(), (0, 0));
    }

    #[test]
    fn a_click_hits_the_thumb_or_the_track_on_two_columns() {
        let b = bar(20, 100, 20, 40); // thumb rows 8..12, screen rows 10..14
        assert_eq!(b.hit(10, 10), Some(Hit::Thumb { grab: 0 }));
        assert_eq!(b.hit(10, 13), Some(Hit::Thumb { grab: 3 }));
        assert_eq!(b.hit(9, 11), Some(Hit::Thumb { grab: 1 }), "the column to the left counts");
        assert_eq!(b.hit(10, 2), Some(Hit::Before));
        assert_eq!(b.hit(10, 21), Some(Hit::After));
        assert_eq!(b.hit(8, 11), None, "two columns to the left is the text");
        assert_eq!(b.hit(11, 11), None);
        assert_eq!(b.hit(10, 1), None, "above the bar");
        assert_eq!(b.hit(10, 22), None, "below the bar");
    }

    #[test]
    fn a_bar_at_the_left_edge_does_not_overflow() {
        let b = Bar { x: 0, ..bar(5, 50, 5, 0) };
        assert!(b.contains(0, 2));
        assert!(!b.contains(u16::MAX, 2));
    }

    #[test]
    fn dragging_the_thumb_keeps_the_grab_and_maps_back_to_the_offset() {
        let b = bar(20, 100, 20, 40);
        // held one row into the thumb at row 11 (thumb top 8 + y 2 + 1): no move
        let (off, grab) = b.press(10, 11).unwrap();
        assert_eq!((off, grab), (40, 1));
        assert_eq!(b.drag(11, grab), 40, "pressing and not moving does not jump");
        // every row of thumb travel is max/range = 80/16 = 5 lines
        assert_eq!(b.drag(12, grab), 45);
        assert_eq!(b.drag(9, grab), 30);
        // past the ends: clamped
        assert_eq!(b.drag(0, grab), 0);
        assert_eq!(b.drag(200, grab), 80);
        // round trip: every offset's thumb maps back near it
        for off in 0..=80 {
            let b = bar(20, 100, 20, off);
            let (start, _) = b.thumb();
            let back = b.offset_for(start as i32);
            assert!(back.abs_diff(off) <= 3, "{off} -> {start} -> {back}");
        }
    }

    #[test]
    fn a_click_on_the_track_centres_the_thumb_there() {
        let b = bar(20, 100, 20, 0);
        // the thumb (4 rows) centred on bar row 12: its top at 10, offset 10*5 = 50
        let (off, grab) = b.press(10, 2 + 12).unwrap();
        assert_eq!(grab, 2);
        assert_eq!(off, 50);
        // and the same press, dragged, carries on from there without a jump
        let moved = Bar { offset: off, ..b };
        assert_eq!(moved.drag(14, grab), 50);
        // the track's very end goes to the very end
        assert_eq!(b.press(10, 21).unwrap().0, 80);
        let end = bar(20, 100, 20, 80);
        assert_eq!(end.press(10, 2).unwrap().0, 0);
    }

    #[test]
    fn hot_bars_are_wider_and_brighter_in_colour_and_in_mono() {
        let theme = Theme::nexo();
        let (cold, sym, side) = thumb_style(&theme, Look::default());
        assert_eq!((sym, side), ("┃", None));
        let (hot, sym, side) = thumb_style(&theme, Look { hot: true, ..Look::default() });
        assert_eq!((sym, side), ("█", Some("▐")));
        assert_ne!(cold.fg, hot.fg);
        let mono = Theme::mono();
        let (st, sym, side) = thumb_style(&mono, Look { hot: true, ..Look::default() });
        assert_eq!((sym, side), ("█", Some("▐")));
        assert!(st.add_modifier.contains(Modifier::BOLD));
        // a pulse shows in mono too
        let (_, sym, _) = thumb_style(&mono, Look { pulse: 1.0, ..Look::default() });
        assert_eq!(sym, "█");
    }

    #[test]
    fn drawing_puts_the_thumb_where_hit_testing_finds_it() {
        let theme = Theme::nexo();
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 24));
        let b = bar(20, 100, 20, 40);
        draw(&mut buf, &b, &theme, Look { track: true, ..Look::default() });
        for row in 0..20u16 {
            let sym = buf[(10, 2 + row)].symbol().to_string();
            let thumb = matches!(b.hit(10, 2 + row), Some(Hit::Thumb { .. }));
            assert_eq!(sym == "┃", thumb, "row {row}: {sym}");
            if !thumb {
                assert_eq!(sym, "│");
            }
        }
        // hot: the column to the left gets the half block on the thumb's rows only
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 24));
        draw(&mut buf, &b, &theme, Look { hot: true, ..Look::default() });
        assert_eq!(buf[(9, 10)].symbol(), "▐");
        assert_eq!(buf[(9, 2)].symbol(), " ");
        assert_eq!(buf[(10, 2)].symbol(), " ", "no track asked for: the column is left alone");
        // a bar that does not scroll draws nothing
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 24));
        draw(&mut buf, &bar(20, 5, 20, 0), &theme, Look { track: true, ..Look::default() });
        assert!(crate::ui::rows_of(&buf).iter().all(|r| r.trim().is_empty()));
        // a bar partly outside the buffer is clipped, not a panic
        let mut small = Buffer::empty(Rect::new(0, 0, 5, 5));
        draw(&mut small, &b, &theme, Look { hot: true, track: true, pulse: 0.0 });
    }
}
