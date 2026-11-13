//! Tool and shell output as lines: diffs, folded tails, boxed text.

use super::*;

/// The colour of one line of a diff shown as shell output.
pub(super) fn diff_line_style(line: &str, theme: &Theme) -> Style {
    if line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("diff --git")
        || line.starts_with("index ")
    {
        theme.dim().add_modifier(Modifier::BOLD)
    } else if line.starts_with('+') {
        theme.diff_add_line()
    } else if line.starts_with('-') {
        theme.diff_del_line()
    } else if line.starts_with("@@") {
        theme.style(theme.hunk)
    } else {
        Style::default()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_output(
    out: &ToolOutput,
    name: &str,
    args: &Value,
    w: usize,
    theme: &Theme,
    pad: &[Span<'static>],
    interpreted: bool,
    meta: &mut Meta,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    // a plan reads as the document it is, then what became of it
    if let Some(plan) = (name == "exit_plan").then(|| args.get("plan").and_then(Value::as_str)).flatten() {
        let (plan_lines, mut m) = markdown::render_as_meta(plan, w.saturating_sub(2), theme, true);
        m.regions.clear();
        m.add_deco(2);
        meta.append(m, 0);
        for l in plan_lines {
            let mut spans = pad.to_vec();
            spans.extend(l.spans);
            lines.push(Line::from(spans).style(l.style));
        }
        lines.push(Line::from(pad.to_vec()));
        push_plain(&mut lines, meta, &out.content, w.saturating_sub(2), theme.dim(), pad);
        meta.regions.push(Region::new(Kind::Plan, 0, lines.len() - 1, 2));
        return lines;
    }
    match &out.display {
        ToolDisplay::Todo { items } => {
            for it in items {
                let (icon, style) = todo_style(it.status, theme);
                let mut prefix = pad.to_vec();
                prefix.push(Span::styled(format!("{icon} "), style));
                push_plain(&mut lines, meta, &it.text, w.saturating_sub(2), style, &prefix);
            }
            boxed(meta, Kind::Output, 0, lines.len(), 2);
        }
        ToolDisplay::Diff { old, new, start_line, .. } => {
            let mut region = Region::new(Kind::Diff, 0, 0, 2);
            let (drows, dsrc) = diff::unified_rows(old, new, *start_line, theme);
            let nrows = drows.len();
            for (k, l) in drows.into_iter().enumerate() {
                // a row with a gutter and a sign is content; a hunk header or a rule is not
                if l.spans.len() == 2 {
                    if region.deco == 2 {
                        region.deco += width(&l.spans[0].content) as u16;
                    }
                } else {
                    region.skip.push(k as u32);
                }
                let mut spans = pad.to_vec();
                if let [gutter, text] = &l.spans[..] {
                    // a long row is shortened with an ellipsis, as a file's are
                    spans.push(gutter.clone());
                    spans.extend(crate::render::wrap::clip_spans(
                        vec![text.clone()],
                        w.saturating_sub(width(&gutter.content)),
                    ));
                } else {
                    spans.extend(l.spans);
                }
                lines.push(Line::from(spans));
            }
            region.end = lines.len().saturating_sub(1) as u32;
            if dsrc.len() == nrows
                && dsrc.iter().all(|s| crate::render::wrap::clean(&s.replace('\t', "    ")) == s.replace('\t', "    "))
            {
                region.src = Some(std::sync::Arc::new(dsrc));
            }
            meta.regions.push(region);
        }
        ToolDisplay::File { path, text, start_line } => {
            let lang = code::lang_of_path(path);
            if let Some(view) =
                interpreted.then(|| code::interpret(lang, text.trim_end_matches('\n'), w, theme)).flatten()
            {
                // shown for what it means, the file loses its line numbers
                for l in view {
                    let mut spans = pad.to_vec();
                    spans.extend(l.spans);
                    lines.push(Line::from(spans).style(theme.code_block()));
                }
                boxed(meta, Kind::File, 0, lines.len(), 2);
                return lines;
            }
            let digits = (start_line + text.lines().count()).max(1).to_string().len();
            for (i, l) in code::highlight(text.trim_end_matches('\n'), lang, theme).into_iter().enumerate() {
                let mut spans = pad.to_vec();
                spans.push(Span::styled(format!("{:>digits$}│ ", start_line + i), theme.dim()));
                spans.extend(crate::render::wrap::clip_spans(l.spans, w.saturating_sub(digits + 2)));
                // the file sits on the code background, like a fenced block
                lines.push(Line::from(spans).style(theme.code_block()));
            }
            if !lines.is_empty() {
                let mut r = Region::new(Kind::File, 0, lines.len() - 1, 2 + digits + 2);
                let src: Vec<String> = text.trim_end_matches('\n').split('\n').map(str::to_string).collect();
                if src.len() == lines.len() {
                    r.src = Some(std::sync::Arc::new(src));
                }
                meta.regions.push(r);
            }
        }
        ToolDisplay::Shell { command, stdout, stderr, .. } => {
            lines.extend(command_lines(command, w, theme, pad));
            let first = lines.len();
            let text = if stderr.trim().is_empty() {
                stdout.clone()
            } else if stdout.trim().is_empty() {
                stderr.clone()
            } else {
                format!("{stdout}\n{stderr}")
            };
            push_plain(&mut lines, meta, text.trim_end_matches('\n'), w, Style::default(), pad);
            boxed(meta, Kind::Output, first, lines.len(), 2);
            keep_source(meta, first, lines.len(), text.trim_end_matches('\n'));
        }
        ToolDisplay::List { items } | ToolDisplay::Remembered { items, .. } => {
            for it in items {
                push_plain(&mut lines, meta, it, w, Style::default(), pad);
            }
            boxed(meta, Kind::Output, 0, lines.len(), 2);
        }
        ToolDisplay::Plain => {
            let style = if out.is_error { theme.style(theme.error) } else { Style::default() };
            let text = if out.content.trim().is_empty() {
                if out.is_error {
                    "(error)".to_string()
                } else {
                    "(no output)".to_string()
                }
            } else {
                out.content.trim_end_matches('\n').to_string()
            };
            let _ = (name, args);
            if !out.is_error && crate::render::markdown::looks_markdown(&text) {
                let (b, mut m) = markdown::md_lines_meta(&text, w, theme, style, false, pad);
                m.shift(lines.len());
                meta.append(m, 0);
                lines.extend(b);
            } else {
                push_plain(&mut lines, meta, &text, w, style, pad);
            }
            boxed(meta, Kind::Output, 0, lines.len(), 2);
        }
    }
    lines
}

/// The box that starts at row `from` keeps `raw` as its source when it maps one to one onto its rows
/// (nothing wrapped), so a copy has the text as it was written, tabs included.
pub(super) fn keep_source(meta: &mut Meta, from: usize, to: usize, raw: &str) {
    if let Some(r) = meta.regions.iter_mut().rev().find(|r| r.start as usize == from) {
        let src: Vec<String> = raw.split('\n').map(str::to_string).collect();
        let plain = crate::render::wrap::clean(&raw.replace('\t', "    ")) == raw.replace('\t', "    ");
        if plain && r.src.is_none() && src.len() == to - from {
            r.src = Some(std::sync::Arc::new(src));
        }
    }
}

/// Rows `from..to` of a body as one box of `kind`, `deco` columns of decoration deep.
pub(super) fn boxed(meta: &mut Meta, kind: Kind, from: usize, to: usize, deco: usize) {
    if to > from {
        meta.regions.push(Region::new(kind, from, to - 1, deco));
    }
}

/// `text` wrapped under `prefix` and appended to `lines`, its wrapped rows noted in `meta`.
pub(super) fn push_plain(
    lines: &mut Vec<Line<'static>>,
    meta: &mut Meta,
    text: &str,
    w: usize,
    style: Style,
    prefix: &[Span<'static>],
) {
    let (l, conts) = wrap_plain_ex(text, w, style, prefix);
    let base = lines.len() as u32;
    meta.conts.extend(conts.into_iter().map(|c| crate::regions::Cont { line: c.line + base, ..c }));
    lines.extend(l);
}

/// Lines of a folded running card's output that stay in sight.
pub(super) const LIVE_TAIL: usize = 4;
/// Lines of a folded failure that stay in sight.
pub(super) const FAILED_PREVIEW: usize = 4;

/// `$ command`, every line of it, wrapped to the width.
pub(super) fn command_lines(command: &str, w: usize, theme: &Theme, pad: &[Span<'static>]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, l) in code::highlight(command.trim_end(), "bash", theme).into_iter().enumerate() {
        let mut first = pad.to_vec();
        first.push(Span::styled(if i == 0 { "$ " } else { "  " }, theme.style(theme.accent)));
        let mut cont = pad.to_vec();
        cont.push(Span::raw("  "));
        let runs: Vec<(String, Style)> = l.spans.into_iter().map(|s| (s.content.into_owned(), s.style)).collect();
        lines.extend(crate::render::wrap::wrap_runs(&runs, w, &first, &cont));
    }
    lines
}
