//! A skill load is an instruction hand-off, not an ordinary command. Keep its
//! body behind an explicit unfold, including on replay and on failures.
use super::*;

pub(super) fn card(b: &Block, w: usize, theme: &Theme, o: RenderOpts, meta: &mut Meta) -> Vec<Line<'static>> {
    let Block::Tool { args, status, output, started, folded, .. } = b else { return Vec::new() };
    let (label, icon, colour) = match status {
        ToolStatus::Pending => ("queued", "◇", theme.dim2),
        ToolStatus::Running => ("loading", "✦", theme.accent),
        ToolStatus::Done => ("loaded", "✦", theme.success),
        ToolStatus::Failed => ("error", "✗", theme.error),
        ToolStatus::Denied => ("error · denied", "⊘", theme.warn),
    };
    let name = args.get("name").and_then(Value::as_str).unwrap_or("Unknown");
    // Do not let a tool argument create extra transcript rows or terminal controls.
    let name: String = name.chars().filter(|c| !c.is_control()).take(160).collect();
    let suffix = format!(" | {label}");
    let prefix = format!("{} {icon} Skill · ", if *folded { "▸" } else { "▾" });
    let room = w.saturating_sub(width(&prefix) + width(&suffix));
    let name = truncate(&name, room);
    let mut spans = vec![Span::styled(prefix, theme.style(colour).add_modifier(Modifier::BOLD))];
    // A comet crosses the name, leaving a soft trail. Reuse the existing live-card
    // beat/cache invalidation: no timer, retained animation state, or idle work.
    let animate = o.fx && *status == ToolStatus::Running && started.is_some();
    let count = name.chars().count();
    let head = o.spinner % (count + 5);
    for (i, c) in name.chars().enumerate() {
        let distance = head.abs_diff(i);
        let glow = if animate { (1.0 - distance as f32 / 4.0).max(0.0) } else { 0.0 };
        let style = theme.style(mix(theme.fg, theme.accent, glow));
        spans.push(Span::styled(c.to_string(), if glow > 0.7 { style.add_modifier(Modifier::BOLD) } else { style }));
    }
    spans.push(Span::styled(suffix, theme.style(colour)));
    let mut lines = vec![Line::from(spans)];
    // A very narrow pane still has a bounded, single-line header.
    if width(&markdown::text_of(&lines[0])) > w {
        lines[0] = Line::from(Span::styled(truncate(&markdown::text_of(&lines[0]), w), theme.style(colour)));
    }
    if !*folded {
        if let Some(out) = output {
            let pad = [Span::styled("│ ", theme.dim2())];
            let mut body_meta = Meta::default();
            let body =
                render_output(out, "skill", args, w.saturating_sub(2), theme, &pad, o.interpreted, &mut body_meta);
            body_meta.shift(1);
            *meta = body_meta;
            lines.extend(body);
            close_guide(&mut lines);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> RenderOpts {
        RenderOpts {
            show_reasoning: true,
            fold_after: 3,
            spinner: 0,
            intro_ms: None,
            interpreted: false,
            timestamps: true,
            fx: false,
            detail: nexo_core::config::Detail::Full,
        }
    }

    fn skill(status: ToolStatus) -> Block {
        Block::Tool {
            id: "s".into(),
            name: "skill".into(),
            summary: "private summary".into(),
            args: serde_json::json!({"name":"Verify", "arguments":"private arguments"}),
            status,
            progress: vec!["private progress".into()],
            output: Some(ToolOutput::ok("PRIVATE BODY\n".repeat(10000))),
            started: Some(Instant::now()),
            elapsed_ms: None,
            folded: true,
        }
    }

    #[test]
    fn skill_states_are_real_and_body_is_always_opt_in() {
        let theme = Theme::dark();
        for (status, label) in [
            (ToolStatus::Pending, "queued"),
            (ToolStatus::Running, "loading"),
            (ToolStatus::Done, "loaded"),
            (ToolStatus::Failed, "error"),
            (ToolStatus::Denied, "error · denied"),
        ] {
            let mut b = skill(status);
            let rows = b.render(80, &theme, opts());
            assert_eq!(rows.len(), 1);
            let text = markdown::text_of(&rows[0]);
            assert!(text.contains(&format!("Skill · Verify | {label}")), "{text}");
            assert!(!text.to_lowercase().contains("private"));
            if let Block::Tool { folded, .. } = &mut b {
                *folded = false;
            }
            assert!(b.render(80, &theme, opts()).len() > 1);
        }
    }

    #[test]
    fn skill_comet_respects_reduced_motion_and_replay() {
        let mut b = skill(ToolStatus::Running);
        let theme = Theme::dark();
        let options = opts();
        let render = |b: &Block, fx, spinner| b.render(80, &theme, RenderOpts { fx, spinner, ..options });
        assert_eq!(render(&b, false, 0), render(&b, false, 5));
        assert_ne!(render(&b, true, 0), render(&b, true, 5));
        if let Block::Tool { started, .. } = &mut b {
            *started = None;
        }
        assert_eq!(render(&b, true, 0), render(&b, true, 5));
        b = skill(ToolStatus::Done);
        assert_eq!(render(&b, true, 0), render(&b, true, 5));
        for w in 8..90 {
            for line in b.render(w, &theme, options) {
                assert!(width(&markdown::text_of(&line)) <= w);
            }
        }
    }
}
