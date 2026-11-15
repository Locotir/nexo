//! The pane: the selected row told in full. Its header (name, kind, model, mode) and state line, then what
//! its kind has to say (a lane's clock, spend and rounds; a session's goal, context, cost, todo, tests and
//! mail; a job's title and the end of its output), then its events, newest first. It scrolls as one page.

use nexo_agent::supervise::{EvDetail, EvKind};

use super::row::{bar, clock_of, money, short_model, tilde};
use super::*;
use crate::panel_fx as px;

/// Cells of the spend and context bars in the pane.
const PANE_BAR: usize = 9;
/// Lines of a job's output shown in the pane.
const TAIL_LINES: usize = 12;

/// `TestsPass` as `tests pass`.
fn kind_word(kind: EvKind) -> String {
    let mut out = String::new();
    for c in format!("{kind:?}").chars() {
        if c.is_uppercase() && !out.is_empty() {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// What an event carries, as a few words.
fn detail_words(d: &EvDetail) -> String {
    match d {
        EvDetail::None => String::new(),
        EvDetail::Progress { done, total } => format!("{done}/{total}"),
        EvDetail::Tests { pass, fail } => format!("{pass} passed, {fail} failed"),
        EvDetail::Usd(n) => format!("${n:.4}"),
        EvDetail::Tokens(n) => format!("{n} tokens"),
        EvDetail::Peer(s) | EvDetail::Text(s) => crate::render::wrap::one_line(s),
        EvDetail::Wait { secs } => format!("{secs}s"),
    }
}

/// `2h`, `30m`, `45s`: a lane's clock.
fn every_words(secs: u64) -> String {
    if secs >= 3600 && secs % 3600 == 0 {
        format!("{}h", secs / 3600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

impl AgentsView {
    /// A label and its value on one line: `label  value`.
    fn field(label: &str, value: Vec<Span<'static>>, theme: &Theme) -> Line<'static> {
        let mut spans = vec![Span::styled(format!("{label:<8}"), theme.dim2())];
        spans.extend(value);
        Line::from(spans)
    }

    /// The pane's lines for `row` in `w` columns.
    pub(super) fn pane_lines(&self, row: &AgentRow, kind: Kind, w: usize, theme: &Theme, now: Instant, fx: bool) -> Vec<Line<'static>> {
        let a = self.ascii;
        let mut out: Vec<Line<'static>> = Vec::new();
        let quiet = age((self.clock - row.since).max(0) as u64);
        // header: name · kind · model · mode
        let model = row.model.as_deref().or(row.lane.as_ref().and_then(|l| l.model.as_deref()));
        let mode = row.permission_mode.as_deref().or(row.lane.as_ref().and_then(|l| l.mode.as_deref()));
        let mut head = vec![Span::styled(row.name.clone(), theme.bold(theme.fg))];
        let mut tail = vec![kind.word().to_string()];
        if kind == Kind::Other {
            tail[0] = row.program.clone();
        }
        tail.extend(model.map(str::to_string));
        tail.extend(mode.map(str::to_string));
        head.push(Span::styled(format!(" · {}", tail.join(" · ")), theme.dim()));
        out.push(Line::from(head));
        // the state line, with its time
        let (g, gc, _) = self.glyph(row, theme, now, false);
        let mut state = vec![Span::styled(format!("{g} "), theme.style(gc))];
        if let Some(l) = &row.lane {
            let words = match l.state.as_str() {
                "running" => format!("round {} running {quiet}", l.rounds + 1),
                "idle" => "idle".to_string(),
                s => s.to_string(),
            };
            state.push(Span::styled(words, theme.style(gc)));
            let next = match (l.next_round, l.every_secs) {
                (Some(n), Some(e)) => format!("next {} (every {})", clock_of(n), every_words(e)),
                (Some(n), None) => format!("next {}", clock_of(n)),
                (None, Some(e)) => format!("every {}", every_words(e)),
                (None, None) => "runs once, then answers mail".to_string(),
            };
            state.push(Span::styled(format!("    {next}"), theme.dim()));
        } else {
            let words = if row.paused { "supervision paused".to_string() } else { row.state.label().to_string() };
            state.push(Span::styled(format!("{words} {quiet}"), theme.style(gc)));
            if let Some(s) = row.retry_secs {
                state.push(Span::styled(format!("    retry in {s}s"), theme.style(theme.warn)));
            }
        }
        out.push(Line::from(state));
        match &row.lane {
            Some(l) => self.lane_lines(&mut out, row, l, theme, fx),
            None => match kind {
                Kind::Sub | Kind::Job => self.job_lines(&mut out, row, w, theme),
                _ => self.session_lines(&mut out, row, theme, fx),
            },
        }
        // its events, newest first, under a rule as wide as the pane (`── events ` takes ten cells)
        let rule_w = w.saturating_sub(10);
        out.push(Line::from(Span::styled(
            format!("{} events {}", if a { "--" } else { "──" }, (if a { "-" } else { "─" }).repeat(rule_w)),
            theme.dim2(),
        )));
        if row.ribbon.is_empty() {
            out.push(Line::from(Span::styled("no events seen yet", theme.dim())));
        }
        for e in row.ribbon.iter().rev() {
            let repeat = if e.n > 1 { format!(" ×{}", e.n) } else { String::new() };
            let d = detail_words(&e.detail);
            let d = if d.is_empty() { String::new() } else { format!("  {d}") };
            let color = match e.kind {
                EvKind::Error | EvKind::TestsFail => theme.error,
                EvKind::TestsPass => theme.success,
                EvKind::PermissionWait | EvKind::Question => theme.warn2,
                _ => theme.fg,
            };
            out.push(Line::from(vec![
                Span::styled(format!("{} ", clock_of(e.at)), theme.dim2()),
                Span::styled(format!("{} ", px::glyph(e.kind, a)), theme.style(color)),
                Span::styled(format!("{}{repeat}", kind_word(e.kind)), theme.style(color)),
                Span::styled(d, theme.dim()),
            ]));
        }
        out
    }

    fn lane_lines(&self, out: &mut Vec<Line<'static>>, row: &AgentRow, l: &nexo_agent::supervise::LaneInfo, theme: &Theme, fx: bool) {
        let a = self.ascii;
        let spend = self.rolled(&row.key, l.spend_today, fx);
        let spent = match l.cap_day.filter(|c| *c > 0.0) {
            Some(cap) => {
                let frac = spend / cap;
                vec![
                    Span::styled(bar(frac, PANE_BAR, a), theme.style(theme.level_color((frac * 100.0) as u64))),
                    Span::styled(format!(" ${spend:.2} of ${} today", money(cap)), theme.style(theme.money)),
                ]
            }
            None => vec![Span::styled(format!("${spend:.2} today · no cap"), theme.style(theme.money))],
        };
        out.push(Self::field("spend", spent, theme));
        // the rounds we know: the one before the failures in a row went well, those failed, one may run now
        let mut marks: Vec<Span<'static>> = Vec::new();
        let ok = if a { "+" } else { "✓" };
        let bad = if a { "x" } else { "✗" };
        let failed = (l.failures as u64).min(l.rounds);
        if l.rounds > failed {
            marks.push(Span::styled(format!("{ok} "), theme.style(theme.success)));
        }
        for _ in 0..failed.min(5) {
            marks.push(Span::styled(format!("{bad} "), theme.style(theme.error)));
        }
        if l.state == "running" {
            marks.push(Span::styled(if a { "* " } else { "● " }, theme.style(theme.success)));
        }
        let word = if l.rounds == 1 { "round" } else { "rounds" };
        marks.push(Span::styled(format!(" {} {word}", l.rounds), theme.dim()));
        if let Some((at, result, cost)) = &l.last_round {
            let c = if result == "ok" { theme.success } else { theme.error };
            marks.push(Span::styled(format!("   last {result}", ), theme.style(c)));
            marks.push(Span::styled(format!(" {} · ${cost:.2}", self.since_action(*at)), theme.dim()));
        }
        out.push(Self::field("rounds", marks, theme));
        if let Some(why) = &l.why {
            out.push(Self::field("why", vec![Span::styled(why.clone(), theme.style(theme.error))], theme));
        }
        if l.state == "running" {
            if let Some(step) = &row.last_step {
                out.push(Self::field("now", vec![Span::styled(format!("▸ {step}"), theme.style(theme.fg))], theme));
            }
        }
        if !l.dir.is_empty() {
            out.push(Self::field("in", vec![Span::styled(tilde(&l.dir), theme.dim())], theme));
        }
        if row.unread + row.held > 0 {
            out.push(Self::field("mail", self.mail_words(row, theme), theme));
        }
        if let Some(r) = self.receipt(row, theme) {
            out.push(Self::field("sent", vec![r], theme));
        }
    }

    fn mail_words(&self, row: &AgentRow, theme: &Theme) -> Vec<Span<'static>> {
        let mut v = vec![Span::styled(format!("{} unread", row.unread), theme.style(theme.accent2))];
        if row.held > 0 {
            v.push(Span::styled(format!(" · {} held for you", row.held), theme.style(theme.warn2)));
        }
        v
    }

    fn session_lines(&self, out: &mut Vec<Line<'static>>, row: &AgentRow, theme: &Theme, fx: bool) {
        let a = self.ascii;
        if let Some(g) = &row.goal {
            out.push(Self::field("goal", vec![Span::styled(g.clone(), theme.style(theme.fg))], theme));
        }
        if let Some(p) = row.ctx_pct {
            out.push(Self::field(
                "context",
                vec![
                    Span::styled(bar(p as f64 / 100.0, PANE_BAR, a), theme.style(theme.level_color(p as u64))),
                    Span::styled(format!(" {p}%"), theme.dim()),
                ],
                theme,
            ));
        }
        if let Some(c) = row.cost_usd {
            let c = self.rolled(&row.key, c, fx);
            out.push(Self::field("cost", vec![Span::styled(format!("${c:.2}"), theme.style(theme.money))], theme));
        }
        if let Some((d, t)) = row.todo.filter(|(_, t)| *t > 0) {
            let frac = self.live.todo_shown.get(&row.key).copied().filter(|_| fx).unwrap_or(d as f64 / t as f64);
            out.push(Self::field(
                "todo",
                vec![
                    Span::styled(bar(frac, PANE_BAR, a), theme.style(theme.accent2)),
                    Span::styled(format!(" {d}/{t}"), theme.dim()),
                ],
                theme,
            ));
        }
        if let Some((p, fl)) = row.tests {
            let mut v = vec![Span::styled(format!("{} {p} passed", if a { "+" } else { "✔" }), theme.style(theme.success))];
            if fl > 0 {
                v.push(Span::styled(format!("  {} {fl} failed", if a { "x" } else { "✘" }), theme.style(theme.error)));
            }
            out.push(Self::field("tests", v, theme));
        }
        if row.unread + row.held > 0 {
            out.push(Self::field("mail", self.mail_words(row, theme), theme));
        }
        if let Some(r) = self.receipt(row, theme) {
            out.push(Self::field("sent", vec![r], theme));
        }
        if let Some(s) = &row.last_step {
            out.push(Self::field("now", vec![Span::styled(format!("▸ {s}"), theme.style(theme.fg))], theme));
        }
        if let Some(c) = &row.last_conclusion {
            out.push(Self::field("last", vec![Span::styled(c.clone(), theme.dim())], theme));
        }
        if let Some((what, at)) = &row.last_action {
            out.push(Self::field(
                "acted",
                vec![Span::styled(format!("{what} · {}", self.since_action(*at)), theme.style(theme.accent2))],
                theme,
            ));
        }
        for c in self.coordination(row) {
            out.push(Self::field("team", vec![Span::styled(c, theme.dim())], theme));
        }
        if let Some(c) = &row.cwd {
            out.push(Self::field("in", vec![Span::styled(tilde(c), theme.dim())], theme));
        }
    }

    fn job_lines(&self, out: &mut Vec<Line<'static>>, row: &AgentRow, w: usize, theme: &Theme) {
        if let Some(s) = &row.last_step {
            out.push(Self::field("step", vec![Span::styled(s.clone(), theme.style(theme.fg))], theme));
        }
        if let Some(m) = &row.model {
            out.push(Self::field("model", vec![Span::styled(short_model(m).to_string(), theme.dim())], theme));
        }
        if let Some(c) = row.cost_usd.filter(|c| *c >= 0.005) {
            out.push(Self::field("cost", vec![Span::styled(format!("${c:.2}"), theme.style(theme.money))], theme));
        }
        if let Some(c) = &row.last_conclusion {
            out.push(Self::field("last", vec![Span::styled(c.clone(), theme.dim())], theme));
        }
        let job = (Kind::of(row) == Kind::Sub).then(|| row.key.rsplit_once('.').and_then(|(_, n)| n.parse::<u32>().ok())).flatten();
        if let Some(id) = job {
            out.push(Line::from(Span::styled(format!("output of job {id}"), theme.dim2())));
            match self.tails.get(&id) {
                Some(t) if !t.trim().is_empty() => {
                    let lines: Vec<&str> = t.lines().collect();
                    for l in &lines[lines.len().saturating_sub(TAIL_LINES)..] {
                        out.push(Line::from(Span::styled(truncate(&crate::render::wrap::one_line(l), w), theme.style(theme.fg))));
                    }
                }
                Some(_) => out.push(Line::from(Span::styled("no output yet", theme.dim()))),
                None => out.push(Line::from(Span::styled("reading…", theme.dim()))),
            }
        }
    }

    /// Draws the pane for the selected line in `area`: a row told in full, a group's header summed up, or the
    /// questions of a new lane.
    pub(super) fn draw_pane(&mut self, f: &mut Frame, area: Rect, theme: &Theme, now: Instant, fx: bool) {
        self.pane_area = area;
        let w = area.width as usize;
        let lines: Vec<Line<'static>> = if self.form.is_some() {
            self.form_lines(w, theme)
        } else {
            match self.selected_item() {
                Some(tree::Item::Row { row, kind, .. }) => self.pane_lines(&row, kind, w, theme, now, fx),
                Some(tree::Item::Head { group, count, .. }) => self.group_lines(&group, count, theme),
                // nothing to tell yet (the tree says why): the pane says what each group will hold
                None => LEGEND
                    .iter()
                    .map(|(g, what)| {
                        Line::from(vec![
                            Span::styled(format!("{g:<15}"), theme.bold(theme.dim)),
                            Span::styled(*what, theme.dim()),
                        ])
                    })
                    .collect(),
            }
        };
        let lines: Vec<Line<'static>> = lines
            .into_iter()
            .flat_map(|l| {
                let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                if width(&text) <= w || w == 0 {
                    vec![l]
                } else {
                    // a long line wraps in the colour of its last part (a goal, a reason, a path)
                    let st = l.spans.last().map_or(theme.style(theme.fg), |s| s.style);
                    crate::render::wrap::wrap_plain(&text, w.max(1), st, &[])
                }
            })
            .collect();
        self.pane_max = lines.len().saturating_sub(area.height as usize);
        self.pane_scroll = self.pane_scroll.min(self.pane_max);
        let shown: Vec<Line<'static>> = lines.into_iter().skip(self.pane_scroll).take(area.height as usize).collect();
        f.render_widget(Paragraph::new(Text::from(shown)), area);
        if self.pane_max > 0 && area.height > 1 {
            let mark = if self.pane_scroll >= self.pane_max { "↑ more" } else { "↓ more" };
            let mark = if self.ascii { if self.pane_scroll >= self.pane_max { "^ more" } else { "v more" } } else { mark };
            let x = area.x + area.width.saturating_sub(width(mark) as u16);
            f.render_widget(
                Paragraph::new(Span::styled(mark, theme.style(theme.accent2))),
                Rect::new(x, area.y + area.height - 1, width(mark) as u16, 1),
            );
        }
    }

    /// What a group's header says in the pane: its members by state.
    fn group_lines(&self, group: &Group, count: usize, theme: &Theme) -> Vec<Line<'static>> {
        let mut out = vec![Line::from(Span::styled(group.title(), theme.bold(theme.accent)))];
        let word = if count == 1 { "row" } else { "rows" };
        out.push(Line::from(Span::styled(format!("{count} {word}"), theme.dim())));
        if *group == Group::Lanes {
            out.push(Line::from(Span::styled(self.lanes_summary(), theme.style(theme.money))));
            let paused = self.lanes_paused;
            out.push(Line::from(Span::styled(
                if paused { "all lanes are paused: p resumes them" } else { "p pauses every lane · a adds a lane" },
                theme.dim(),
            )));
        }
        if *group == Group::Needs {
            out.push(Line::from(Span::styled(
                "waiting for permission or an answer, looping, a failed lane, or mail held for you",
                theme.dim(),
            )));
        }
        out.push(Line::from(Span::styled("← → or Space fold this group", theme.dim2())));
        out
    }
}
