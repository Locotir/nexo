//! A click on `◎ goal` in the status bar: the goal in a small window. The person's own words and the agent's
//! restatement of them, where the goal stands (round, state), how long it has run, what it spent, the last thing
//! it did and every time it stopped short, with the reason. A paused or met goal has a Resume button.

use crossterm::event::{KeyCode, KeyEvent};
use nexo_core::agent::{AgentCommand, GoalLog, GoalState};
use nexo_core::mailbox::now_ms;
use ratatui::text::{Line, Span};

use crate::app::Goal;
use crate::modal::ModalOut;
use crate::render::wrap::{truncate, width, wrap_plain};
use crate::theme::Theme;

#[derive(Debug, Clone)]
pub struct GoalView {
    goal: Goal,
    log: GoalLog,
    pub scroll: usize,
    /// The second the clock was last drawn at, to redraw when it moves on.
    shown_s: i64,
}

impl GoalView {
    pub fn new(goal: Goal, log: GoalLog) -> GoalView {
        GoalView { goal, log, scroll: 0, shown_s: now_ms() / 1000 }
    }

    /// The agent told the goal again: the window follows it.
    pub fn set(&mut self, goal: Goal, log: GoalLog) {
        self.goal = goal;
        self.log = log;
    }

    /// Whether the clock moved on since the last draw (only a goal at work has one that runs).
    pub fn tick(&mut self) -> bool {
        let s = now_ms() / 1000;
        let moved = s != self.shown_s;
        self.shown_s = s;
        moved && self.goal.state == GoalState::Active
    }

    /// What the size of the window depends on: the words it holds, the stops, the button, the longest line.
    pub fn size_key(&self) -> (bool, usize, usize, bool, usize) {
        let l = &self.log;
        let words = l.original.as_deref().map_or(0, str::len)
            + l.brief.as_deref().map_or(0, str::len)
            + self.goal.text.len()
            + match &self.goal.state {
                GoalState::Paused(why) => why.len(),
                _ => 0,
            };
        (self.can_resume(), l.stops.len(), words, l.last.is_some(), self.natural_width())
    }

    /// The columns the window wants so its longest line sits on one row: that line, its indent and the frame.
    pub fn natural_width(&self) -> usize {
        let l = &self.log;
        let longest = |s: &str| s.lines().map(width).max().unwrap_or(0);
        let mut most = longest(&self.goal.text);
        for s in [l.original.as_deref(), l.brief.as_deref(), l.last.as_deref()].into_iter().flatten() {
            most = most.max(longest(s));
        }
        for stop in &l.stops {
            // "12 min ago · " leads each stop
            most = most.max(longest(&stop.why) + 13);
        }
        most + 7
    }

    /// A goal that stopped (paused) or was met can go on from here.
    pub fn can_resume(&self) -> bool {
        matches!(self.goal.state, GoalState::Paused(_) | GoalState::Done)
    }

    pub fn key(&mut self, key: KeyEvent) -> ModalOut {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return ModalOut::Close,
            KeyCode::Char('r') | KeyCode::Enter if self.can_resume() => return self.resume(),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = usize::MAX / 2,
            _ => {}
        }
        ModalOut::Nothing
    }

    /// The Resume button: the goal goes on and the window closes.
    pub fn resume(&self) -> ModalOut {
        ModalOut::SendAndClose(AgentCommand::ResumeGoal)
    }

    pub fn scroll_by(&mut self, delta: i32) {
        self.scroll = (self.scroll as i32 + delta).max(0) as usize;
    }

    pub fn title(&self) -> String {
        " ◎ goal ".into()
    }

    /// The rows pinned under the body, and which of them is the button (a blank line, the button, the keys).
    pub fn pinned(&self, w: usize, theme: &Theme) -> (Vec<Line<'static>>, Option<usize>) {
        let keys = |set: &[(&str, &str)]| {
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (k, what) in set {
                if !spans.is_empty() {
                    spans.push(Span::styled(" · ", theme.dim2()));
                }
                spans.push(Span::styled((*k).to_string(), theme.style(theme.accent)));
                spans.push(Span::styled(format!(" {what}"), theme.dim()));
            }
            Line::from(spans)
        };
        let mut out = vec![Line::default()];
        let mut button = None;
        if self.can_resume() {
            let label = if self.goal.state == GoalState::Done { "Reopen" } else { "Resume" };
            let text = format!(" ▶ {label} ");
            let pad = w.saturating_sub(width(&text));
            button = Some(out.len());
            out.push(Line::from(vec![Span::styled(text, theme.chosen()), Span::raw(" ".repeat(pad))]));
            out.push(keys(&[("r", "resume"), ("↑↓", "scroll"), ("Esc", "close")]));
        } else {
            out.push(keys(&[("↑↓", "scroll"), ("Esc", "close")]));
        }
        (out, button)
    }

    /// [`lines`](Self::lines) with the motion of the moment: the rows coming in, the round lit once, the mark of a
    /// met goal popping with a sweep of light along its row (`at` is the time the effects are read at).
    pub fn lines_fx(
        &self,
        w: usize,
        theme: &Theme,
        now: i64,
        fx: &crate::fx_goal::GoalFx,
        at: std::time::Instant,
    ) -> Vec<Line<'static>> {
        let mut out = self.lines(w, theme, now);
        let fig = format!("{}/{}", self.goal.round, self.goal.max);
        if let Some(l) = out.iter_mut().find(|l| l.spans.iter().any(|s| s.content.contains(&format!("round {fig}")))) {
            crate::fx_goal::light(theme, l, &fig, fx.round_at, at, crate::motion::LIT_MS);
        }
        if self.goal.state == GoalState::Done {
            if let Some(l) = out.first_mut() {
                crate::fx_goal::pop(theme, l, "✓", fx.met_at, at, crate::fx_goal::MET_MS);
                crate::fx_goal::glow(theme, l, 0, fx.met_at, at);
            }
        }
        crate::fx_goal::open_rows(theme, &mut out, 0, fx.modal_at, at);
        out
    }

    /// The body at `w` columns, with the clock read at `now` (unix ms).
    pub fn lines(&self, w: usize, theme: &Theme, now: i64) -> Vec<Line<'static>> {
        let head = |t: &str| Line::from(Span::styled(t.to_string(), theme.dim()));
        let indent = [Span::raw("  ")];
        let inner = w.saturating_sub(2).max(1);
        let log = &self.log;
        let mut out: Vec<Line<'static>> = Vec::new();

        // where it stands
        let (state, tone) = match &self.goal.state {
            GoalState::Active => ("working".to_string(), theme.accent2),
            GoalState::Waiting => ("waiting for your answer".to_string(), theme.warn),
            GoalState::Paused(why) => (format!("paused: {why}"), theme.dim),
            GoalState::Done => ("met ✓".to_string(), theme.success),
        };
        out.extend(wrap_plain(&state, w, theme.bold(tone), &[]));
        let mut facts: Vec<String> = Vec::new();
        if self.goal.max > 0 {
            facts.push(format!("round {}/{}", self.goal.round, self.goal.max));
        }
        if log.started_ms > 0 {
            facts.push(format!("running {}", span(now - log.started_ms)));
        }
        let s = if log.steps == 1 { "" } else { "s" };
        facts.push(format!("{} step{s}", log.steps));
        if log.cost_usd > 0.0 {
            facts.push(format!("${:.4}", log.cost_usd));
        }
        out.extend(wrap_plain(&facts.join(" · "), w, theme.dim(), &[]));

        // the person's words, then how the agent restated them
        if let Some(own) = &log.original {
            out.push(Line::default());
            out.push(head("you asked"));
            out.extend(wrap_plain(own, inner, Default::default(), &indent));
        }
        if let Some(brief) = &log.brief {
            out.push(Line::default());
            out.push(head("restated"));
            for l in brief.lines() {
                // "Want: …" keeps its label in the accent colour, as the side panel reads it
                let (label, rest) = match l.split_once(": ") {
                    Some((k, v)) if matches!(k, "Want" | "Why" | "How" | "Done when") => (format!("{k}: "), v),
                    _ => (String::new(), l),
                };
                let mut lines = wrap_plain(&format!("{label}{rest}"), inner, Default::default(), &indent);
                if let (false, Some(first)) = (label.is_empty(), lines.first_mut()) {
                    let total: String = first.spans.iter().map(|s| s.content.as_ref()).collect();
                    let tail = total.strip_prefix(&format!("  {label}")).unwrap_or(&total).to_string();
                    *first = Line::from(vec![
                        Span::raw("  "),
                        Span::styled(label.clone(), theme.style(theme.accent)),
                        Span::raw(tail),
                    ]);
                }
                out.extend(lines);
            }
        }
        if log.original.is_none() && log.brief.is_none() {
            out.push(Line::default());
            out.push(head("goal"));
            out.extend(wrap_plain(&self.goal.text, inner, Default::default(), &indent));
        }

        // the latest progress
        if let Some(last) = &log.last {
            out.push(Line::default());
            out.push(head("last reply"));
            out.push(Line::from(vec![Span::raw("  "), Span::raw(truncate(last, inner))]));
        }

        // every stop, the newest first
        out.push(Line::default());
        if log.stops.is_empty() {
            out.push(head("no interruptions"));
        } else {
            out.push(head(&format!("interruptions ({})", log.stops.len())));
            for stop in log.stops.iter().rev() {
                let ago = if now > stop.at_ms { format!("{} ago", span(now - stop.at_ms)) } else { "just now".into() };
                let line = format!("{ago} · {}", stop.why);
                out.extend(wrap_plain(&line, inner, Default::default(), &indent));
            }
        }
        out
    }
}

/// `12 s`, `5 min`, `1 h 05 min`: a span of time for a person, not a log.
pub fn span(ms: i64) -> String {
    let ms = ms.max(0) as u64;
    match ms {
        0..=59_999 => format!("{} s", ms / 1000),
        60_000..=3_599_999 => format!("{} min", ms / 60_000),
        _ => format!("{} h {:02} min", ms / 3_600_000, ms % 3_600_000 / 60_000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_core::agent::GoalStop;

    fn goal(state: GoalState) -> Goal {
        Goal { text: "Want: green".into(), round: 2, max: 5, state }
    }

    fn log() -> GoalLog {
        GoalLog {
            original: Some("tests green pls".into()),
            brief: Some("Want: every test passes\nWhy: release\nHow: fix code\nDone when: 0 failed".into()),
            started_ms: 1_000_000,
            steps: 7,
            cost_usd: 0.0123,
            stops: vec![
                GoalStop { at_ms: 1_100_000, why: "interrupted".into() },
                GoalStop { at_ms: 1_200_000, why: "the turn failed".into() },
            ],
            last: Some("Fixed the parser".into()),
        }
    }

    fn text(v: &GoalView, now: i64) -> String {
        v.lines(60, &Theme::dark(), now)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn spans_read_as_a_person_would_say_them() {
        assert_eq!(span(0), "0 s");
        assert_eq!(span(12_900), "12 s");
        assert_eq!(span(5 * 60_000 + 40_000), "5 min");
        assert_eq!(span(3_900_000), "1 h 05 min");
        assert_eq!(span(-5), "0 s");
    }

    #[test]
    fn the_window_shows_both_wordings_the_clock_the_spend_and_each_stop() {
        let v = GoalView::new(goal(GoalState::Paused("interrupted".into())), log());
        let t = text(&v, 1_000_000 + 600_000);
        for want in [
            "paused: interrupted",
            "round 2/5",
            "running 10 min",
            "7 steps",
            "$0.0123",
            "you asked",
            "tests green pls",
            "restated",
            "Want: every test passes",
            "Done when: 0 failed",
            "last reply",
            "Fixed the parser",
            "interruptions (2)",
            "6 min ago · the turn failed",
            "8 min ago · interrupted",
        ] {
            assert!(t.contains(want), "missing {want:?} in:\n{t}");
        }
        // the newest stop comes first
        assert!(t.find("the turn failed") < t.find("· interrupted"));
    }

    #[test]
    fn a_goal_with_nothing_logged_still_shows_its_text() {
        let v = GoalView::new(goal(GoalState::Active), GoalLog::default());
        let t = text(&v, 0);
        assert!(t.contains("Want: green") && t.contains("no interruptions") && t.contains("0 steps"), "{t}");
        assert!(!t.contains("you asked") && !t.contains("running"));
    }

    #[test]
    fn only_a_stopped_or_met_goal_can_be_resumed() {
        let theme = Theme::dark();
        for (state, can) in [
            (GoalState::Active, false),
            (GoalState::Waiting, false),
            (GoalState::Paused("x".into()), true),
            (GoalState::Done, true),
        ] {
            let mut v = GoalView::new(goal(state), log());
            assert_eq!(v.can_resume(), can);
            assert_eq!(v.pinned(40, &theme).1.is_some(), can);
            let out = v.key(KeyEvent::new(KeyCode::Char('r'), crossterm::event::KeyModifiers::NONE));
            assert_eq!(out == ModalOut::SendAndClose(AgentCommand::ResumeGoal), can);
        }
    }

    #[test]
    fn esc_closes_and_the_arrows_scroll() {
        let mut v = GoalView::new(goal(GoalState::Active), log());
        let key = |c| KeyEvent::new(c, crossterm::event::KeyModifiers::NONE);
        assert_eq!(v.key(key(KeyCode::Down)), ModalOut::Nothing);
        assert_eq!(v.scroll, 1);
        assert_eq!(v.key(key(KeyCode::Esc)), ModalOut::Close);
    }
}
