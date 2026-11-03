//! The status bar's context gauge in three parts: the cached prefix of the last call, what it
//! sent fresh, and what the curator kept out of the view, against the window (or the whole
//! history when that is larger).

use ratatui::style::Style;

use crate::theme::{mix, Theme};
use crate::ui::Seg;

/// Tokens of the last call by where they stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Split {
    pub cached: u64,
    pub fresh: u64,
    /// In the history, left out of the view.
    pub out: u64,
}

/// How many of `cells` each part takes: `(cached, fresh, out)`. A part with tokens gets at least one
/// cell while there is room; the three never add up to more than `cells`.
pub fn cells_of(s: Split, den: u64, cells: usize) -> (usize, usize, usize) {
    let den = den.max(s.cached + s.fresh + s.out).max(1);
    let cell = |n: u64| ((n as f64 / den as f64) * cells as f64).round() as usize;
    let mut c = [cell(s.cached), cell(s.fresh), cell(s.out)];
    for (i, n) in [s.cached, s.fresh, s.out].into_iter().enumerate() {
        if n > 0 && c[i] == 0 {
            c[i] = 1;
        }
    }
    while c.iter().sum::<usize>() > cells {
        let big = (0..3).max_by_key(|i| c[*i]).unwrap_or(0);
        c[big] -= 1;
    }
    (c[0], c[1], c[2])
}

/// The gauge as coloured pieces of exactly `cells` cells: cached `━` in the cool token, fresh `━`
/// in `colour` (the level colour), out `─` dim, the rest the track.
pub fn stacked(theme: &Theme, s: Split, den: u64, cells: usize, colour: ratatui::style::Color) -> Vec<Seg> {
    let (c, f, o) = cells_of(s, den, cells);
    let track = mix(theme.bar_bg, theme.border, 0.7);
    let rest = cells - c - f - o;
    let parts = [("━", c, theme.info), ("━", f, colour), ("─", o, theme.dim), ("━", rest, track)];
    parts
        .into_iter()
        .filter(|(_, n, _)| *n > 0)
        .map(|(ch, n, col)| Seg { text: ch.repeat(n), style: Style::default().fg(col) })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_fill_at_most_the_gauge() {
        let (c, f, o) = cells_of(Split { cached: 60, fresh: 30, out: 10 }, 100, 8);
        assert_eq!(c + f + o, 8);
        assert!(c > f && f >= o);
    }

    #[test]
    fn small_parts_stay_visible() {
        let (c, f, o) = cells_of(Split { cached: 1000, fresh: 1, out: 1 }, 100_000, 8);
        assert_eq!((c, f, o), (1, 1, 1));
    }

    #[test]
    fn history_larger_than_window_scales() {
        let (c, f, o) = cells_of(Split { cached: 50, fresh: 50, out: 100 }, 100, 8);
        assert_eq!((c, f, o), (2, 2, 4));
    }

    #[test]
    fn stacked_is_always_exactly_the_width() {
        let t = Theme::dark();
        for s in [Split::default(), Split { cached: 5, fresh: 5, out: 5 }, Split { cached: 99, fresh: 1, out: 0 }] {
            let segs = stacked(&t, s, 100, 8, t.success);
            let n: usize = segs.iter().map(|g| g.text.chars().count()).sum();
            assert_eq!(n, 8);
        }
    }
}
