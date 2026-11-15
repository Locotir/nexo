//! The context gauge of the status bar.

use super::*;

/// Cells of the status bar's context gauge.
pub const GAUGE_CELLS: usize = 8;

/// The filled part of a gauge of `cells` cells at `share` (0..=1), in eighths of
/// a cell, and how many empty cells follow it: `(fill, empty)`.
pub fn gauge_cells(share: f64, cells: usize) -> (String, usize) {
    const PART: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = (share.clamp(0.0, 1.0) * (cells * 8) as f64).round() as usize;
    let full = eighths / 8;
    let mut fill = "█".repeat(full);
    fill.push_str(PART[eighths % 8]);
    let used = full + usize::from(!eighths.is_multiple_of(8));
    (fill, cells - used)
}

/// A thin line gauge of `cells` cells at `share` (0..=1), in half cells:
/// `(fill, track)`, the fill heavy (`━`, `╸` for a half) and the track the same
/// line in a dim colour, so the bar sits on the text's baseline rather than
/// filling the whole row.
pub fn line_gauge(share: f64, cells: usize) -> (String, String) {
    let halves = (share.clamp(0.0, 1.0) * (cells * 2) as f64).round() as usize;
    let mut fill = "━".repeat(halves / 2);
    if halves % 2 == 1 {
        fill.push('╸');
    }
    let used = halves.div_ceil(2);
    (fill, "━".repeat(cells - used))
}

/// The gauge as coloured pieces: the fill in `colour` on a dim track, brighter while `pulse`.
pub(super) fn gauge_segs(
    theme: &Theme,
    share: f64,
    cells: usize,
    colour: ratatui::style::Color,
    pulse: bool,
) -> Vec<Seg> {
    let track = mix(theme.bar_bg, theme.border, 0.7);
    let fill_colour = if pulse { mix(colour, theme.fg, 0.35) } else { colour };
    let (fill, rest) = line_gauge(share, cells);
    let mut out = vec![Seg { text: fill, style: Style::default().fg(fill_colour) }];
    if !rest.is_empty() {
        out.push(Seg { text: rest, style: Style::default().fg(track) });
    }
    out
}
