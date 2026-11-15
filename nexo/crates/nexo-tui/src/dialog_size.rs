//! A dialog keeps the size it took when it opened, for its whole life.
//!
//! The size a dialog wants is worked out from its content, and the content moves with
//! the cursor (a row's hint, a description under the chosen option, the rows a filter
//! leaves). Drawn from that every frame, the box would stretch and shrink as the
//! selection moves. [`Slot::keep`] remembers the first size under the dialog's key
//! and hands it back on every later frame; what does not fit is cut or scrolled inside.
//! It is worked out again only when the terminal is resized or the dialog is replaced
//! (another key).

use std::hash::{Hash, Hasher};

use ratatui::layout::Rect;

use crate::modal::Modal;

/// One dialog's remembered size.
#[derive(Debug, Clone, Default)]
pub struct Slot {
    kept: Option<Kept>,
    /// The rectangle of the last frame, as drawn (a pop-open frame included).
    pub drawn: Rect,
}

#[derive(Debug, Clone, Copy)]
struct Kept {
    key: u64,
    screen: (u16, u16),
    w: u16,
    h: u16,
}

impl Slot {
    /// The size to draw `key` at on a terminal of `screen`: the one first asked for
    /// (`want`, a closure so a kept size costs no layout work), clamped to `room`.
    pub fn keep(&mut self, key: u64, screen: Rect, room: Rect, want: impl FnOnce() -> (u16, u16)) -> (u16, u16) {
        let screen = (screen.width, screen.height);
        let k = match self.kept {
            Some(k) if k.key == key && k.screen == screen => k,
            _ => {
                let (w, h) = want();
                let k = Kept { key, screen, w, h };
                self.kept = Some(k);
                k
            }
        };
        (k.w.min(room.width), k.h.min(room.height))
    }

    /// Works the size out again at the next frame (the content changed under the same dialog).
    pub fn remeasure(&mut self) {
        self.kept = None;
    }

    /// The dialog went away: the next one starts afresh.
    pub fn forget(&mut self) {
        self.kept = None;
        self.drawn = Rect::default();
    }
}

/// The narrowest a window is drawn, so a nearly empty one still looks like a window.
pub const MIN_WINDOW_W: u16 = 44;

/// The widest a window may be on a terminal `screen_w` columns wide: the screen less two columns of
/// margin on each side, never more than `asked` (110 unless the window has a long line to show: the
/// to-do and goal windows ask for up to two thirds of a wide screen).
pub fn max_width(screen_w: u16, asked: u16) -> u16 {
    let room = screen_w.saturating_sub(4);
    let widest = 110.max(asked.min(screen_w / 3 * 2));
    room.min(widest)
}

/// The width a window with these lines is drawn at: what the longest line needs plus the frame
/// and a column of air on each side, between [`MIN_WINDOW_W`] and [`max_width`]. This is the one
/// size rule of every window.
pub fn fit_width<'a>(screen_w: u16, asked: u16, lines: impl Iterator<Item = &'a ratatui::text::Line<'a>>) -> u16 {
    let max = max_width(screen_w, asked);
    let longest = lines.map(content_width).max().unwrap_or(0) as u16;
    let want = longest.saturating_add(4).clamp(MIN_WINDOW_W.min(max), max);
    // text that wrapped at the widest layout stops a few columns short of it: that is a full window
    if want + 4 >= max {
        max
    } else {
        want
    }
}

/// A line's width without trailing blanks, except where the blanks carry a background (a selected row).
fn content_width(l: &ratatui::text::Line<'_>) -> usize {
    let total: usize = l.spans.iter().map(|s| crate::render::wrap::width(&s.content)).sum();
    let mut trim = 0;
    for s in l.spans.iter().rev() {
        if s.style.bg.is_some() {
            break;
        }
        let t = s.content.trim_end();
        trim += crate::render::wrap::width(&s.content) - crate::render::wrap::width(t);
        if !t.is_empty() {
            break;
        }
    }
    total - trim
}

/// The remembered sizes of the palette and of the dialog on top.
#[derive(Debug, Clone, Default)]
pub struct DialogSize {
    pub modal: Slot,
    pub palette: Slot,
}

/// What makes a dialog the same dialog from frame to frame: its kind and the thing it
/// is about (a permission's request, a question's id, a list's title), not its cursor,
/// its filter or its scroll.
pub fn modal_key(m: &Modal) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match m {
        Modal::Permission(p) => (0u8, &p.req.id, p.rules_open).hash(&mut h),
        Modal::Question(a) => (1u8, &a.q.id, a.q.plan.is_some()).hash(&mut h),
        Modal::Picker(p) => (2u8, &p.title, format!("{:?}", p.kind)).hash(&mut h),
        Modal::Help { .. } => 3u8.hash(&mut h),
        Modal::Setup(s) => (4u8, format!("{:?}", s.step)).hash(&mut h),
        Modal::Info(_) => 5u8.hash(&mut h),
        Modal::Agents(v) => (6u8, v.size_key()).hash(&mut h),
        Modal::Jobs(v) => v.size_hash(&mut h),
        Modal::Todo(v) => v.size_hash(&mut h),
        Modal::Mcp(v) => v.size_hash(&mut h),
        Modal::Goal(v) => v.size_hash(&mut h),
        Modal::Sent(v) => v.size_hash(&mut h),
        Modal::Trust(_) => 10u8.hash(&mut h),
        Modal::Chrome(v) => v.size_hash(&mut h),
        Modal::Team(v) => v.size_hash(&mut h),
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_size_stays_until_the_key_or_the_screen_changes() {
        let mut s = Slot::default();
        let screen = Rect::new(0, 0, 100, 40);
        assert_eq!(s.keep(1, screen, screen, || (50, 10)), (50, 10));
        assert_eq!(s.keep(1, screen, screen, || (80, 20)), (50, 10), "same dialog: the first size");
        assert_eq!(s.keep(2, screen, screen, || (80, 20)), (80, 20), "replaced");
        let small = Rect::new(0, 0, 60, 30);
        assert_eq!(s.keep(2, small, small, || (60, 12)), (60, 12), "resized");
    }

    #[test]
    fn a_window_is_as_wide_as_its_content_between_a_floor_and_a_share_of_the_screen() {
        use ratatui::text::Line;
        let short = [Line::from("No jobs"), Line::from("Esc close      ")];
        assert_eq!(fit_width(170, 0, short.iter()), MIN_WINDOW_W);
        let long = [Line::from("x".repeat(200))];
        assert_eq!(fit_width(170, 0, long.iter()), 110);
        assert_eq!(fit_width(80, 0, long.iter()), 76);
        assert_eq!(fit_width(240, 150, long.iter()), 150, "a long line asks for more on a wide screen");
        let mid = [Line::from("y".repeat(60))];
        assert_eq!(fit_width(170, 0, mid.iter()), 64);
        assert_eq!(max_width(30, 0), 26);
    }

    #[test]
    fn a_kept_size_is_clamped_to_the_room_without_being_forgotten() {
        let mut s = Slot::default();
        let screen = Rect::new(0, 0, 100, 40);
        s.keep(1, screen, screen, || (90, 30));
        let tight = Rect::new(0, 0, 70, 12);
        assert_eq!(s.keep(1, screen, tight, || unreachable!()), (70, 12));
        assert_eq!(s.keep(1, screen, screen, || unreachable!()), (90, 30));
    }
}
