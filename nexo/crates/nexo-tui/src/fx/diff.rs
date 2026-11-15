//! Motion of the diff side panel and of the `/diff` viewer. Pure functions of the time asked about, plus the
//! small state of "what changed and when". Colour only, inside cells that are already drawn: the panel opens
//! at its final size and its content fades in from the ground, top to bottom (the transcript beside it is
//! never reflowed per frame).

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use crate::anim::{ease_out, progress};
use crate::app::ChangedFile;
use crate::motion::{fade, within, Counter, Glide};
use crate::theme::Theme;

/// The side panel's content fades in from the ground, top to bottom, in this long.
pub const OPEN_MS: u64 = 180;
/// A file whose counts changed is lit and its figures roll; the light fades back in this long.
pub const CHANGE_MS: u64 = 450;
/// A file that shows up for the first time fades in over this long.
pub const NEW_MS: u64 = 200;
/// The hunk header the viewer jumped to is lit once, this long.
pub const JUMP_MS: u64 = 300;
/// How much of the open fade is spent staggering the rows (the rest is each row's own fade).
const STAGGER: f64 = 0.5;

/// 0..1: how visible row `row` of `rows` is `elapsed_ms` into the open fade.
pub fn reveal(row: u16, rows: u16, elapsed_ms: u64) -> f32 {
    let t = (elapsed_ms as f64 / OPEN_MS as f64).min(1.0);
    let start = if rows == 0 { 0.0 } else { row as f64 / rows as f64 * STAGGER };
    ease_out(((t - start) / (1.0 - STAGGER)).clamp(0.0, 1.0)) as f32
}

/// 1 right after `at`, easing to 0 over `ms`; 0 before `at` is known and after the end.
pub fn lit(at: Option<Instant>, now: Instant, ms: u64) -> f32 {
    if !within(at, now, ms) {
        return 0.0;
    }
    (1.0 - ease_out(progress(at, now, ms))) as f32
}

/// A selected row's style while the highlight glides: the background eases from the ground to the
/// settled one, the text and weight switch half way. Level 1 is exactly `settled`, level 0 the plain row.
pub fn glide_style(theme: &Theme, settled: Style, level: f32) -> Style {
    if level >= 1.0 {
        return settled;
    }
    if level <= 0.0 {
        return Style::default();
    }
    let mut s = Style::default();
    if let Some(bg) = settled.bg {
        s = s.bg(fade(theme, theme.bg, bg, level));
    }
    if level > 0.5 {
        s.fg = settled.fg;
        s = s.add_modifier(settled.add_modifier);
    }
    s
}

/// Fades the cells of `area` that `row_k` says (by row index inside the area) from the ground to what they are.
pub fn fade_row(buf: &mut Buffer, row: Rect, theme: &Theme, k: f32) {
    fade_cells(buf, row, theme, |_| k);
}

fn fade_cells(buf: &mut Buffer, area: Rect, theme: &Theme, row_k: impl Fn(u16) -> f32) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        let k = row_k(y - area.y);
        if k >= 1.0 {
            continue;
        }
        for x in area.left()..area.right() {
            let c = &mut buf[(x, y)];
            let (fg, bg) = (if c.fg == Color::Reset { theme.fg } else { c.fg }, c.bg);
            let bg = if bg == Color::Reset { theme.bg } else { bg };
            c.fg = fade(theme, bg, fg, k);
            c.bg = bg;
        }
    }
}

/// One file of the panel as the effects know it.
#[derive(Debug, Clone)]
struct FileFx {
    path: String,
    last: (usize, usize),
    add: Counter,
    del: Counter,
    shown: (u64, u64),
    changed: Option<Instant>,
    born: Option<Instant>,
}

/// What the panel's files are doing now: the figure to show and how lit the row is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileLook {
    pub added: u64,
    pub removed: u64,
    /// 0..1 light of a changed file, fading to 0.
    pub lit: f32,
    /// 0..1 visibility of a file that just appeared (1 when settled).
    pub born: f32,
}

#[derive(Debug, Default, Clone)]
pub struct PanelFx {
    /// Whether the panel was on screen at the last note (None before the first look).
    on: Option<bool>,
    opened: Option<Instant>,
    files: Vec<FileFx>,
}

impl PanelFx {
    /// Called every frame before the panel is drawn. The files already there when the panel opens are not
    /// news; a path that shows up later fades in, and one whose counts changed is lit and rolls.
    pub fn note(&mut self, shown: bool, changed: &[ChangedFile], now: Instant, animate: bool) {
        let was = self.on.replace(shown);
        if !animate || !shown {
            self.files.clear();
            self.opened = None;
            return;
        }
        let seeded = was == Some(true);
        if !seeded {
            self.files.clear();
            self.opened = was.map(|_| now);
        }
        self.files.retain(|f| changed.iter().any(|c| c.path == f.path));
        for c in changed {
            let cur = (c.added, c.removed);
            let i = match self.files.iter().position(|f| f.path == c.path) {
                Some(i) => i,
                None => {
                    let mut f = FileFx {
                        path: c.path.clone(),
                        last: cur,
                        add: Counter::default(),
                        del: Counter::default(),
                        shown: (cur.0 as u64, cur.1 as u64),
                        changed: None,
                        born: seeded.then_some(now),
                    };
                    f.add.sample(cur.0 as f64, now, false);
                    f.del.sample(cur.1 as f64, now, false);
                    self.files.push(f);
                    self.files.len() - 1
                }
            };
            let f = &mut self.files[i];
            if f.last != cur {
                f.last = cur;
                f.changed = Some(now);
            }
            f.shown = (
                f.add.sample(cur.0 as f64, now, true).0.round() as u64,
                f.del.sample(cur.1 as f64, now, true).0.round() as u64,
            );
        }
    }

    /// The look of `path` at `now`, None while it has no effect (draw it as usual).
    pub fn look(&self, path: &str, now: Instant) -> Option<FileLook> {
        let f = self.files.iter().find(|f| f.path == path)?;
        let lit = lit(f.changed, now, CHANGE_MS);
        let born = if within(f.born, now, NEW_MS) { ease_out(progress(f.born, now, NEW_MS)) as f32 } else { 1.0 };
        if lit == 0.0 && born >= 1.0 {
            return None;
        }
        Some(FileLook { added: f.shown.0, removed: f.shown.1, lit, born })
    }

    /// Whether anything of the panel still moves at `now` (or is about to start).
    pub fn moving(&self, now: Instant) -> bool {
        within(self.opened, now, OPEN_MS)
            || self.files.iter().any(|f| within(f.changed, now, CHANGE_MS) || within(f.born, now, NEW_MS))
    }

    /// The open fade over the panel's cells, after they are drawn.
    pub fn draw_open(&self, buf: &mut Buffer, area: Rect, theme: &Theme, now: Instant) {
        let Some(at) = self.opened.filter(|_| within(self.opened, now, OPEN_MS)) else { return };
        let ms = now.saturating_duration_since(at).as_millis() as u64;
        fade_cells(buf, area, theme, |row| reveal(row, area.height, ms));
    }
}

/// The viewer's motion: the list highlight glides, a hunk jumped to is lit once.
#[derive(Debug, Default, Clone)]
pub struct ViewFx {
    /// The clock and the switch, set by the caller before each draw (off by default: the settled look).
    pub now: Option<Instant>,
    pub on: bool,
    pub sel: Glide,
    seen: bool,
    /// A jump the key asked for, stamped with the time on the next draw.
    pub pending: Option<(usize, usize)>,
    jump: Option<(usize, usize, Instant)>,
}

impl ViewFx {
    pub fn clock(&self) -> Instant {
        self.now.unwrap_or_else(Instant::now)
    }

    /// The selected file of the list; the first look is not a move.
    pub fn note_sel(&mut self, sel: usize) {
        let now = self.clock();
        self.sel.note(Some(sel), now, self.on && self.seen);
        self.seen = true;
    }

    /// Stamps a jump the keys asked for.
    pub fn take_pending(&mut self) {
        let Some((f, h)) = self.pending.take() else { return };
        self.jump = self.on.then(|| (f, h, self.clock()));
    }

    /// 0..1 light on hunk `h` of file `f`.
    pub fn hunk_lit(&self, f: usize, h: usize) -> f32 {
        match self.jump {
            Some((jf, jh, at)) if (jf, jh) == (f, h) => lit(Some(at), self.clock(), JUMP_MS),
            _ => 0.0,
        }
    }

    pub fn moving(&self, now: Instant) -> bool {
        self.pending.is_some()
            || self.sel.moving(now)
            || self.jump.is_some_and(|(_, _, at)| within(Some(at), now, JUMP_MS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn cf(path: &str, a: usize, r: usize) -> ChangedFile {
        ChangedFile { path: path.into(), added: a, removed: r }
    }

    #[test]
    fn reveal_goes_top_down_and_settles() {
        assert_eq!(reveal(0, 10, 0), 0.0);
        assert_eq!(reveal(9, 10, OPEN_MS), 1.0);
        let mid = OPEN_MS / 2;
        assert!(reveal(0, 10, mid) > reveal(9, 10, mid));
        assert!(reveal(0, 10, mid) > 0.0 && reveal(0, 10, mid) < 1.0 + f32::EPSILON);
        assert_eq!(reveal(5, 10, OPEN_MS * 3), 1.0);
    }

    #[test]
    fn lit_fades_from_one_to_nothing() {
        let t0 = Instant::now();
        assert_eq!(lit(None, t0, CHANGE_MS), 0.0);
        assert_eq!(lit(Some(t0), t0, CHANGE_MS), 1.0);
        let mid = lit(Some(t0), at(t0, CHANGE_MS / 2), CHANGE_MS);
        assert!(mid > 0.0 && mid < 1.0);
        assert_eq!(lit(Some(t0), at(t0, CHANGE_MS), CHANGE_MS), 0.0);
    }

    #[test]
    fn glide_style_ends_are_exact() {
        let th = Theme::dark();
        let s = th.highlight();
        assert_eq!(glide_style(&th, s, 1.0), s);
        assert_eq!(glide_style(&th, s, 0.0), Style::default());
        assert_ne!(glide_style(&th, s, 0.3), s);
    }

    #[test]
    fn a_changed_file_is_lit_and_rolls_then_rests() {
        let t0 = Instant::now();
        let mut p = PanelFx::default();
        p.note(true, &[cf("a.rs", 2, 1)], t0, true);
        assert!(p.look("a.rs", t0).is_none(), "the first look is not news");
        assert!(!p.moving(t0));
        p.note(true, &[cf("a.rs", 12, 1)], at(t0, 10), true);
        assert!(p.moving(at(t0, 10)));
        p.note(true, &[cf("a.rs", 12, 1)], at(t0, 100), true);
        let l = p.look("a.rs", at(t0, 100)).unwrap();
        assert!(l.lit > 0.5 && l.added > 2 && l.added < 12, "{l:?}");
        let end = at(t0, 10 + CHANGE_MS);
        p.note(true, &[cf("a.rs", 12, 1)], end, true);
        assert!(p.look("a.rs", end).is_none());
        assert!(!p.moving(end));
    }

    #[test]
    fn a_new_file_fades_in_but_those_at_open_do_not() {
        let t0 = Instant::now();
        let mut p = PanelFx::default();
        p.note(false, &[], t0, true);
        p.note(true, &[cf("a.rs", 1, 0)], at(t0, 5), true);
        assert!(p.look("a.rs", at(t0, 5)).is_none());
        assert!(p.moving(at(t0, 5)), "the panel itself opens");
        p.note(true, &[cf("b.rs", 3, 0), cf("a.rs", 1, 0)], at(t0, 400), true);
        let b = p.look("b.rs", at(t0, 410)).unwrap();
        assert!(b.born < 1.0 && b.lit == 0.0);
        assert!(p.look("b.rs", at(t0, 400 + NEW_MS)).is_none());
        assert!(!p.moving(at(t0, 400 + NEW_MS)));
    }

    #[test]
    fn no_motion_when_off() {
        let t0 = Instant::now();
        let mut p = PanelFx::default();
        p.note(true, &[cf("a.rs", 1, 0)], t0, false);
        p.note(true, &[cf("a.rs", 5, 0), cf("b.rs", 1, 1)], at(t0, 10), false);
        assert!(p.look("a.rs", at(t0, 10)).is_none() && !p.moving(at(t0, 10)));
    }

    #[test]
    fn a_jump_lights_its_hunk_once() {
        let t0 = Instant::now();
        let mut v = ViewFx { now: Some(t0), on: true, ..Default::default() };
        v.pending = Some((0, 2));
        assert!(v.moving(t0));
        v.take_pending();
        assert_eq!(v.hunk_lit(0, 2), 1.0);
        assert_eq!(v.hunk_lit(0, 1), 0.0);
        v.now = Some(at(t0, JUMP_MS / 2));
        assert!(v.hunk_lit(0, 2) > 0.0 && v.hunk_lit(0, 2) < 1.0);
        v.now = Some(at(t0, JUMP_MS));
        assert_eq!(v.hunk_lit(0, 2), 0.0);
        assert!(!v.moving(at(t0, JUMP_MS)));
    }
}
