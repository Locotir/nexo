//! The welcome box and the animated nexo mark that opens a session.

use super::*;

/// The mark of nexo: four nodes tied to one centre, the link the name means.
pub(super) const LOGO: [&str; 3] = ["●─╮ ╭─●", "  ╰●╯  ", "●─╯ ╰─●"];

/// How long the banner takes to come in when nexo opens (every part has settled by then).
pub const INTRO_MS: u64 = 1900;

/// The four outer nodes of the mark and when each is born, clockwise from the top left.
pub(super) const NODES: [(usize, usize, u64); 4] = [(0, 0, 0), (0, 6, 110), (2, 6, 220), (2, 0, 330)];
/// A pulse leaves a node this long after it is born ...
pub(super) const PULSE_AFTER: u64 = 180;
/// ... and crosses one cell of its tie per step, towards the centre.
pub(super) const PULSE_STEP: u64 = 75;
/// The wave that leaves the centre once it lights moves one cell out per step.
pub(super) const WAVE_STEP: u64 = 70;

/// How many cells the mark's cell at row `r`, column `c` is from the centre.
pub(super) fn from_centre(r: usize, c: usize) -> u64 {
    (c as i64 - 3).unsigned_abs() + (r as i64 - 1).unsigned_abs()
}

/// When the last pulse reaches the centre and it lights.
pub(super) fn centre_lit() -> u64 {
    NODES.iter().map(|n| n.2).max().unwrap_or(0) + PULSE_AFTER + 4 * PULSE_STEP
}

/// When the mark's cell at row `r`, column `c` first shows: a node when it is born, a tie
/// when the first pulse of a node on its side reaches it, the centre when the last one does.
pub(super) fn cell_born(r: usize, c: usize) -> u64 {
    if (r, c) == (1, 3) {
        return centre_lit();
    }
    if let Some(n) = NODES.iter().find(|n| (n.0, n.1) == (r, c)) {
        return n.2;
    }
    NODES
        .iter()
        .filter(|n| (n.1 < 3) == (c < 3) && (r == 1 || r == n.0))
        .map(|n| n.2 + PULSE_AFTER + 4u64.saturating_sub(from_centre(r, c)) * PULSE_STEP)
        .min()
        .unwrap_or(0)
}

/// The mark's cell at row `r`, column `c`, `ms` into the intro: its glyph and colour.
/// The nodes are born one after another and grow from a dot; a pulse runs down each tie,
/// drawing it with a bright head; the centre, a faint seed until then, lights when the last
/// pulse arrives and sends a wave back out over the whole mark.
pub(super) fn intro_cell(theme: &Theme, glyph: char, colour: Color, r: usize, c: usize, ms: u64) -> (char, Color) {
    let born = cell_born(r, c);
    let centre = (r, c) == (1, 3);
    if ms < born {
        return if centre { ('·', theme.dim2) } else { (' ', colour) };
    }
    let age = ms - born;
    let glyph = match (glyph, age) {
        ('●', 0..=59) => '·',
        ('●', 60..=129) => '•',
        _ => glyph,
    };
    let mut lit = if age < 90 { theme.fg } else { mix(theme.fg, colour, (age - 90) as f32 / 260.0) };
    let wave = centre_lit() + WAVE_STEP * from_centre(r, c);
    if !centre && (wave..wave + 240).contains(&ms) {
        lit = mix(theme.fg, lit, (ms - wave) as f32 / 240.0);
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

/// When each line of the banner's text starts to be typed, and the ms between characters.
pub(super) const TYPE_TITLE: (u64, u64) = (560, 45);
pub(super) const TYPE_MODEL: (u64, u64) = (900, 7);
pub(super) const TYPE_PLACE: (u64, u64) = (1000, 7);
pub(super) const TYPE_HINT: (u64, u64) = (1250, 4);
/// How long the frame of the banner takes to fade in.
pub(super) const BORDER_MS: u64 = 350;

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
    let inner = w.saturating_sub(4).clamp(1, 72);
    let border = match intro_ms {
        Some(ms) if ms < BORDER_MS => theme.style(mix(theme.bg, theme.border, ms as f32 / BORDER_MS as f32)),
        _ => theme.style(theme.border),
    };
    let type_in = |spans: Vec<Span<'static>>, (start, per): (u64, u64)| match intro_ms {
        Some(ms) => typed(spans, theme, ms, start, per),
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
        let pace = [TYPE_TITLE, TYPE_MODEL, TYPE_PLACE][i];
        let typing = intro_ms.is_some_and(|ms| i == 0 && ms >= pace.0 && ms < pace.0 + pace.1 * 12 + 150);
        v.extend(type_in(spans, pace));
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
