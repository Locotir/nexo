//! The welcome box and the animated nexo mark that opens a session.

use super::*;
use crate::fx_ignite as ignite;

/// The mark of nexo: four nodes tied to one centre, the link the name means.
pub(super) const LOGO: [&str; 3] = ["●─╮ ╭─●", "  ╰●╯  ", "●─╯ ╰─●"];

/// How long the banner takes to come in when nexo opens (every part has settled by then). The mark here
/// keeps time with the large one the sky draws as the session opens (`fx_ignite`): its nodes ignite, its
/// lights run and its centre ignites at the same moments.
pub const INTRO_MS: u64 = ignite::END_MS;

/// How many cells the mark's cell at row `r`, column `c` is from the centre.
pub(super) fn from_centre(r: usize, c: usize) -> u64 {
    (c as i64 - 3).unsigned_abs() + (r as i64 - 1).unsigned_abs()
}

/// The strokes the mark's cell at row `r`, column `c` is on, numbered as in `fx_ignite` (clockwise from the
/// top left, like the nodes they leave): the ties beside the centre are on two.
fn strokes(r: usize, c: usize) -> impl Iterator<Item = usize> {
    let (top, bottom) = if c < 3 { (0, 3) } else { (1, 2) };
    [(r != 2).then_some(top), (r != 0).then_some(bottom)].into_iter().flatten()
}

/// When the mark's cell at row `r`, column `c` first shows: a node when it ignites, a tie when the first light
/// running down a stroke it is on reaches it, the centre when the last light does.
pub(super) fn cell_born(r: usize, c: usize) -> u64 {
    let d = from_centre(r, c);
    if d == 0 {
        return ignite::CENTRE_MS;
    }
    let along = (4 - d.min(4)) as f32 / 4.0;
    strokes(r, c)
        .map(|i| if d >= 4 { ignite::BORN[i] } else { ignite::reach(i, along).ceil() as u64 })
        .min()
        .unwrap_or(0)
}

/// White-hot (the text's colour) for `hot` ms after a cell lights, then cooling to its own `colour`, `age` ms
/// after it lit.
fn cooling(theme: &Theme, colour: Color, age: u64, hot: u64) -> Color {
    const COOL_MS: u64 = 220;
    match age.checked_sub(hot) {
        None => theme.fg,
        Some(a) if a < COOL_MS => mix(theme.fg, colour, a as f32 / COOL_MS as f32),
        Some(_) => colour,
    }
}

/// The mark's cell at row `r`, column `c`, `ms` into the intro: its glyph and colour. The nodes ignite one
/// after another and grow from a dot; a light runs down each tie, drawing it white-hot; the centre, a faint
/// seed that brightens as the lights arrive, ignites with the last one and sends the light back out over the
/// whole mark.
pub(super) fn intro_cell(theme: &Theme, glyph: char, colour: Color, r: usize, c: usize, ms: u64) -> (char, Color) {
    let d = from_centre(r, c);
    if d == 0 {
        if ms < ignite::CENTRE_MS {
            let gathered = (0..4).filter(|&i| ms as f32 >= ignite::reach(i, 1.0)).count();
            let seed = if gathered >= 2 { '•' } else { '·' };
            return (seed, mix(theme.dim2, colour, gathered as f32 / 6.0));
        }
        return (glyph, cooling(theme, colour, ms - ignite::CENTRE_MS, 130));
    }
    let born = cell_born(r, c);
    if ms < born {
        return (' ', colour);
    }
    let age = ms - born;
    let glyph = match (glyph, age) {
        ('●', 0..=39) => '·',
        ('●', 40..=79) => '•',
        _ => glyph,
    };
    let lit = cooling(theme, colour, age, 90);
    // the light the centre sends back out passes over the cell, white-hot for a moment
    let back = ignite::echo(d as f32 / 4.0);
    let since = ms as f32 - back;
    if (0.0..200.0).contains(&since) {
        return (glyph, mix(theme.fg, lit, since / 200.0));
    }
    (glyph, lit)
}

/// `spans` as they are being typed, `ms` into the intro: the first character shows at
/// `start`, each next one `per` ms later, bright for a moment, then in its own style.
/// What is not typed yet takes no room (the row pads to its width anyway).
pub(super) fn typed(spans: Vec<Span<'static>>, theme: &Theme, ms: u64, start: u64, per: u64) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut k = 0;
    for s in spans {
        let own = s.style.fg.unwrap_or(theme.fg);
        for ch in s.content.chars() {
            let at = start + k * per;
            k += 1;
            if ms < at {
                return out;
            }
            let t = (ms - at) as f32 / 200.0;
            let style = if t >= 1.0 { s.style } else { s.style.fg(mix(theme.accent, own, t)) };
            out.push(Span::styled(ch.to_string(), style));
        }
    }
    out
}

/// When each line of the banner's text starts to be typed, and the ms between characters: the title while the
/// lights run down the mark, the rest as its centre ignites and after. A long line is typed faster, so every
/// line is in and settled by `INTRO_MS`.
pub(super) const TYPE_TITLE: (u64, u64) = (420, 24);
pub(super) const TYPE_MODEL: (u64, u64) = (720, 5);
pub(super) const TYPE_PLACE: (u64, u64) = (840, 4);
pub(super) const TYPE_HINT: (u64, u64) = (1000, 3);
/// How long the frame of the banner takes to fade in.
pub(super) const BORDER_MS: u64 = 300;

/// The ms between the characters of a line of `chars` characters typed from `start` at `per`: `per`, or less
/// when the line would not be in (with the 200 ms each character takes to settle) by `INTRO_MS`.
pub(super) fn pace(chars: usize, (start, per): (u64, u64)) -> u64 {
    per.min(INTRO_MS.saturating_sub(start + 200) / chars.max(1) as u64)
}

fn chars(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.chars().count()).sum()
}

/// The banner of an empty session: the mark beside the version, the model and the place, then one line of hints.
pub(super) fn welcome(
    w: usize,
    theme: &Theme,
    model: &str,
    place: &str,
    permissions: &str,
    intro_ms: Option<u64>,
    detail: nexo_core::config::Detail,
) -> Vec<Line<'static>> {
    // 72 columns, more when the folder or the model would be cut short and the terminal has room
    let want = (width(place).max(width(model) + width(permissions) + 3) + width(LOGO[0]) + 3).clamp(72, 104);
    let inner = w.saturating_sub(4).clamp(1, want);
    let border = match intro_ms {
        Some(ms) if ms < BORDER_MS => theme.style(mix(theme.bg, theme.border, ms as f32 / BORDER_MS as f32)),
        _ => theme.style(theme.border),
    };
    let type_in = |spans: Vec<Span<'static>>, (start, per): (u64, u64)| match intro_ms {
        Some(ms) => {
            let per = pace(chars(&spans), (start, per));
            typed(spans, theme, ms, start, per)
        }
        None => spans,
    };
    let row = |spans: Vec<Span<'static>>| -> Line<'static> {
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        let mut v = vec![Span::styled("│ ", border)];
        v.extend(spans);
        v.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        v.push(Span::styled(" │", border));
        Line::from(v)
    };
    // Narrow boxes drop the mark and keep the text.
    let gutter = if inner >= 40 { width(LOGO[0]) + 3 } else { 0 };
    let text = inner - gutter;
    // the mode that never asks shows as YOLO, in red
    let (permissions, mode_style) = match permissions {
        "yolo" | "auto" => ("YOLO", theme.bold(theme.error)),
        p => (p, theme.dim()),
    };
    let info: [Vec<Span<'static>>; 3] = [
        vec![
            Span::styled("nexo", theme.bold(theme.accent)),
            Span::styled(format!(" {}", nexo_core::version::get()), theme.dim()),
        ],
        vec![
            Span::raw(truncate(model, text.saturating_sub(width(permissions) + 3))),
            Span::styled(" · ", theme.dim()),
            Span::styled(permissions.to_string(), mode_style),
        ],
        vec![Span::styled(truncate(place, text), theme.dim())],
    ];
    let mut lines = vec![Line::from(Span::styled(format!("╭{}╮", "─".repeat(inner + 2)), border))];
    for (i, spans) in info.into_iter().enumerate() {
        let mut v = Vec::new();
        if gutter > 0 {
            // The outer nodes in the second accent, the ties and the centre in the first.
            for (j, c) in LOGO[i].chars().enumerate() {
                let outer = c == '●' && i != 1;
                let colour = if outer { theme.accent2 } else { theme.accent };
                let (c, colour) = match intro_ms {
                    Some(ms) if c != ' ' => intro_cell(theme, c, colour, i, j, ms),
                    _ => (c, colour),
                };
                let style = if outer { theme.style(colour) } else { theme.bold(colour) };
                v.push(Span::styled(c.to_string(), style));
            }
            v.push(Span::raw("   "));
        }
        let when = [TYPE_TITLE, TYPE_MODEL, TYPE_PLACE][i];
        let n = chars(&spans);
        let typed_by = when.0 + pace(n, when) * n.saturating_sub(1) as u64;
        let typing = intro_ms.is_some_and(|ms| i == 0 && ms >= when.0 && ms < typed_by + 150);
        v.extend(type_in(spans, when));
        if typing {
            // the title is typed with a caret
            v.push(Span::styled("▍", theme.style(theme.accent)));
        }
        lines.push(row(v));
    }
    // quiet: name, model and folder only; the commands are in /help
    if detail == nexo_core::config::Detail::Full {
        lines.push(row(vec![]));
        let hint = "/model sets up a model · /resume picks a session · ? help";
        lines.push(row(type_in(vec![Span::styled(truncate(hint, inner), theme.dim())], TYPE_HINT)));
    }
    lines.push(Line::from(Span::styled(format!("╰{}╯", "─".repeat(inner + 2)), border)));
    lines
}
