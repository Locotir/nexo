//! The motion of a reply while it streams and of the blocks around it: fresh ink at the leading edge of
//! the text, the breathing caret, the thinking header settling, a failed card's flash, the lit note of a
//! finished card, the person's message fading in and the rail of an interrupted block. Pure functions of the
//! time they are asked about; the caller injects `now`. Colour only: nothing here changes a line count or
//! moves a cell.

use std::time::Instant;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::anim::ease_out;
use crate::motion::{fade, lit_style};
use crate::theme::Theme;

/// Text that arrived this recently is drawn dimmer and eases to its colour.
pub const INK_MS: u64 = 150;
/// How dim a letter is the moment it lands (0 = the dim colour, 1 = its own colour).
const INK_FLOOR: f32 = 0.55;
/// The thinking header settles from its active colour to dim in this long.
pub const THINK_MS: u64 = 300;
/// A failed card's mark flashes twice in this long.
pub const FAIL_MS: u64 = 360;
/// The note at the end of a finished card's head lights up once in this long.
pub const NOTE_MS: u64 = 300;
/// The person's message fades in from the input background in this long.
pub const SEND_MS: u64 = 200;
/// The rail of an interrupted block turns to the warning colour and back in this long.
pub const STOP_MS: u64 = 300;
/// One breath of the caret, in spinner beats.
const BREATH: usize = 8;

/// The text that arrived lately: when and how many letters.
#[derive(Debug, Default, Clone)]
pub struct Ink {
    deltas: Vec<(Instant, usize)>,
}

/// Letters counted from the end of the text up to `upto` (exclusive) share one `level` (0..1 of the way
/// from dim to their colour).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seg {
    pub upto: usize,
    pub level: f32,
}

impl Ink {
    /// A delta of `chars` letters arrived at `now`.
    pub fn note(&mut self, now: Instant, chars: usize) {
        self.deltas.retain(|(t, _)| within_ms(*t, now, INK_MS));
        if chars > 0 {
            self.deltas.push((now, chars));
        }
    }

    /// Whether any letter is still settling.
    pub fn alive(&self, now: Instant) -> bool {
        self.deltas.iter().any(|(t, _)| within_ms(*t, now, INK_MS))
    }

    pub fn clear(&mut self) {
        self.deltas.clear();
    }

    /// The newest letters first, each delta with how settled it is. Empty when nothing is fresh.
    pub fn segments(&self, now: Instant) -> Vec<Seg> {
        let mut upto = 0;
        let mut out = Vec::new();
        for (t, n) in self.deltas.iter().rev() {
            let age = now.saturating_duration_since(*t).as_millis() as u64;
            if age >= INK_MS {
                break;
            }
            upto += n;
            out.push(Seg { upto, level: ease_out(age as f64 / INK_MS as f64) as f32 });
        }
        out
    }
}

fn within_ms(at: Instant, now: Instant, ms: u64) -> bool {
    (now.saturating_duration_since(at).as_millis() as u64) < ms
}

/// 0..1 through an effect of `ms` that began `since_ms` ago (1 once over).
pub fn progress(since_ms: u64, ms: u64) -> f32 {
    (since_ms as f32 / ms.max(1) as f32).min(1.0)
}

/// Whether `span` ends a streaming line without being text: the caret and blanks.
fn is_tail_mark(span: &Span<'_>) -> bool {
    let c = span.content.as_ref();
    c == "▍" || c.trim().is_empty()
}

/// Dims the letters of `line` that `segs` call fresh, counted from its end. Spans before `head` (the
/// block's marker) are left alone. `stamp` is true when the renderer put a time stamp as the last
/// span (it knows; the text is never guessed from). Cost is the size of the fresh tail.
pub fn ink_tail(line: &mut Line<'static>, segs: &[Seg], theme: &Theme, head: usize, stamp: bool) {
    let Some(total) = segs.last().map(|s| s.upto) else { return };
    let mut end = line.spans.len();
    if stamp && end > head {
        end -= 1;
    }
    while end > head && is_tail_mark(&line.spans[end - 1]) {
        end -= 1;
    }
    let base = line.style.fg.unwrap_or(theme.fg);
    let mut consumed = 0;
    let mut k = end;
    while k > head && consumed < total {
        k -= 1;
        let span = &line.spans[k];
        let n = span.content.chars().count();
        // letters of this span that are fresh: the last `fresh`
        let fresh = n.min(total - consumed);
        let skip = n - fresh;
        let fg = span.style.fg.unwrap_or(base);
        let style = span.style;
        let mut pieces: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut run_level: Option<usize> = None;
        let flush = |run: &mut String, lvl: Option<usize>, pieces: &mut Vec<Span<'static>>| {
            if run.is_empty() {
                return;
            }
            let text = std::mem::take(run);
            let st = match lvl {
                Some(si) => style.fg(ink_colour(theme, fg, segs[si].level)),
                None => style,
            };
            pieces.push(Span::styled(text, st));
        };
        for (idx, ch) in span.content.chars().enumerate() {
            let lvl = (idx >= skip).then(|| {
                let d = consumed + (n - 1 - idx);
                segs.iter().position(|s| d < s.upto).unwrap_or(segs.len() - 1)
            });
            if lvl != run_level {
                flush(&mut run, run_level, &mut pieces);
                run_level = lvl;
            }
            run.push(ch);
        }
        flush(&mut run, run_level, &mut pieces);
        consumed += n;
        line.spans.splice(k..k + 1, pieces);
    }
}

/// The colour of a letter `level` (0..1) of the way from freshly landed to settled.
pub fn ink_colour(theme: &Theme, fg: Color, level: f32) -> Color {
    fade(theme, theme.dim2, fg, INK_FLOOR + (1.0 - INK_FLOOR) * level)
}

/// The caret at the end of the streaming text: it breathes between the dim and the accent colour on the
/// spinner beat.
pub fn caret_style(theme: &Theme, spinner: usize) -> Style {
    let p = (spinner % BREATH) as f32 / BREATH as f32;
    let tri = 1.0 - (2.0 * p - 1.0).abs();
    theme.style(fade(theme, theme.dim2, theme.accent, tri))
}

/// The colour of the thinking header while the model is thinking.
pub fn think_active(theme: &Theme) -> Color {
    fade(theme, theme.dim, theme.fg, 0.7)
}

/// The thinking header `since_ms` after the reasoning closed: from active to dim.
pub fn think_colour(theme: &Theme, since_ms: u64) -> Color {
    fade(theme, think_active(theme), theme.dim, ease_out(progress(since_ms, THINK_MS) as f64) as f32)
}

/// Two beats over `FAIL_MS`: 1 lit, 0 dim, 1, 0, and settled lit.
pub fn fail_level(since_ms: u64) -> f32 {
    let t = progress(since_ms, FAIL_MS);
    0.5 * (1.0 + (4.0 * std::f32::consts::PI * t).cos())
}

/// The mark of a failed card `since_ms` after it failed.
pub fn fail_colour(theme: &Theme, since_ms: u64) -> Color {
    let dim = fade(theme, theme.bg, theme.error, 0.35);
    fade(theme, dim, theme.error, fail_level(since_ms))
}

/// 0..1..0 once over `ms` (a hump); 0 when over.
pub fn hump(since_ms: u64, ms: u64) -> f32 {
    if since_ms >= ms {
        return 0.0;
    }
    (std::f32::consts::PI * progress(since_ms, ms)).sin()
}

/// Lights the note spans of a finished card's head (`· 3 lines`, `  1.2s`): dim spans after the mark that
/// start with the separator and are not the chevron.
pub fn light_note(head: &mut Line<'static>, theme: &Theme, since_ms: u64) {
    let level = hump(since_ms, NOTE_MS);
    if level <= 0.0 {
        return;
    }
    for s in head.spans.iter_mut().skip(1) {
        if is_note(&s.content) && s.style.fg == Some(theme.dim) {
            s.style = lit_style(theme, s.style, level);
        }
    }
}

fn is_note(c: &str) -> bool {
    if c.starts_with(" · ") {
        return true;
    }
    c.starts_with("  ") && c.trim().chars().next().is_some_and(|ch| ch.is_ascii_digit())
}

/// The person's message `t` (0..1) of the way from the input's background to its own band.
pub fn band_in(mut line: Line<'static>, theme: &Theme, t: f32) -> Line<'static> {
    let band = theme.user_band().bg;
    let Some(to) = band else { return line };
    let eased = ease_out(t as f64) as f32;
    let now = fade(theme, theme.bg, to, eased);
    if line.style.bg == Some(to) {
        line.style.bg = Some(now);
    }
    for s in &mut line.spans {
        if s.style.bg == Some(to) {
            s.style.bg = Some(now);
        }
    }
    line
}

/// Recolours the foreground of `span` towards `warn` by `level`.
pub fn warm(span: &mut Span<'static>, theme: &Theme, level: f32) {
    if level <= 0.0 {
        return;
    }
    let from = span.style.fg.unwrap_or(theme.fg);
    span.style.fg = Some(fade(theme, from, theme.warn, level));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn th() -> Theme {
        Theme::dark()
    }

    #[test]
    fn ink_is_fresh_then_settles_and_rests() {
        let t0 = Instant::now();
        let mut ink = Ink::default();
        ink.note(t0, 5);
        assert!(ink.alive(t0));
        let at = |ms| t0 + Duration::from_millis(ms);
        let s0 = ink.segments(t0);
        assert_eq!(s0.len(), 1);
        assert!(s0[0].level < 0.01);
        let mid = ink.segments(at(75))[0].level;
        assert!(mid > 0.3 && mid < 1.0, "{mid}");
        assert!(ink.segments(at(INK_MS)).is_empty());
        assert!(!ink.alive(at(INK_MS)));
    }

    #[test]
    fn newest_letters_are_the_dimmest() {
        let t0 = Instant::now();
        let mut ink = Ink::default();
        ink.note(t0, 3);
        ink.note(t0 + Duration::from_millis(100), 2);
        let segs = ink.segments(t0 + Duration::from_millis(100));
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].upto, 2);
        assert_eq!(segs[1].upto, 5);
        assert!(segs[0].level < segs[1].level);
    }

    #[test]
    fn ink_dims_only_the_tail_and_keeps_the_text() {
        let th = th();
        let mut line = Line::from(vec![Span::raw("◆ "), Span::raw("hello world"), Span::raw("▍")]);
        let segs = [Seg { upto: 5, level: 0.0 }];
        ink_tail(&mut line, &segs, &th, 1, false);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "◆ hello world▍");
        // the marker and the settled part keep their style; the last five letters are dimmer
        assert_eq!(line.spans[0].style.fg, None);
        assert_eq!(line.spans[1].content, "hello ");
        assert_eq!(line.spans[1].style.fg, None);
        assert_eq!(line.spans[2].content, "world");
        assert!(line.spans[2].style.fg.is_some());
        assert_eq!(line.spans[3].content, "▍");
    }

    #[test]
    fn a_stamp_is_skipped_by_the_flag_not_by_its_look() {
        let th = th();
        let segs = [Seg { upto: 99, level: 0.5 }];
        let mk = || Line::from(vec![Span::raw("◆ "), Span::raw("OK: sure"), Span::raw("   "), Span::raw("12:34")]);
        let mut with = mk();
        ink_tail(&mut with, &segs, &th, 1, true);
        assert!(with.spans[1].style.fg.is_some(), "text that looks like a stamp is still ink");
        assert_eq!(with.spans.last().unwrap().style.fg, None, "the stamp keeps its colour");
        // no stamp on the line: the last span is text, whatever it looks like
        let mut without = Line::from(vec![Span::raw("◆ "), Span::raw("OK: sure")]);
        ink_tail(&mut without, &segs, &th, 1, false);
        assert!(without.spans[1].style.fg.is_some());
    }

    #[test]
    fn ink_crosses_spans_and_stops_at_the_head() {
        let th = th();
        let mut line = Line::from(vec![Span::raw("◆ "), Span::raw("ab"), Span::raw("cd")]);
        let segs = [Seg { upto: 99, level: 0.5 }];
        ink_tail(&mut line, &segs, &th, 1, false);
        assert_eq!(line.spans[0].style.fg, None);
        assert!(line.spans[1].style.fg.is_some() && line.spans[2].style.fg.is_some());
    }

    #[test]
    fn ink_colour_runs_from_dim_to_the_colour() {
        let th = th();
        let fg = Color::Rgb(200, 200, 200);
        assert_eq!(ink_colour(&th, fg, 1.0), fg);
        assert_ne!(ink_colour(&th, fg, 0.0), fg);
    }

    #[test]
    fn think_header_settles_to_dim() {
        let th = th();
        assert_eq!(think_colour(&th, 0), think_active(&th));
        assert_eq!(think_colour(&th, THINK_MS), th.dim);
        assert_eq!(think_colour(&th, THINK_MS * 3), th.dim);
    }

    #[test]
    fn failure_flashes_twice_and_settles_lit() {
        assert!((fail_level(0) - 1.0).abs() < 1e-3);
        assert!(fail_level(FAIL_MS / 4) < 0.05);
        assert!(fail_level(FAIL_MS / 2) > 0.95);
        assert!(fail_level(FAIL_MS * 3 / 4) < 0.05);
        assert!((fail_level(FAIL_MS) - 1.0).abs() < 1e-3);
        let th = th();
        assert_eq!(fail_colour(&th, FAIL_MS), th.error);
    }

    #[test]
    fn hump_rises_and_falls() {
        assert_eq!(hump(0, 300), 0.0);
        assert!(hump(150, 300) > 0.99);
        assert_eq!(hump(300, 300), 0.0);
    }

    #[test]
    fn the_note_lights_and_rests() {
        let th = th();
        let mk = || Line::from(vec![Span::raw("✓ "), Span::raw("read a.rs"), Span::styled(" · 3 lines", th.dim())]);
        let mut mid = mk();
        light_note(&mut mid, &th, NOTE_MS / 2);
        assert_ne!(mid.spans[2].style.bg, mk().spans[2].style.bg);
        assert_eq!(mid.spans[1].style, mk().spans[1].style);
        let mut end = mk();
        light_note(&mut end, &th, NOTE_MS);
        assert_eq!(end.spans, mk().spans);
    }

    #[test]
    fn the_band_fades_in_to_its_own_colour() {
        let th = th();
        let band = th.user_band();
        let mk = || Line::from(Span::styled("hi", band)).style(band);
        let end = band_in(mk(), &th, 1.0);
        assert_eq!(end.style.bg, band.bg);
        let start = band_in(mk(), &th, 0.0);
        assert_eq!(start.style.bg, Some(th.bg));
    }

    #[test]
    fn caret_breathes_on_the_beat() {
        let th = th();
        assert_ne!(caret_style(&th, 0), caret_style(&th, 4));
        assert_eq!(caret_style(&th, 0), caret_style(&th, BREATH));
    }
}
