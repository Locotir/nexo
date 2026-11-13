//! The title, the buttons and the two-column layout of a permission dialog.

use super::*;

/// Columns of a sub-agent's name the title of a permission dialog keeps.
pub(super) const TITLE_AGENT_COLS: usize = 16;

/// The title of a permission dialog within `room` columns: ` permission · bash `, and for a sub-agent
/// ` permission · <agent> · bash ` with the agent's name cut (with an ellipsis) to fit. The main agent's
/// title never changes; when not even a short name fits, the sub-agent's is the same short title.
pub fn permission_title(agent: Option<&str>, tool: &str, room: usize) -> String {
    let plain = format!(" permission · {tool} ");
    let Some(agent) = agent else { return plain };
    let fixed = crate::render::wrap::width(&plain) + 3;
    let cols = room.saturating_sub(fixed).min(TITLE_AGENT_COLS);
    if cols < 2 {
        return plain;
    }
    format!(" permission · {} · {tool} ", truncate(agent, cols))
}

/// The permission buttons that fit `avail` columns: (label, key), and the gap between them.
/// Full words when there is room, then closer together, then the keys alone.
pub fn permission_buttons(avail: usize) -> (Vec<(String, char)>, usize) {
    let sets: [(&[(&str, char)], usize); 4] = [
        (&[("[y] allow", 'y'), ("[a] always", 'a'), ("[n] deny", 'n')], 3),
        (&[("[y] allow", 'y'), ("[a] always", 'a'), ("[n] deny", 'n')], 1),
        (&[("[y] yes", 'y'), ("[a] always", 'a'), ("[n] no", 'n')], 1),
        (&[("[y]", 'y'), ("[a]", 'a'), ("[n]", 'n')], 1),
    ];
    for (set, gap) in sets {
        let need: usize = set.iter().map(|(l, _)| l.chars().count()).sum::<usize>() + gap * (set.len() - 1);
        if need <= avail {
            return (set.iter().map(|(l, k)| (l.to_string(), *k)).collect(), gap);
        }
    }
    (vec![("[y]".to_string(), 'y'), ("[a]".to_string(), 'a'), ("[n]".to_string(), 'n')], 1)
}

/// Whether `path` names a markdown file.
pub(super) fn is_markdown_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".md") || p.ends_with(".markdown")
}

/// `left` in a column `col` wide, `desc` wrapped beside it; `tail` after the last line.
pub(super) fn two_columns(
    left: Span<'static>,
    desc: &str,
    tail: &[Span<'static>],
    col: usize,
    w: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let desc_w = w.saturating_sub(col).max(12);
    let mut body = crate::render::markdown::md_lines(desc, desc_w, theme, Style::default(), false, &[]);
    if body.is_empty() {
        body.push(Line::default());
    }
    if let Some(last) = body.last_mut() {
        let used: usize = last.spans.iter().map(|s| crate::render::wrap::width(&s.content)).sum();
        let tail_w: usize = tail.iter().map(|s| crate::render::wrap::width(&s.content)).sum();
        if used + tail_w <= desc_w {
            last.spans.extend(tail.iter().cloned());
        } else if !tail.is_empty() {
            body.push(Line::from(tail.to_vec()));
        }
    }
    let lw = crate::render::wrap::width(&left.content);
    let mut out = Vec::new();
    let mut rows = body.into_iter();
    if lw < col {
        let mut first = vec![left.clone(), Span::raw(" ".repeat(col - lw))];
        first.extend(rows.next().map(|l| l.spans).unwrap_or_default());
        out.push(Line::from(first));
    } else {
        out.push(Line::from(left));
    }
    for l in rows {
        let mut v = vec![Span::raw(" ".repeat(col))];
        v.extend(l.spans);
        out.push(Line::from(v));
    }
    out
}
