//! Toasts: small confirmations (copied, saved, an error) stacked at the top right of the
//! transcript. A toast slides in from the right, stays about three seconds and fades out.
//! Time is always passed in, so the frames are deterministic.

use std::collections::VecDeque;
use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::anim::ease_out;
use crate::motion::fade;
use crate::render::wrap::truncate;
use crate::theme::Theme;

pub const MAX: usize = 3;
pub const SLIDE_MS: u64 = 150;
pub const LIFE_MS: u64 = 3000;
pub const FADE_MS: u64 = 400;
/// Below this width only the newest toast shows.
pub const NARROW: u16 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: Kind,
    pub text: String,
    pub at: Instant,
    /// How many times the same text arrived while it was up (shown as `x2`).
    pub count: u32,
}

#[derive(Debug, Default, Clone)]
pub struct Toasts {
    items: VecDeque<Toast>,
}

impl Toasts {
    pub fn push(&mut self, kind: Kind, text: &str, now: Instant) {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if let Some(t) = self.items.iter_mut().find(|t| t.kind == kind && t.text == text) {
            t.count = t.count.saturating_add(1);
            t.at = now;
            return;
        }
        self.items.push_back(Toast { kind, text, at: now, count: 1 });
        while self.items.len() > MAX {
            self.items.pop_front();
        }
    }

    /// Drops the toasts whose time is up.
    pub fn prune(&mut self, now: Instant) {
        self.items.retain(|t| (now.saturating_duration_since(t.at).as_millis() as u64) < LIFE_MS);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether anything still slides, stays or fades (the screen is drawn every frame).
    pub fn alive(&self, now: Instant) -> bool {
        self.items.iter().any(|t| (now.saturating_duration_since(t.at).as_millis() as u64) < LIFE_MS)
    }

    pub fn texts(&self) -> Vec<&str> {
        self.items.iter().map(|t| t.text.as_str()).collect()
    }

    /// The toasts, oldest first (for `/toasts`).
    pub fn iter(&self) -> impl Iterator<Item = &Toast> {
        self.items.iter()
    }

    /// The label of a toast: its text, then `x2` when it was repeated.
    pub fn label(t: &Toast) -> String {
        if t.count > 1 {
            format!("{} x{}", t.text, t.count)
        } else {
            t.text.clone()
        }
    }
}

/// 0..1: how far a toast of age `ms` has slid in (1 with the motion off).
pub fn slide(ms: u64, animate: bool) -> f64 {
    if !animate {
        return 1.0;
    }
    ease_out(ms as f64 / SLIDE_MS as f64)
}

/// 0..1: how much of its colour a toast of age `ms` still has (1 with the motion off).
pub fn opacity(ms: u64, animate: bool) -> f32 {
    if !animate || ms + FADE_MS < LIFE_MS {
        return 1.0;
    }
    (LIFE_MS.saturating_sub(ms) as f32 / FADE_MS as f32).clamp(0.0, 1.0)
}

/// Draws the stack in the top right corner of `area` (the transcript: never the input box).
pub fn draw(f: &mut Frame, toasts: &Toasts, theme: &Theme, area: Rect, now: Instant, animate: bool) {
    if toasts.items.is_empty() || area.width < 12 || area.height == 0 {
        return;
    }
    let narrow = area.width < NARROW;
    let max_rows = if narrow { 1 } else { MAX.min(area.height as usize) };
    let shown: Vec<&Toast> = toasts.items.iter().rev().take(max_rows).collect();
    let buf = f.buffer_mut();
    for (row, t) in shown.iter().rev().enumerate() {
        let ms = now.saturating_duration_since(t.at).as_millis() as u64;
        if ms >= LIFE_MS {
            continue;
        }
        let (icon, colour) = match t.kind {
            Kind::Info => ("i", theme.info),
            Kind::Success => ("ok", theme.success),
            Kind::Error => ("!", theme.error),
        };
        let room = area.width.saturating_sub(2) as usize;
        let text = Toasts::label(t);
        let body = truncate(&text, room.saturating_sub(5).max(1));
        let label = format!(" {icon} {body} ");
        let w = (UnicodeWidthStr::width(label.as_str()) as u16).min(area.width);
        let off = ((1.0 - slide(ms, animate)) * w as f64).round() as u16;
        let x = area.x + area.width - w + off;
        let y = area.y + row as u16;
        let k = opacity(ms, animate);
        let bg = theme.modal_bg;
        let style = Style::default().fg(fade(theme, bg, colour, k)).bg(bg).add_modifier(if theme.mono {
            Modifier::REVERSED
        } else {
            Modifier::empty()
        });
        let right = area.x + area.width;
        let mut cx = x;
        for ch in label.chars() {
            let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
            if cx + cw > right {
                break;
            }
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_char(ch).set_style(style);
            }
            cx += cw;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn the_stack_keeps_three_and_toasts_expire() {
        let t0 = Instant::now();
        let mut s = Toasts::default();
        for i in 0..5 {
            s.push(Kind::Info, &format!("n{i}"), at(t0, i * 10));
        }
        assert_eq!(s.texts(), ["n2", "n3", "n4"]);
        s.prune(at(t0, LIFE_MS + 35));
        assert_eq!(s.texts(), ["n4"]);
        assert!(s.alive(at(t0, 100)));
        s.prune(at(t0, 10_000));
        assert!(s.is_empty() && !s.alive(at(t0, 10_000)));
    }

    #[test]
    fn slide_and_fade_maths() {
        assert_eq!(slide(0, true), 0.0);
        assert_eq!(slide(SLIDE_MS, true), 1.0);
        assert_eq!(slide(0, false), 1.0);
        assert_eq!(opacity(1000, true), 1.0);
        assert!(opacity(LIFE_MS - 100, true) < 0.5);
        assert_eq!(opacity(LIFE_MS - 100, false), 1.0);
    }
}

#[cfg(test)]
mod stack_tests {
    use super::*;

    #[test]
    fn a_repeated_text_stacks_as_x2_instead_of_a_new_row() {
        let t0 = Instant::now();
        let mut s = Toasts::default();
        s.push(Kind::Success, "copied", t0);
        s.push(Kind::Success, "copied", t0 + std::time::Duration::from_millis(500));
        s.push(Kind::Error, "copied", t0);
        assert_eq!(s.len(), 2);
        let labels: Vec<String> = s.iter().map(Toasts::label).collect();
        assert_eq!(labels, ["copied x2", "copied"]);
        // the repeat restarts the clock
        assert!(s.alive(t0 + std::time::Duration::from_millis(LIFE_MS + 200)));
    }
}
