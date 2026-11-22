//! Cards for what nexo remembers, a newer nexo, and the notices the user reads.

use super::*;

/// A notice as the user reads it: the curator's name stays inside, the screen says what happened.
/// The card for a newer nexo: a framed box in the accent colour, so it is not read past like a
/// grey notice. Too narrow for a frame, it falls back to two bold lines.
/// A `file://` address for a path, the form a Ctrl+click opens: forward slashes, spaces and other
/// characters a link cannot hold written as `%XX`.
pub fn file_url(path: &str) -> String {
    let path = path.replace('\\', "/");
    let mut url = String::from("file://");
    if !path.starts_with('/') {
        url.push('/');
    }
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' | b'@' | b'+' | b',' => {
                url.push(b as char)
            }
            _ => url.push_str(&format!("%{b:02X}")),
        }
    }
    url
}

/// What nexo saved for later sessions: `✦ remembered · this project`, the lines, and the file as a link. A
/// rewritten line shows the old one first (`replaced: ...`); a removed one has the card `✦ forgot` with `removed: ...`.
pub(super) fn remembered_card(
    scope: &str,
    items: &[String],
    path: &str,
    was: Option<&str>,
    theme: &Theme,
    w: usize,
) -> Vec<Line<'static>> {
    let edge = theme.style(theme.accent2);
    let rail = || Span::styled("│ ", edge);
    let place = if scope == "project" { "this project" } else { "global" };
    let mut lines = vec![Line::from(vec![
        Span::styled("✦ ", edge),
        Span::styled(if items.is_empty() { "forgot" } else { "remembered" }, edge.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" · {place}"), theme.dim()),
    ])];
    // the older line a rewrite or a removal took out, one row (wrapped when it is long) above the new line
    if let Some(old) = was {
        let tag = if items.is_empty() { "removed: " } else { "replaced: " };
        lines.extend(crate::render::wrap::wrap_runs(
            &[(format!("{tag}{old}"), theme.dim())],
            w,
            &[rail()],
            &[rail(), Span::raw("  ")],
        ));
    }
    for it in items {
        lines.extend(crate::render::wrap::wrap_runs(
            &[(it.clone(), Style::default().fg(theme.fg))],
            w,
            &[rail(), Span::styled("• ", edge)],
            &[rail(), Span::raw("  ")],
        ));
    }
    // the address on one row, whole, so a Ctrl+click finds all of it
    lines.push(Line::from(vec![
        Span::styled("└ ", edge),
        Span::styled(file_url(path), Style::default().fg(theme.accent).add_modifier(Modifier::UNDERLINED)),
    ]));
    lines
}

/// The colour of a reasoning level: the light ones dim, `medium` plain, `high` in the accent, the top ones
/// warm and bold.
fn effort_style(effort: &str, theme: &Theme) -> Style {
    match effort {
        "off" | "minimal" | "low" => theme.style(theme.dim),
        "high" => theme.style(theme.accent),
        "xhigh" | "max" => Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
        _ => theme.style(theme.fg),
    }
}

/// Choose readable text without changing the badge's background. ANSI's first 16 colours can be
/// customised by the terminal, so their standard RGB values are the best available approximation.
fn badge_foreground(background: Color, preferred: Color) -> Color {
    fn luminance(c: Color) -> Option<f64> {
        let (r, g, b) = match c {
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(n @ 16..=231) => {
                let n = n - 16;
                let level = |v| if v == 0 { 0 } else { 55 + 40 * v };
                (level(n / 36), level((n / 6) % 6), level(n % 6))
            }
            Color::Indexed(n @ 232..=255) => {
                let v = 8 + (n - 232) * 10;
                (v, v, v)
            }
            Color::Black | Color::Indexed(0) => (0, 0, 0),
            Color::Red | Color::Indexed(1) => (128, 0, 0),
            Color::Green | Color::Indexed(2) => (0, 128, 0),
            Color::Yellow | Color::Indexed(3) => (128, 128, 0),
            Color::Blue | Color::Indexed(4) => (0, 0, 128),
            Color::Magenta | Color::Indexed(5) => (128, 0, 128),
            Color::Cyan | Color::Indexed(6) => (0, 128, 128),
            Color::Gray | Color::Indexed(7) => (192, 192, 192),
            Color::DarkGray | Color::Indexed(8) => (128, 128, 128),
            Color::LightRed | Color::Indexed(9) => (255, 0, 0),
            Color::LightGreen | Color::Indexed(10) => (0, 255, 0),
            Color::LightYellow | Color::Indexed(11) => (255, 255, 0),
            Color::LightBlue | Color::Indexed(12) => (0, 0, 255),
            Color::LightMagenta | Color::Indexed(13) => (255, 0, 255),
            Color::LightCyan | Color::Indexed(14) => (0, 255, 255),
            Color::White | Color::Indexed(15) => (255, 255, 255),
            Color::Reset => return None,
        };
        let linear = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        Some(0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b))
    }
    let Some(bg) = luminance(background) else { return preferred };
    if let Some(fg) = luminance(preferred) {
        if (bg.max(fg) + 0.05) / (bg.min(fg) + 0.05) >= 4.5 {
            return preferred;
        }
    }
    if (bg + 0.05) / 0.05 >= 1.05 / (bg + 0.05) {
        Color::Black
    } else {
        Color::White
    }
}

/// A sub-agent's line: ` general  codex/gpt · medium · task`. `spinner` makes the model shimmer while the run
/// works (None: no motion); an ended run is all dim. The task is cut to fit one row.
pub(super) fn agent_line(run: &AgentRun, spinner: Option<usize>, theme: &Theme, w: usize) -> Vec<Line<'static>> {
    let dim = theme.style(theme.dim);
    // Keep explore's existing accent; general gets a stable, contrasting theme colour.
    // Mono deliberately retains the terminal's own colours.
    let profile_colour = if run.profile == "general" && !theme.mono {
        [theme.success, theme.accent2, theme.warn, theme.fg]
            .into_iter()
            .find(|&colour| colour != theme.accent && colour != theme.bg)
            .unwrap_or(theme.accent)
    } else {
        theme.accent
    };
    let badge_bg = if run.running { profile_colour } else { theme.dim };
    let badge_fg = if theme.mono { theme.bg } else { badge_foreground(badge_bg, theme.bg) };
    let badge = Style::default().fg(badge_fg).bg(badge_bg).add_modifier(Modifier::BOLD);
    let face = super::live_marks::robot(run.running, spinner.unwrap_or(0));
    let mut spans = vec![
        Span::styled(format!("{face} "), if run.running { theme.style(theme.accent2) } else { dim }),
        Span::styled(format!(" {} ", run.profile), badge),
        Span::raw(" "),
    ];
    match spinner.filter(|_| run.running) {
        Some(frame) => spans.extend(super::live_marks::shimmer(&run.model, frame, theme)),
        None if run.running => spans.push(Span::styled(run.model.clone(), theme.style(theme.accent))),
        None => spans.push(Span::styled(run.model.clone(), dim)),
    }
    if let Some(e) = &run.effort {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(e.clone(), if run.running { effort_style(e, theme) } else { dim }));
    }
    let used: usize = spans.iter().map(|s| width(&s.content)).sum();
    if used + 4 > w {
        let effort = run.effort.as_deref().map(|e| format!(" · {e}")).unwrap_or_default();
        let plain = format!("{face} {} · {}{effort} · {}", run.profile, run.model, run.title);
        let mut rows = wrap_plain(&plain, w, dim, &[Span::raw("  ")]);
        // Preserve the face and highlighted profile even when metadata wraps.
        if w >= width(&run.profile) + 9 {
            rows = vec![Line::from(vec![
                Span::styled(format!("{face} "), theme.style(theme.accent2)),
                Span::styled(format!(" {} ", run.profile), badge),
            ])];
            rows.extend(wrap_plain(&format!("{}{effort} · {}", run.model, run.title), w, dim, &[Span::raw("  ")]));
        }
        return rows;
    }
    spans.push(Span::styled(" · ", dim));
    spans.push(Span::styled(truncate(&run.title, w - used - 3), dim));
    vec![Line::from(spans)]
}

pub(super) fn update_card(behind: u32, theme: &Theme, w: usize) -> Vec<Line<'static>> {
    let edge = Style::default().fg(theme.accent2).add_modifier(Modifier::BOLD);
    let text = Style::default().fg(theme.fg);
    let cmd = Style::default().fg(theme.accent).add_modifier(Modifier::BOLD);
    let s = if behind == 1 { "" } else { "s" };
    let what = format!("{behind} new commit{s}, ready-built.");
    let run = [("Run ", text), ("nexo update", cmd), (", then restart nexo.", text)];
    let run_w: usize = run.iter().map(|(t, _)| width(t)).sum();
    let title = " ⬆ Update ready ";
    let inner = width(&what).max(run_w).max(width(title) + 1);
    if w < inner + 4 {
        let mut lines = wrap_plain(&format!("⬆ Update ready · {what}"), w, edge, &[Span::raw("  ")]);
        lines.extend(wrap_plain("Run `nexo update`, then restart nexo.", w, text, &[Span::raw("  ")]));
        return lines;
    }
    let row = |spans: Vec<Span<'static>>, used: usize| {
        let mut v = vec![Span::styled("│ ", edge)];
        v.extend(spans);
        v.push(Span::raw(" ".repeat(inner - used)));
        v.push(Span::styled(" │", edge));
        Line::from(v)
    };
    vec![
        Line::from(vec![
            Span::styled("╭─", edge),
            Span::styled(title, edge),
            Span::styled(format!("{}╮", "─".repeat(inner + 1 - width(title))), edge),
        ]),
        row(vec![Span::styled(what.clone(), text)], width(&what)),
        row(run.iter().map(|(t, st)| Span::styled(t.to_string(), *st)).collect(), run_w),
        Line::from(Span::styled(format!("╰{}╯", "─".repeat(inner + 2)), edge)),
    ]
}

/// How the agent starts its note that the curator trimmed the context.
pub(super) const CURATOR_PREFIX: &str = "the curator kept the view at ";

/// The first line of the notice that shows the task the main agent gave a sub-agent: the lines under it
/// are the task, drawn as written (no markdown), and not as words the person typed.
pub const TASK_HEAD: &str = "task from the main agent:";

pub(super) fn user_notice(text: &str) -> std::borrow::Cow<'_, str> {
    match text.strip_prefix(CURATOR_PREFIX) {
        Some(rest) => format!("context trimmed to {rest}").into(),
        None => text.into(),
    }
}

#[cfg(test)]
mod badge_contrast_tests {
    use super::*;

    #[test]
    fn badge_render_uses_readable_foreground_for_light_and_dark_backgrounds() {
        for (background, expected) in [
            (Color::Rgb(0, 255, 0), Color::Black),
            (Color::Rgb(12, 20, 45), Color::White),
            (Color::Indexed(46), Color::Black),
            (Color::Indexed(17), Color::White),
        ] {
            for running in [true, false] {
                for width in [24, 120] {
                    let mut theme = Theme::dark();
                    theme.bg = if expected == Color::Black { Color::White } else { Color::Black };
                    theme.success = background;
                    theme.dim = background;
                    theme.accent = Color::Rgb(120, 0, 120);
                    let run = AgentRun {
                        id: "1".into(),
                        profile: "general".into(),
                        model: "provider/model".into(),
                        effort: None,
                        title: "task".into(),
                        running,
                    };
                    let rows = agent_line(&run, Some(0), &theme, width);
                    let badge = rows.iter().flat_map(|row| &row.spans).find(|s| s.content == " general ").unwrap();
                    assert_eq!(badge.style.bg, Some(background));
                    assert_eq!(badge.style.fg, Some(expected));
                    assert!(badge.style.add_modifier.contains(Modifier::BOLD));
                }
            }
        }
        assert_eq!(badge_foreground(Color::Rgb(0, 255, 0), Color::Rgb(0, 0, 0)), Color::Rgb(0, 0, 0));
    }
}
