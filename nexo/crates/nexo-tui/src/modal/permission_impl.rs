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
                    // the path comes from the model: escape and bidi characters show as markers, never as themselves or as nothing
                    Span::styled(
                        truncate(&crate::render::wrap::one_line(&reveal(path)), w.saturating_sub(12)),
                        theme.style(theme.accent),
                    ),
                    Span::raw("  "),
                    Span::styled(format!("+{added}"), theme.style(theme.success)),
                    Span::raw(" "),
                    Span::styled(format!("-{removed}"), theme.style(theme.error)),
                ]));
                // Wrapped under the gutter, not cut at the edge: the part of a long line that changed is the part
                // being approved. `reveal` shows hidden characters as markers first, so file content cannot hide in the row.
                for row in diff::unified(old, new, *start_line, theme) {
                    if let [gutter, text] = row.spans.as_slice() {
                        let indent = Span::raw(" ".repeat(crate::render::wrap::width(&gutter.content) + 2));
                        lines.extend(crate::render::wrap::wrap_runs(
                            &[(reveal(&text.content).trim_end_matches('\r').to_string(), text.style)],
                            w,
                            std::slice::from_ref(gutter),
                            &[indent],
                        ));
                    } else {
                        lines.push(row);
                    }
                }
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
                lines.extend(wrap_plain(
                    &crate::render::wrap::one_line(&reveal(path)),
                    w,
                    theme.style(theme.accent),
                    &[],
                ));
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
            ToolDisplay::Plain | ToolDisplay::NoBrowser => {}
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
    pub fn help_lines(theme: &Theme, w: usize, query: &str) -> Vec<Line<'static>> {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let q = query.trim().to_lowercase();
        let hit = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        let mut lines = Vec::new();
        for (group, keys) in help_rows(cfg!(target_os = "macos")).iter() {
            // a group's own name keeps all of it
            let all = hit(group);
            let keys: Vec<&(&str, &str)> = keys.iter().filter(|(k, d)| all || hit(k) || hit(d)).collect();
            if keys.is_empty() {
                continue;
            }
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(group.to_string(), bold)));
            for (k, d) in keys {
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
        let mut commands = crate::commands::COMMANDS
            .iter()
            .filter(|s| hit(s.name) || hit(s.description) || s.aliases.iter().any(|a| hit(a)))
            .peekable();
        if commands.peek().is_some() {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled("Commands", bold)));
        }
        for s in commands {
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
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(format!("nothing matches \"{}\"", query.trim()), theme.dim())));
        }
        lines
    }
}

impl Permission {
    /// The body lines without the rules, from the cache when the width, layout and theme are those of the last
    /// draw. The request never changes while the dialog is open, so those three are all the lines depend on.
    pub fn body(&mut self, w: usize, theme: &Theme, compact: bool) -> Vec<Line<'static>> {
        let slot = &mut self.body_cache[usize::from(compact)];
        if let Some(c) = slot.as_ref().filter(|c| c.width == w && c.theme == *theme) {
            return c.lines.clone();
        }
        let lines = Modal::permission_lines_with(&self.req, w, theme, compact, false);
        *slot = Some(BodyCache { width: w, theme: theme.clone(), lines: lines.clone() });
        lines
    }
}

/// `s` with what a terminal would act on or hide made visible, for text the model supplies and the owner approves: a
/// carriage return in the middle of a line (it would write over what comes before it), the other control characters and
/// the bidirectional and zero-width characters that make text read differently from how it is stored. Each shows as its
/// control picture or as `<U+XXXX>`, so the dialog never shows less than what is written. A carriage return at the
/// end of a line (CRLF) is the line ending and stays as it is.
fn reveal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\n' | '\t' => out.push(c),
            '\r' if it.peek().is_none() => out.push(c),
            '\0'..='\u{1f}' => out.push(char::from_u32(0x2400 + c as u32).unwrap_or('\u{fffd}')),
            '\u{7f}' => out.push('\u{2421}'),
            c if c.is_control()
                || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}' | '\u{E0020}'..='\u{E007F}') =>
            {
                out.push_str(&format!("<U+{:04X}>", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}
