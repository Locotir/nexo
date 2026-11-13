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

/// What nexo saved for later sessions: `✦ remembered · this project`, the lines, and the file as a link.
pub(super) fn remembered_card(
    scope: &str,
    items: &[String],
    path: &str,
    theme: &Theme,
    w: usize,
) -> Vec<Line<'static>> {
    let edge = theme.style(theme.accent2);
    let rail = || Span::styled("│ ", edge);
    let place = if scope == "project" { "this project" } else { "global" };
    let mut lines = vec![Line::from(vec![
        Span::styled("✦ ", edge),
        Span::styled("remembered", edge.add_modifier(Modifier::BOLD)),
        Span::styled(format!(" · {place}"), theme.dim()),
    ])];
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

pub(super) fn user_notice(text: &str) -> std::borrow::Cow<'_, str> {
    match text.strip_prefix(CURATOR_PREFIX) {
        Some(rest) => format!("context trimmed to {rest}").into(),
        None => text.into(),
    }
}
