//! The body lines of a permission dialog and the help page.

use super::*;

impl Modal {
    /// The body lines of a permission dialog at `w` columns.
    pub fn permission_lines(req: &PermissionRequest, w: usize, theme: &Theme) -> Vec<Line<'static>> {
        Self::permission_lines_with(req, w, theme, false, true)
    }

    /// `compact`: without the blank lines between the parts, for a screen too short to show all.
    pub fn permission_lines_with(
        req: &PermissionRequest,
        w: usize,
        theme: &Theme,
        compact: bool,
        with_rules: bool,
    ) -> Vec<Line<'static>> {
        let gap = |lines: &mut Vec<Line<'static>>| {
            if !compact {
                lines.push(Line::default());
            }
        };
        let mut lines = vec![Line::from(vec![
            Span::styled(req.tool.clone(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::raw(truncate(
                &crate::render::wrap::clean(
                    req.summary
                        .strip_prefix(req.tool.as_str())
                        .map(|r| r.trim_start_matches(':').trim_start())
                        .unwrap_or(req.summary.as_str()),
                ),
                w.saturating_sub(req.tool.len() + 2),
            )),
        ])];
        gap(&mut lines);
        match &req.detail {
            ToolDisplay::Diff { path, old, new, start_line } => {
                let (added, removed) = diff::stats(old, new);
                lines.push(Line::from(vec![
                    Span::styled(truncate(path, w.saturating_sub(12)), theme.style(theme.accent)),
                    Span::raw("  "),
                    Span::styled(format!("+{added}"), theme.style(theme.success)),
                    Span::raw(" "),
                    Span::styled(format!("-{removed}"), theme.style(theme.error)),
                ]));
                lines.extend(diff::unified(old, new, *start_line, theme));
            }
            ToolDisplay::Shell { command, .. } => {
                // All of it, wrapped: what is being allowed must be seen whole.
                let dollar = [Span::styled("$ ", theme.style(theme.accent))];
                for (i, l) in command.lines().enumerate() {
                    let first: &[Span<'static>] = if i == 0 { &dollar } else { &[Span::raw("  ")] };
                    lines.extend(crate::render::wrap::wrap_runs(
                        &[(l.to_string(), Style::default())],
                        w,
                        first,
                        &[Span::raw("  ")],
                    ));
                }
            }
            ToolDisplay::File { path, text, .. } => {
                lines.push(Line::from(Span::styled(path.clone(), theme.style(theme.accent))));
                if is_markdown_path(path) {
                    lines.extend(crate::render::markdown::md_lines(text, w, theme, Style::default(), false, &[]));
                } else {
                    lines.extend(wrap_plain(text, w, Style::default(), &[]));
                }
            }
            ToolDisplay::List { items } | ToolDisplay::Remembered { items, .. } => {
                for it in items {
                    // what an edit of several files replaces: `- old` and `+ new` read as a diff does;
                    // a path, or the count of what is left, stays plain
                    let style = match (req.tool == "edit", it.as_str()) {
                        (true, t) if t.starts_with("- ") => theme.diff_del_line(),
                        (true, t) if t.starts_with("+ ") => theme.diff_add_line(),
                        (true, t) if t.starts_with("+") => theme.dim(),
                        _ => Style::default(),
                    };
                    lines.extend(wrap_plain(it, w, style, &[]));
                }
            }
            ToolDisplay::Todo { items } => {
                for it in items {
                    lines.extend(crate::render::todo::todo_row(it, w, theme, Default::default()));
                }
            }
            ToolDisplay::Plain => {}
        }
        if with_rules {
            gap(&mut lines);
            lines.extend(Self::permission_rules(req, theme, true));
        }
        lines
    }

    /// The rules the window offers for "allow always": a header (when `header`) and one line each.
    pub fn permission_rules(req: &PermissionRequest, theme: &Theme, header: bool) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        if req.suggested_rules.is_empty() {
            return lines;
        }
        if header {
            lines.push(Line::from(Span::styled("Allow always with a rule:", theme.dim())));
        }
        for (i, r) in req.suggested_rules.iter().take(9).enumerate() {
            lines.push(Line::from(vec![
                Span::styled(format!("  {}  ", i + 1), theme.style(theme.accent)),
                Span::raw(crate::render::wrap::clean(r).into_owned()),
            ]));
        }
        lines
    }

    /// The help at `w` columns: keys and commands in two columns, the descriptions wrapped
    /// under their own column (a long command takes a line of its own).
    pub fn help_lines(theme: &Theme, w: usize) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut lines = Vec::new();
        for (i, (group, keys)) in HELP.iter().enumerate() {
            if i > 0 {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(group.to_string(), bold)));
            for (k, d) in keys.iter() {
                lines.extend(two_columns(
                    Span::styled(k.to_string(), theme.style(theme.accent)),
                    d,
                    &[],
                    (w / 3).clamp(12, 19),
                    w,
                    theme,
                ));
            }
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Commands", bold)));
        for s in crate::commands::COMMANDS {
            let left = if s.hint.is_empty() { format!("/{}", s.name) } else { format!("/{} {}", s.name, s.hint) };
            let also = if s.aliases.is_empty() {
                Vec::new()
            } else {
                let a: Vec<String> = s.aliases.iter().map(|a| format!("/{a}")).collect();
                vec![Span::styled(format!("  ({})", a.join(" ")), theme.dim())]
            };
            lines.extend(two_columns(
                Span::styled(left, theme.style(theme.accent)),
                s.description,
                &also,
                24,
                w,
                theme,
            ));
        }
        lines
    }
}
