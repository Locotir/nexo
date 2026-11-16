//! The curator lets context go, shown at a glance: the cells of the context map that it cut condense a step at
//! a time (`█ ▓ ▒ ░ ·`, oldest first, so the wave ends at the newest), a light carries the freed space out of
//! the map, down the panel's border and along the rule to the curator's label, and the label's figure counts
//! up to what was let go as the light lands. Only glyphs of the map and colours change: no text moves, no row
//! is added, and once it is over every cell is the settled one.
//!
//! Every frame is a pure function of the time since the cut, so a test can ask for any of them. With
//! `[ui] animations = false` none of it runs: the map, the rule and the label show their end state at once.

use nexo_core::CellState;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

use crate::motion::{ease_in_out, fade, fixed, ROLL_MS};
use crate::theme::Theme;

/// The whole show, from the first step of a cell to the label's figure settling: frames come only this long.
pub const CONDENSE_MS: u64 = 1400;
/// A condensing cell takes one step down the ladder this often.
pub const STEP_MS: u64 = 80;
/// The condensing cells start one after another over this long, oldest first.
pub const STAGGER_MS: u64 = 160;
/// The light leaves the newest condensing cell at this time, and reaches the label's column at the next.
pub const LIGHT_FROM_MS: u64 = 200;
pub const LIGHT_TO_MS: u64 = 1000;
/// Once landed, the light fades where it stopped in this long.
const LIGHT_FADE_MS: u64 = 150;
/// The label's figure counts up from 0 from this time on, for `ROLL_MS`.
pub const ROLL_FROM_MS: u64 = 900;

/// From whole to left out, densest first.
const LADDER: [char; 5] = ['█', '▓', '▒', '░', '·'];
/// The same ladder where only ASCII shows; it ends on what `ascii::fold` makes of the settled glyphs.
const LADDER_ASCII: [char; 5] = ['#', '=', ':', '.', '.'];

/// Where a state sits on the ladder: how much of the message the call still carries.
fn rung(s: CellState) -> usize {
    match s {
        CellState::Whole => 0,
        CellState::Shrunk => 2,
        CellState::Recalled => 3,
        CellState::Dropped => 4,
    }
}

/// Whether a cell that went from `from` to `to` lost some of what the call carries of it.
pub fn condenses(from: CellState, to: CellState) -> bool {
    rung(to) > rung(from)
}

/// The glyph of a cell that went from `from` to `to`, `ms` after its own start: one step down the ladder every
/// `STEP_MS`, from the old glyph to the new one. A cell that did not condense shows its new glyph.
pub fn condense_glyph(ms: u64, from: CellState, to: CellState, ascii: bool) -> char {
    let ladder = if ascii { &LADDER_ASCII } else { &LADDER };
    let (a, b) = (rung(from), rung(to));
    if b <= a {
        return ladder[b];
    }
    ladder[(a + (ms / STEP_MS) as usize).min(b)]
}

/// 0..1: how far that cell is down its ladder. Its colour follows smoothly while the glyph steps.
pub fn condense_level(ms: u64, from: CellState, to: CellState) -> f32 {
    let (a, b) = (rung(from), rung(to));
    if b <= a {
        return 1.0;
    }
    (ms as f32 / (STEP_MS * (b - a) as u64) as f32).min(1.0)
}

/// When the condensing cell `rank` of `n` (oldest first) starts: the wave crosses the map once.
pub fn start_ms(rank: usize, n: usize) -> u64 {
    if n < 2 {
        return 0;
    }
    STAGGER_MS * rank as u64 / (n - 1) as u64
}

/// Where the light is on a path of `len` cells, `ms` after the cut: the index of its head and how bright it is
/// (1 while it runs, fading once it has landed on the last cell). None before it leaves and once it is gone.
pub fn condense_light(ms: u64, len: usize) -> Option<(usize, f32)> {
    if len == 0 || !(LIGHT_FROM_MS..LIGHT_TO_MS + LIGHT_FADE_MS).contains(&ms) {
        return None;
    }
    if ms >= LIGHT_TO_MS {
        return Some((len - 1, 1.0 - (ms - LIGHT_TO_MS) as f32 / LIGHT_FADE_MS as f32));
    }
    let t = (ms - LIGHT_FROM_MS) as f64 / (LIGHT_TO_MS - LIGHT_FROM_MS) as f64;
    Some(((ease_in_out(t) * (len - 1) as f64).round() as usize, 1.0))
}

/// The cells of the light's tail: six, more on a long path, where the head covers several cells a frame.
fn tail_len(len: usize) -> usize {
    (len / 16).clamp(6, 12)
}

/// The cells the light runs through: from the condensing cell `from` left along its row to the panel's border
/// at column `rail`, down the border to the rule's row `rule`, then along the rule to column `to`. With no
/// cell to start from (the map is not on screen) it runs the rule alone, from `rule_left`.
pub fn light_path(from: Option<(u16, u16)>, rail: u16, rule: u16, rule_left: u16, to: u16) -> Vec<(u16, u16)> {
    let mut path = Vec::new();
    let corner = match from {
        Some((x, y)) if y < rule => {
            path.extend((rail.min(x)..=x).rev().map(|c| (c, y)));
            path.extend((y + 1..rule).map(|r| (rail, r)));
            rail
        }
        _ => rule_left,
    };
    if to >= corner {
        path.extend((corner..=to).map(|c| (c, rule)));
    } else {
        path.extend((to..=corner).rev().map(|c| (c, rule)));
    }
    path
}

/// Cells the light may tint: the lines of the panel and the rule, and the map's own cells. Text it passes
/// keeps its colour.
fn carries(symbol: &str) -> bool {
    matches!(symbol, "─" | "│" | "█" | "▓" | "▒" | "░" | "·")
}

/// Tints the light onto `buf` along `path`, `ms` after the cut: a bold head in the text colour and a tail that
/// cools from the second accent back to each cell's own colour. Colour only; on a mono theme the bold head
/// alone carries it.
pub fn paint(buf: &mut Buffer, theme: &Theme, path: &[(u16, u16)], ms: u64) {
    let Some((head, k)) = condense_light(ms, path.len()) else { return };
    let tail = tail_len(path.len());
    for d in 0..=tail.min(head) {
        let Some(c) = buf.cell_mut(path[head - d]) else { continue };
        if !carries(c.symbol()) {
            continue;
        }
        let own = c.fg;
        if d == 0 {
            c.fg = fade(theme, own, theme.fg, k);
            if k > 0.5 {
                c.modifier.insert(Modifier::BOLD);
            }
        } else {
            let cool = fade(theme, theme.accent2, own, d as f32 / (tail + 1) as f32);
            c.fg = fade(theme, own, cool, k);
        }
    }
}

/// The curator's label `ms` after a cut: the figure of what it let go (`-12k` in `curator -12k tok`) shows 0
/// until the light lands, then counts up to its value in `ROLL_MS`, padded to its final width so nothing
/// beside it moves. A label without such a figure, and every label once the count is over, is shown as it is.
pub fn roll_label(label: &str, ms: u64) -> String {
    let rolled = || -> Option<String> {
        let body = label.strip_suffix(" tok")?;
        let (head, fig) = body.rsplit_once(' ')?;
        let num = fig.strip_prefix('-')?;
        let suffix = num.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
        let digits = &num[..num.len() - suffix.len()];
        let value: f64 = digits.parse().ok()?;
        let places = digits.split_once('.').map_or(0, |(_, d)| d.len());
        let t = ms.saturating_sub(ROLL_FROM_MS) as f64 / ROLL_MS as f64;
        if t >= 1.0 {
            return None;
        }
        let shown = format!("-{:.places$}{suffix}", value * crate::anim::ease_out(t));
        Some(format!("{head} {} tok", fixed(&shown, fig)))
    };
    rolled().unwrap_or_else(|| label.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use CellState::*;

    #[test]
    fn a_dropped_cell_steps_down_the_whole_ladder_one_glyph_every_eighty_ms() {
        let at = |ms| condense_glyph(ms, Whole, Dropped, false);
        assert_eq!([0, 80, 160, 240, 320, 900].map(at), ['█', '▓', '▒', '░', '·', '·']);
        let ascii = |ms| condense_glyph(ms, Whole, Dropped, true);
        assert_eq!([0, 80, 160, 240, 320].map(ascii), ['#', '=', ':', '.', '.']);
    }

    #[test]
    fn a_shrunk_cell_stops_half_way_and_a_cell_that_gained_shows_its_new_glyph() {
        let at = |ms| condense_glyph(ms, Whole, Shrunk, false);
        assert_eq!([0, 80, 160, 400].map(at), ['█', '▓', '▒', '▒']);
        assert_eq!(condense_glyph(0, Dropped, Recalled, false), '░');
        assert_eq!(condense_glyph(0, Shrunk, Whole, true), '#');
        assert!(condenses(Whole, Shrunk) && condenses(Shrunk, Dropped) && !condenses(Dropped, Recalled));
    }

    #[test]
    fn the_colour_follows_the_ladder_and_settles_when_the_glyph_does() {
        assert_eq!(condense_level(0, Whole, Dropped), 0.0);
        assert!((condense_level(160, Whole, Dropped) - 0.5).abs() < 1e-6);
        assert_eq!(condense_level(320, Whole, Dropped), 1.0);
        assert_eq!(condense_level(160, Whole, Shrunk), 1.0);
    }

    #[test]
    fn the_wave_starts_with_the_oldest_cell_and_ends_with_the_newest() {
        assert_eq!(start_ms(0, 1), 0);
        assert_eq!([0, 1, 2].map(|r| start_ms(r, 3)), [0, STAGGER_MS / 2, STAGGER_MS]);
    }

    #[test]
    fn the_light_waits_runs_the_whole_path_by_one_second_and_fades_out() {
        assert_eq!(condense_light(0, 40), None);
        assert_eq!(condense_light(199, 40), None);
        assert_eq!(condense_light(200, 40), Some((0, 1.0)));
        let mid = condense_light(600, 40).unwrap().0;
        assert!(mid > 0 && mid < 39, "{mid}");
        assert_eq!(condense_light(1000, 40), Some((39, 1.0)));
        assert!(condense_light(1100, 40).unwrap().1 < 0.5);
        assert_eq!(condense_light(1150, 40), None);
        assert_eq!(condense_light(600, 0), None);
        // the head never goes back
        let heads: Vec<usize> = (200..1000).step_by(16).map(|ms| condense_light(ms, 40).unwrap().0).collect();
        assert!(heads.windows(2).all(|w| w[0] <= w[1]), "{heads:?}");
    }

    #[test]
    fn the_path_leaves_the_cell_by_its_row_runs_down_the_border_and_along_the_rule() {
        // a cell at (54, 3) of a panel whose border is column 50; the rule is row 8; the label at column 20
        let p = light_path(Some((54, 3)), 50, 8, 0, 20);
        assert_eq!(&p[..5], &[(54, 3), (53, 3), (52, 3), (51, 3), (50, 3)]);
        assert_eq!(&p[5..9], &[(50, 4), (50, 5), (50, 6), (50, 7)]);
        assert_eq!(p[9], (50, 8));
        assert_eq!(p.last(), Some(&(20, 8)));
        assert_eq!(p.len(), 5 + 4 + 31);
        // no map on screen: the rule alone, from its left end
        let p = light_path(None, 50, 8, 0, 20);
        assert_eq!((p[0], p.len()), ((0, 8), 21));
    }

    #[test]
    fn the_label_figure_counts_up_from_zero_in_the_same_width() {
        let label = "curator -12k tok";
        let w = unicode_width::UnicodeWidthStr::width;
        for ms in (0..=CONDENSE_MS).step_by(50) {
            assert_eq!(w(roll_label(label, ms).as_str()), w(label), "{ms} ms: {}", roll_label(label, ms));
        }
        assert_eq!(roll_label(label, 0), "curator  -0k tok");
        assert_eq!(roll_label(label, ROLL_FROM_MS + ROLL_MS), label);
        let mid = roll_label("curator -3.2k tok", ROLL_FROM_MS + ROLL_MS / 2);
        assert!(mid.starts_with("curator -") && mid != "curator -3.2k tok", "{mid}");
        // a label without a figure stays as it is
        assert_eq!(roll_label("curator", 0), "curator");
        assert_eq!(roll_label("curator +2k tok", 0), "curator +2k tok");
    }
}
