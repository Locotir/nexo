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
    let track = crate::motion::fade(theme, theme.bar_bg, theme.border, 0.7);
    let fill_colour = if pulse { crate::motion::fade(theme, colour, theme.fg, 0.35) } else { colour };
    let (fill, rest) = line_gauge(share, cells);
    let mut out = vec![Seg { text: fill, style: Style::default().fg(fill_colour) }];
    if !rest.is_empty() {
        out.push(Seg { text: rest, style: Style::default().fg(track) });
    }
    out
}

/// The gauge's cells with the leading one lit when the fill just reached a new step (`step_ms` since, a
/// quick rise to the text colour and back), and a bright edge running over all of them when the level just
/// crossed into a higher band (`cross_ms` since). Pure in the two times; with neither it is `cells`
/// untouched. Where the colours do not blend (16 colours) the lit cells are bold instead.
pub(super) fn meniscus(theme: &Theme, cells: Vec<Seg>, step_ms: Option<u64>, cross_ms: Option<u64>) -> Vec<Seg> {
    use crate::fx::chrome::{CROSS_MS, STEP_MS};
    if step_ms.is_none() && cross_ms.is_none() {
        return cells;
    }
    let track = crate::motion::fade(theme, theme.bar_bg, theme.border, 0.7);
    let mut one: Vec<Seg> = cells
        .into_iter()
        .flat_map(|s| {
            let st = s.style;
            s.text.chars().map(move |c| Seg { text: c.to_string(), style: st }).collect::<Vec<_>>()
        })
        .collect();
    let blends = |c: Option<ratatui::style::Color>| {
        matches!(c, Some(ratatui::style::Color::Rgb(..)) | Some(ratatui::style::Color::Indexed(_)))
    };
    let light = |seg: &mut Seg, k: f32| {
        if k <= 0.0 {
            return;
        }
        if blends(seg.style.fg) {
            let from = seg.style.fg.unwrap_or(theme.fg);
            seg.style = seg.style.fg(crate::motion::fade(theme, from, theme.fg, k));
        } else if k > 0.3 || theme.mono {
            seg.style = seg.style.add_modifier(Modifier::BOLD);
        }
    };
    if let Some(ms) = step_ms {
        // up within 80 ms, back by STEP_MS
        let k = if ms < 80 { ms as f32 / 80.0 } else { 1.0 - (ms - 80) as f32 / (STEP_MS - 80) as f32 };
        if let Some(lead) = one.iter().rposition(|s| s.style.fg != Some(track)) {
            light(&mut one[lead], k.clamp(0.0, 1.0));
        }
    }
    if let Some(ms) = cross_ms {
        let n = one.len() as f32;
        let head = ms as f32 / CROSS_MS as f32 * (n + 2.0) - 1.0;
        for (i, seg) in one.iter_mut().enumerate() {
            if seg.style.fg == Some(track) {
                continue;
            }
            light(seg, (1.0 - (head - i as f32).abs() / 1.5).clamp(0.0, 1.0));
        }
    }
    one
}

#[cfg(test)]
mod meniscus_tests {
    use super::*;
    use ratatui::style::Color;

    fn row(theme: &Theme, share: f64) -> Vec<Seg> {
        gauge_segs(theme, share, GAUGE_CELLS, theme.success, false)
    }
    fn fgs(v: &[Seg]) -> Vec<Option<Color>> {
        v.iter().map(|s| s.style.fg).collect()
    }

    #[test]
    fn the_leading_cell_lights_and_goes_back_and_nothing_else_changes() {
        let th = Theme::dark();
        let base = fgs(&meniscus(&th, row(&th, 0.5), Some(crate::fx::chrome::STEP_MS), None));
        let at = |ms| fgs(&meniscus(&th, row(&th, 0.5), Some(ms), None));
        assert_eq!(at(0), base);
        let (mid, late) = (at(80), at(300));
        assert_eq!(late, base);
        let lead = base.iter().rposition(|c| *c == Some(th.success)).unwrap();
        assert_ne!(mid[lead], base[lead], "lit at 80 ms");
        assert!(at(150)[lead] != base[lead] && at(150)[lead] != mid[lead], "on its way back");
        for (i, c) in mid.iter().enumerate().filter(|(i, _)| *i != lead) {
            assert_eq!(*c, base[i], "cell {i}");
        }
        assert_eq!(mid.len(), base.len(), "no cell is added or lost");
    }

    #[test]
    fn a_crossing_runs_an_edge_from_left_to_right_over_the_fill() {
        let th = Theme::dark();
        let base = fgs(&meniscus(&th, row(&th, 1.0), None, Some(crate::fx::chrome::CROSS_MS)));
        let lit = |ms: u64| -> Vec<usize> {
            let v = fgs(&meniscus(&th, row(&th, 1.0), None, Some(ms)));
            (0..v.len()).filter(|i| v[*i] != base[*i]).collect()
        };
        assert!(lit(0).iter().all(|i| *i < 2));
        let mid = lit(225);
        assert!(mid.iter().any(|i| (2..6).contains(i)) && !mid.contains(&0), "{mid:?}");
        assert!(lit(449).iter().all(|i| *i >= 5), "{:?}", lit(449));
    }

    #[test]
    fn on_sixteen_colours_it_is_bold_with_the_colour_unchanged() {
        let mut th = Theme::dark();
        th.success = Color::Green;
        th.border = Color::DarkGray;
        th.bar_bg = Color::Black;
        let v = meniscus(&th, row(&th, 0.5), Some(80), None);
        let lead = v.iter().rposition(|s| s.style.fg == Some(Color::Green)).unwrap();
        assert!(v[lead].style.add_modifier.contains(Modifier::BOLD));
    }
}
