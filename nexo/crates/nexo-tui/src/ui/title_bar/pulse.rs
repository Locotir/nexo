//! One-shot ink on the finished layout. Both passes use already-scheduled frames and leave the
//! glyphs, backgrounds, hover styles and hit rectangles alone, including on transparent themes.

use std::{ops::Range, time::Duration};

use ratatui::{buffer::Buffer, layout::Rect};

use crate::{fx_condense, motion, theme::Theme};

/// A soft ripple spreads from the minus sign when the map's light lands. Only the nearby cells
/// of the label participate, no matter how wide the terminal or the policy name.
pub(super) fn landing(buf: &mut Buffer, theme: &Theme, y: u16, cols: Range<u16>, sign: u16, ms: u64) {
    const REACH: u16 = 7;
    let Some(elapsed) = ms.checked_sub(fx_condense::LIGHT_TO_MS).filter(|&t| t < motion::ROLL_MS) else { return };
    let p = elapsed as f32 / motion::ROLL_MS as f32;
    let radius = p * (REACH + 1) as f32;
    let envelope = (p * 10.0).min(1.0) * ((1.0 - p) * 3.0).min(1.0);
    for x in cols.start.max(sign.saturating_sub(REACH))..cols.end.min(sign.saturating_add(REACH + 1)) {
        let d = (x.abs_diff(sign) as f32 - radius).abs();
        let k = (1.0 - d / 2.25).max(0.0) * envelope;
        if k > 0.0 {
            let cell = &mut buf[(x, y)];
            cell.fg = motion::fade(theme, cell.fg, theme.bar_fg, 0.85 * k);
        }
    }
}

/// On a successful turn, a green tip with an accent-coloured tail signs the four brand letters
/// from left to right. It rides the same clock as the turn-end glow on the separator.
pub(super) fn signature(buf: &mut Buffer, theme: &Theme, area: Rect, elapsed: Duration) {
    if elapsed >= Duration::from_millis(motion::GLOW_MS) {
        return;
    }
    let p = elapsed.as_secs_f32() / (motion::GLOW_MS as f32 * 0.001);
    let head = -1.0 + p * 6.5;
    for (col, x) in (area.left().saturating_add(1)..area.right()).take(4).enumerate() {
        let d = head - col as f32;
        let k = if d < 0.0 { 1.0 + d } else { 1.0 - d / 2.5 };
        if k > 0.0 {
            let cell = &mut buf[(x, area.y)];
            let ink = motion::fade(theme, theme.success, theme.accent, (d / 1.5).clamp(0.0, 1.0));
            cell.fg = motion::fade(theme, cell.fg, ink, k * 0.80);
        }
    }
}

#[cfg(test)]
mod tests;
