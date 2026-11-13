//! Helpers around the renderer: plain text of lines, prefixed blocks, and one-line inline markdown.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::super::wrap::{truncate, width};
use super::{render, render_as_meta};
use crate::regions::Meta;
use crate::theme::Theme;

/// The plain text of rendered lines (for searching and copying).
pub fn text_of(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// `md` as wrapped lines inside `room` columns: plain text takes `base`, every line starts with
/// `prefix`, blank lines at the edges are dropped.
pub fn md_lines(
    md: &str,
    room: usize,
    theme: &Theme,
    base: Style,
    interpreted: bool,
    prefix: &[Span<'static>],
) -> Vec<Line<'static>> {
    md_lines_meta(md, room, theme, base, interpreted, prefix).0
}

/// [`md_lines`] and its wrapped rows (the boxes inside are left to the caller, who boxes the whole).
pub fn md_lines_meta(
    md: &str,
    room: usize,
    theme: &Theme,
    base: Style,
    interpreted: bool,
    prefix: &[Span<'static>],
) -> (Vec<Line<'static>>, Meta) {
    let pw: usize = prefix.iter().map(|s| width(&s.content)).sum();
    let (mut lines, mut meta) = render_as_meta(md, room.saturating_sub(pw), theme, interpreted);
    while lines.last().is_some_and(|l| text_of(l).trim().is_empty()) {
        lines.pop();
    }
    let lead = lines.iter().take_while(|l| text_of(l).trim().is_empty()).count();
    meta.regions.clear();
    meta.drop_front(lead);
    meta.truncate(lines.len() - lead.min(lines.len()));
    meta.add_deco(pw);
    let lines = lines
        .into_iter()
        .skip(lead)
        .map(|l| {
            let mut v = prefix.to_vec();
            v.extend(l.spans.into_iter().map(|s| tint_plain(s, base)));
            Line::from(v)
        })
        .collect();
    (lines, meta)
}

/// `s` without inline marks (backticks, `**`, `__`), for one-line places that cannot style.
pub fn strip_md(s: &str) -> String {
    s.replace("**", "").replace("__", "").replace('`', "")
}

/// One line of a message as styled runs: backticks and `**` become code and bold, plain text takes
/// `base`. A line without marks, or one that would read as a list, heading or quote, stays as is.
pub fn inline_runs(line: &str, theme: &Theme, base: Style) -> Vec<(String, Style)> {
    let t = line.trim_start();
    let blockish = t.starts_with(['#', '>', '|'])
        || ["- ", "* ", "+ "].iter().any(|p| t.starts_with(p))
        || t.trim_start_matches(|c: char| c.is_ascii_digit()).starts_with(". ");
    if blockish || !(t.contains('`') || t.contains("**") || t.contains("__")) {
        return vec![(line.to_string(), base)];
    }
    let indent = &line[..line.len() - t.len()];
    let mut runs = Vec::new();
    if !indent.is_empty() {
        runs.push((indent.to_string(), base));
    }
    runs.extend(inline_md(t, usize::MAX / 2, theme, base).into_iter().map(|s| (s.content.into_owned(), s.style)));
    runs
}

/// Whether plain text is a markdown report (a `#` heading, `**bold**` or a table) and not log or
/// command output.
pub fn looks_markdown(text: &str) -> bool {
    let is_heading = |l: &str| {
        let h = l.trim_start_matches('#');
        l.starts_with('#') && l.len() - h.len() <= 6 && h.starts_with(' ')
    };
    // a lone `# x` further down is more likely a shell comment
    let heading =
        text.lines().next().is_some_and(is_heading) || text.lines().any(|l| l.starts_with("##") && is_heading(l));
    let bold = text.matches("**").count() >= 2;
    let table = text.lines().any(|l| {
        let t = l.trim();
        t.starts_with('|') && t.contains("---")
    });
    heading || bold || table
}

/// A span of rendered markdown: plain text (no colour of its own) takes `base`; code, links and
/// the like keep theirs.
pub fn tint_plain(s: Span<'static>, base: Style) -> Span<'static> {
    if s.style.fg.is_none() {
        Span::styled(s.content, base.patch(s.style))
    } else {
        s
    }
}

/// `md` (one line of it) rendered as markdown spans, cut to `room` columns with an ellipsis.
pub fn inline_md(md: &str, room: usize, theme: &Theme, base: Style) -> Vec<Span<'static>> {
    let lines = render(md, 10_000, theme);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (k, l) in lines.into_iter().filter(|l| !text_of(l).trim().is_empty()).enumerate() {
        if k > 0 {
            spans.push(Span::raw(" "));
        }
        spans.extend(l.spans.into_iter().map(|s| tint_plain(s, base)));
    }
    let mut used = 0;
    let mut out = Vec::new();
    for s in spans {
        let sw = width(&s.content);
        if used + sw > room {
            let left = room.saturating_sub(used);
            if left > 0 {
                out.push(Span::styled(truncate(&s.content, left), s.style));
            }
            break;
        }
        used += sw;
        out.push(s);
    }
    out
}
