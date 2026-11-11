//! Motion of the goal and to-do views: the side panel and the goal window opening row by row, the to-do
//! bar easing to its new figure, the changed figure lit once, the mark of a met goal or a checked item
//! popping, a sweep of light along the met row, a checked item's text wiping to its done colour, a new
//! row fading in and a task that starts taking the running colour. Everything is colour on cells that are
//! already drawn: no text moves, and once an effect is over the lines are exactly the settled ones.
//!
//! The state is "what changed and when", noted once per tick by comparing with what the last tick saw
//! (the first look is not a change). The draw asks `now`; with animations off nothing is ever stamped.

use std::time::Instant;

use nexo_core::tool::{TodoItem, TodoStatus};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use crate::anim::{ease_out, progress, Tween};
use crate::motion::{self, fade, hump, lit_style, within};
use crate::render::blocks::todo_style;
use crate::render::wrap::width;
use crate::theme::Theme;

/// The rows of a panel or a window that just opened come in, top to bottom, in this long in all.
pub const OPEN_MS: u64 = 200;
/// One row of that stagger fades in this long.
const ROW_FADE_MS: u64 = 100;
/// The bar of the to-do list eases to its new figure in this long.
pub const BAR_MS: u64 = 300;
/// A met goal's mark pops and one sweep of light runs along its row in this long.
pub const MET_MS: u64 = 450;
/// A checked item's mark pops in this long.
pub const POP_MS: u64 = motion::POP_MS;
/// A checked item's text wipes to its done colour in this long.
pub const WIPE_MS: u64 = 300;
/// A new to-do row fades in from the background in this long.
pub const BORN_MS: u64 = 200;
/// A task that starts takes the running colour in this long.
pub const RUN_MS: u64 = 160;
/// The soft edge of the wipe and the tail of the sweep, in cells.
const EDGE: f32 = 6.0;
/// At most this many rows take part in the opening stagger (the rest come in with the last).
const STAGGER_ROWS: usize = 10;

/// What the last tick saw of one task, and when it last changed.
#[derive(Debug, Clone, PartialEq)]
pub struct RowFx {
    text: String,
    status: TodoStatus,
    /// The status before the last change, and when it changed.
    from: TodoStatus,
    since: Option<Instant>,
    /// When the row first appeared.
    born: Option<Instant>,
}

impl RowFx {
    fn moving(&self, now: Instant) -> bool {
        within(self.born, now, BORN_MS) || within(self.since, now, POP_MS.max(WIPE_MS).max(RUN_MS))
    }
}

/// What a tick shows of the goal and the to-do list.
pub struct View<'a> {
    /// The side panel is showing.
    pub panel: bool,
    /// The goal, if any: met or not, and the round in progress.
    pub goal: Option<(bool, u32)>,
    /// The goal window is open.
    pub modal: bool,
    pub todo: &'a [TodoItem],
}

#[derive(Debug, Clone)]
pub struct GoalFx {
    seen: bool,
    panel_on: bool,
    goal_on: bool,
    met: bool,
    modal_on: bool,
    round: Option<u32>,
    counts: Option<(usize, usize)>,
    rows: Vec<RowFx>,
    pub open_at: Option<Instant>,
    pub modal_at: Option<Instant>,
    pub round_at: Option<Instant>,
    pub met_at: Option<Instant>,
    pub count_at: Option<Instant>,
    bar: Tween,
    /// The last tick ran with animations on (a stale bar then holds its value until the next tick).
    animated: bool,
}

impl Default for GoalFx {
    fn default() -> GoalFx {
        GoalFx {
            seen: false,
            panel_on: false,
            goal_on: false,
            met: false,
            modal_on: false,
            round: None,
            counts: None,
            rows: Vec::new(),
            open_at: None,
            modal_at: None,
            round_at: None,
            met_at: None,
            count_at: None,
            bar: Tween::new(0.0, BAR_MS),
            animated: false,
        }
    }
}

impl GoalFx {
    /// Compares `v` with what the last tick saw; true while anything of this lane still moves.
    pub fn note(&mut self, v: &View<'_>, now: Instant, on: bool) -> bool {
        let live = on && self.seen;
        let stamp = || live.then_some(now);
        // the side panel opens, or a goal appears in it: the rows come in one after another
        let goal_on = v.goal.is_some();
        if v.panel && (!self.panel_on || (goal_on && !self.goal_on)) {
            self.open_at = stamp();
        }
        if v.modal && !self.modal_on {
            self.modal_at = stamp();
        }
        let met = v.goal.is_some_and(|g| g.0);
        if met && !self.met {
            self.met_at = stamp();
        }
        let round = v.goal.map(|g| g.1);
        if self.round.is_some() && round.is_some() && round != self.round {
            self.round_at = stamp();
        }
        self.panel_on = v.panel;
        self.goal_on = goal_on;
        self.modal_on = v.modal;
        self.met = met;
        self.round = round;
        // the list: a task that is new fades in, one whose status changed pops, wipes or takes the colour
        let old = std::mem::take(&mut self.rows);
        for (i, it) in v.todo.iter().enumerate() {
            let prev = old.get(i).filter(|r| r.text == it.text).or_else(|| old.iter().find(|r| r.text == it.text));
            let row = match prev {
                Some(p) if p.status == it.status => p.clone(),
                Some(p) => RowFx { status: it.status, from: p.status, since: stamp(), ..p.clone() },
                None => RowFx { text: it.text.clone(), status: it.status, from: it.status, since: None, born: stamp() },
            };
            self.rows.push(row);
        }
        // the bar and the count follow the figures
        let counts = (crate::todo_strip::done_count(v.todo), v.todo.len());
        let frac = if counts.1 == 0 { 0.0 } else { counts.0 as f64 / counts.1 as f64 };
        if self.counts.is_some_and(|c| c != counts) {
            self.count_at = stamp();
        }
        self.counts = Some(counts);
        if live {
            self.bar.retarget(frac, now);
        } else {
            self.bar.snap(frac);
        }
        self.seen = true;
        self.animated = on;
        self.alive(now)
    }

    /// Whether any effect of this lane is still running at `now`.
    pub fn alive(&self, now: Instant) -> bool {
        within(self.open_at, now, OPEN_MS)
            || within(self.modal_at, now, OPEN_MS)
            || within(self.round_at, now, motion::LIT_MS)
            || within(self.met_at, now, MET_MS)
            || within(self.count_at, now, motion::LIT_MS)
            || self.bar.is_moving(now)
            || self.rows.iter().any(|r| r.moving(now))
    }

    /// The figure the to-do bar shows: eased while it moves, exactly `frac` otherwise. A figure the next
    /// tick has not retargeted to yet keeps the bar where it is, so it never steps ahead and back.
    pub fn bar_shown(&self, frac: f64, now: Instant) -> f64 {
        if (self.bar.target() - frac).abs() > 1e-9 && !(self.seen && self.animated) {
            return frac;
        }
        self.bar.value_at(now)
    }

    /// What the last tick saw of task `i`, if it is `text`.
    pub fn row(&self, i: usize, text: &str) -> Option<&RowFx> {
        self.rows.get(i).filter(|r| r.text == text)
    }
}

fn fg_of(theme: &Theme, s: Style) -> ratatui::style::Color {
    s.fg.unwrap_or(theme.fg)
}

/// How far row `i` of `n` is into its fade, 0..1, `ms` after the stagger began.
pub fn stagger(i: usize, n: usize, ms: u64) -> f32 {
    let n = n.clamp(1, STAGGER_ROWS);
    let i = i.min(n - 1);
    let step = if n > 1 { (OPEN_MS - ROW_FADE_MS) as f32 / (n - 1) as f32 } else { 0.0 };
    let t = (ms as f32 - step * i as f32) / ROW_FADE_MS as f32;
    ease_out(t.clamp(0.0, 1.0) as f64) as f32
}

/// The text colours of `line` from span `from` on, eased from the background by `k`.
fn fade_in(theme: &Theme, line: &mut Line<'static>, from: usize, k: f32) {
    if k >= 1.0 || theme.mono {
        return;
    }
    for s in line.spans.iter_mut().skip(from) {
        let fg = fg_of(theme, s.style);
        s.style = s.style.fg(fade(theme, theme.bg, fg, k));
    }
}

/// Rows `lines[from..]` come in top to bottom, `OPEN_MS` after `at`.
pub fn open_rows(theme: &Theme, lines: &mut [Line<'static>], from: usize, at: Option<Instant>, now: Instant) {
    if !within(at, now, OPEN_MS) {
        return;
    }
    let ms = at.map_or(0, |a| now.saturating_duration_since(a).as_millis() as u64);
    let n = lines.len().saturating_sub(from);
    for (i, l) in lines.iter_mut().skip(from).enumerate() {
        fade_in(theme, l, 0, stagger(i, n, ms));
    }
}

/// `line` with the first `needle` it holds restyled by `f`; the text is the same.
fn restyle(line: &mut Line<'static>, needle: &str, f: impl Fn(Style) -> Style) {
    let Some(i) = line.spans.iter().position(|s| s.content.contains(needle)) else { return };
    let span = line.spans.remove(i);
    let at = span.content.find(needle).unwrap_or(0);
    let (a, rest) = span.content.split_at(at);
    let (mid, b) = rest.split_at(needle.len());
    let mut parts = Vec::with_capacity(3);
    if !a.is_empty() {
        parts.push(Span::styled(a.to_string(), span.style));
    }
    parts.push(Span::styled(mid.to_string(), f(span.style)));
    if !b.is_empty() {
        parts.push(Span::styled(b.to_string(), span.style));
    }
    line.spans.splice(i..i, parts);
}

/// The figure `needle` of `line` lit once, `LIT_MS` after `at`.
pub fn light(theme: &Theme, line: &mut Line<'static>, needle: &str, at: Option<Instant>, now: Instant, ms: u64) {
    let level = hump(at, now, ms);
    if level > 0.0 && !theme.mono {
        restyle(line, needle, |s| lit_style(theme, s, level));
    }
}

/// The mark `needle` of `line` pops: bold and bright, then it settles.
pub fn pop(theme: &Theme, line: &mut Line<'static>, needle: &str, at: Option<Instant>, now: Instant, ms: u64) {
    let h = hump(at, now, ms);
    if h > 0.0 && !theme.mono {
        restyle(line, needle, |s| {
            let fg = fade(theme, fg_of(theme, s), theme.success, h);
            let s = s.fg(fg);
            if h > 0.2 {
                s.add_modifier(Modifier::BOLD)
            } else {
                s
            }
        });
    }
}

/// One sweep of light along `line` from span `from` on, `MET_MS` after `at`: the colours brighten under
/// the head as it runs left to right.
pub fn glow(theme: &Theme, line: &mut Line<'static>, from: usize, at: Option<Instant>, now: Instant) {
    if !within(at, now, MET_MS) || theme.mono {
        return;
    }
    let p = progress(at, now, MET_MS) as f32;
    let cells: usize = line.spans.iter().skip(from).map(|s| width(&s.content)).sum();
    let head = -EDGE + p * (cells as f32 + 2.0 * EDGE);
    let mut col = 0usize;
    let mut out: Vec<Span<'static>> = line.spans.iter().take(from).cloned().collect();
    for s in line.spans.iter().skip(from) {
        for g in s.content.graphemes(true) {
            let d = head - col as f32;
            let lit = if d < 0.0 { (1.0 + d / EDGE).max(0.0) } else { (1.0 - d / EDGE).max(0.0) };
            let style = s.style.fg(fade(theme, fg_of(theme, s.style), theme.fg, lit * 0.8));
            col += width(g);
            push(&mut out, g, style);
        }
    }
    line.spans = out;
}

fn push(out: &mut Vec<Span<'static>>, g: &str, style: Style) {
    match out.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(g),
        _ => out.push(Span::styled(g.to_string(), style)),
    }
}

/// The effects of one task's lines. `lines[0]` holds the mark at span `mark`; every span after it, and
/// every span after `mark` of the lines below, is the task's text.
pub fn row(theme: &Theme, lines: &mut [Line<'static>], mark: usize, rf: &RowFx, now: Instant) {
    if theme.mono {
        return;
    }
    if within(rf.born, now, BORN_MS) {
        let k = ease_out(progress(rf.born, now, BORN_MS)) as f32;
        for l in lines.iter_mut() {
            fade_in(theme, l, mark, k);
        }
    }
    let Some(since) = rf.since else { return };
    let (_, from_style) = todo_style(rf.from, theme);
    let from_fg = fg_of(theme, from_style);
    match rf.status {
        TodoStatus::Done => {
            // the pop may split the mark's span in up to three: the text starts after all of them
            let mut split = 0;
            if let Some(first) = lines.first_mut() {
                let spans = first.spans.len();
                pop(theme, first, mark_text(first, mark).as_str(), Some(since), now, POP_MS);
                split = first.spans.len() - spans;
            }
            if within(Some(since), now, WIPE_MS) {
                let p = progress(Some(since), now, WIPE_MS) as f32;
                for (i, l) in lines.iter_mut().enumerate() {
                    wipe(theme, l, mark + 1 + if i == 0 { split } else { 0 }, from_style, from_fg, p);
                }
            }
        }
        TodoStatus::InProgress => {
            if within(Some(since), now, RUN_MS) {
                let k = ease_out(progress(Some(since), now, RUN_MS)) as f32;
                for l in lines.iter_mut() {
                    for s in l.spans.iter_mut().skip(mark) {
                        let to = fg_of(theme, s.style);
                        s.style = s.style.fg(fade(theme, from_fg, to, k));
                    }
                }
            }
        }
        TodoStatus::Pending => {}
    }
}

/// The glyph of the mark span, without its padding.
fn mark_text(line: &Line<'static>, mark: usize) -> String {
    line.spans.get(mark).map(|s| s.content.trim().to_string()).unwrap_or_default()
}

/// The text of `line` from span `from` on wipes, left to right, from the colour of the task's old status
/// to the one it has now (`p` is 0..1 of the way; the words never move).
fn wipe(theme: &Theme, line: &mut Line<'static>, from: usize, old: Style, old_fg: ratatui::style::Color, p: f32) {
    let cells: usize = line.spans.iter().skip(from).map(|s| width(&s.content)).sum();
    let head = p * (cells as f32 + EDGE);
    let mut col = 0usize;
    let mut out: Vec<Span<'static>> = line.spans.iter().take(from).cloned().collect();
    for s in line.spans.iter().skip(from) {
        for g in s.content.graphemes(true) {
            let k = ((head - col as f32) / EDGE).clamp(0.0, 1.0);
            let style = if k >= 1.0 {
                s.style
            } else if k <= 0.0 {
                let mut st = s.style.fg(old_fg).remove_modifier(Modifier::CROSSED_OUT);
                st.add_modifier |= old.add_modifier;
                st
            } else {
                s.style.fg(fade(theme, old_fg, fg_of(theme, s.style), k))
            };
            col += width(g);
            push(&mut out, g, style);
        }
    }
    line.spans = out;
}

impl crate::app::App {
    /// Feeds the goal and to-do lane its tick; true while it moves.
    pub fn note_goal_fx(&mut self, now: Instant) -> bool {
        let on = self.fx_on();
        let view = View {
            panel: self.show_files || self.show_live,
            goal: self.goal.as_ref().map(|g| (matches!(g.state, nexo_core::agent::GoalState::Done), g.round)),
            modal: matches!(self.modal, Some(crate::modal::Modal::Goal(_))),
            todo: &self.todo,
        };
        self.fx.goal.note(&view, now, on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn it(t: &str, s: TodoStatus) -> TodoItem {
        TodoItem { text: t.into(), status: s }
    }
    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }
    fn text(l: &Line<'_>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }
    fn view(todo: &[TodoItem]) -> View<'_> {
        View { panel: false, goal: None, modal: false, todo }
    }

    #[test]
    fn rows_stagger_from_the_top_and_all_arrive_by_the_end() {
        assert_eq!(stagger(0, 5, 0), 0.0);
        assert!(stagger(0, 5, 50) > stagger(4, 5, 50));
        assert!(stagger(2, 5, 100) > 0.0 && stagger(2, 5, 100) < 1.0);
        for i in 0..5 {
            assert_eq!(stagger(i, 5, OPEN_MS), 1.0);
        }
        // a long list shares the last slot
        assert_eq!(stagger(40, 50, OPEN_MS), 1.0);
    }

    #[test]
    fn the_first_look_is_not_a_change_and_a_changed_status_is() {
        let t0 = Instant::now();
        let mut fx = GoalFx::default();
        let a = [it("a", TodoStatus::InProgress), it("b", TodoStatus::Pending)];
        assert!(!fx.note(&view(&a), t0, true));
        let b = [it("a", TodoStatus::Done), it("b", TodoStatus::InProgress)];
        assert!(fx.note(&view(&b), at(t0, 10), true));
        assert!(fx.bar.is_moving(at(t0, 100)));
        assert!(fx.alive(at(t0, 250)));
        assert!(!fx.alive(at(t0, 10 + motion::LIT_MS + 5)));
        assert!(!fx.note(&view(&b), at(t0, 1000), true));
        assert_eq!(fx.row(0, "a").unwrap().from, TodoStatus::InProgress);
    }

    #[test]
    fn nothing_is_stamped_with_animations_off() {
        let t0 = Instant::now();
        let mut fx = GoalFx::default();
        fx.note(&view(&[]), t0, false);
        let a = [it("a", TodoStatus::Done), it("new", TodoStatus::Pending)];
        assert!(!fx.note(&View { panel: true, goal: Some((true, 2)), modal: true, todo: &a }, at(t0, 5), false));
        assert_eq!(fx.bar_shown(0.5, at(t0, 5)), 0.5);
    }

    #[test]
    fn the_bar_eases_to_the_new_figure_and_rests_on_it() {
        let t0 = Instant::now();
        let mut fx = GoalFx::default();
        fx.note(&view(&[it("a", TodoStatus::Pending), it("b", TodoStatus::Pending)]), t0, true);
        let b = [it("a", TodoStatus::Done), it("b", TodoStatus::Pending)];
        fx.note(&view(&b), at(t0, 10), true);
        assert_eq!(fx.bar_shown(0.5, at(t0, 10)), 0.0);
        let mid = fx.bar_shown(0.5, at(t0, 10 + BAR_MS / 2));
        assert!(mid > 0.0 && mid < 0.5);
        assert_eq!(fx.bar_shown(0.5, at(t0, 10 + BAR_MS)), 0.5);
    }

    #[test]
    fn a_new_goal_met_or_round_is_stamped_once() {
        let t0 = Instant::now();
        let mut fx = GoalFx::default();
        let v = |met, round| View { panel: true, goal: Some((met, round)), modal: false, todo: &[] };
        fx.note(&v(false, 1), t0, true);
        assert!(fx.open_at.is_none() && fx.round_at.is_none());
        fx.note(&v(false, 2), at(t0, 10), true);
        assert!(fx.round_at.is_some());
        fx.note(&v(true, 2), at(t0, 20), true);
        assert_eq!(fx.met_at, Some(at(t0, 20)));
        fx.note(&v(true, 2), at(t0, 900), true);
        assert_eq!(fx.met_at, Some(at(t0, 20)));
    }

    #[test]
    fn a_figure_is_lit_in_its_own_span_and_the_text_stays() {
        let t = Theme::dark();
        let t0 = Instant::now();
        let mut l = Line::from(vec![Span::raw("│ "), Span::raw("round 2/5 now")]);
        let before = text(&l);
        light(&t, &mut l, "2/5", Some(t0), at(t0, 100), motion::LIT_MS);
        assert_eq!(text(&l), before);
        assert_eq!(l.spans.len(), 4);
        assert!(l.spans[2].style.bg.is_some() && l.spans[1].style.bg.is_none());
        let mut done = Line::from(vec![Span::raw("round 2/5")]);
        light(&t, &mut done, "2/5", Some(t0), at(t0, motion::LIT_MS), motion::LIT_MS);
        assert_eq!(done.spans.len(), 1);
    }

    #[test]
    fn the_sweep_runs_once_and_leaves_the_line_as_it_was() {
        let t = Theme::dark();
        let t0 = Instant::now();
        let mk = || Line::from(vec![Span::raw("│ "), Span::styled("met ✓ · /goal off", t.style(t.success))]);
        let mut mid = mk();
        glow(&t, &mut mid, 1, Some(t0), at(t0, MET_MS / 2));
        assert_eq!(text(&mid), text(&mk()));
        assert_ne!(mid.spans, mk().spans);
        let mut end = mk();
        glow(&t, &mut end, 1, Some(t0), at(t0, MET_MS));
        assert_eq!(end.spans, mk().spans);
    }

    #[test]
    fn a_checked_item_pops_and_wipes_without_moving_a_word() {
        let t = Theme::dark();
        let t0 = Instant::now();
        let settled = vec![Line::from(vec![Span::styled(" ✓ ", t.dim()), Span::styled("write the report", t.dim())])];
        let rf = RowFx {
            text: "write the report".into(),
            status: TodoStatus::Done,
            from: TodoStatus::InProgress,
            since: Some(t0),
            born: None,
        };
        let mut mid = settled.clone();
        row(&t, &mut mid, 0, &rf, at(t0, 120));
        assert_eq!(text(&mid[0]), text(&settled[0]));
        assert_ne!(mid[0].spans, settled[0].spans);
        assert!(
            mid[0].spans.iter().any(|s| s.content == "✓" && s.style.add_modifier.contains(Modifier::BOLD)),
            "the mark pops"
        );
        let mut end = settled.clone();
        row(&t, &mut end, 0, &rf, at(t0, WIPE_MS.max(POP_MS)));
        assert_eq!(end, settled);
    }

    #[test]
    fn the_wipe_spares_the_popped_mark_of_the_strip() {
        let t = Theme::dark();
        let t0 = Instant::now();
        let settled = vec![Line::from(vec![Span::styled(" ✓ ", t.dim()), Span::styled("write the report", t.dim())])];
        let rf = RowFx {
            text: "write the report".into(),
            status: TodoStatus::Done,
            from: TodoStatus::InProgress,
            since: Some(t0),
            born: None,
        };
        // early in the effect the wipe head is still inside its soft edge on the first cells
        let now = at(t0, 30);
        let mut popped = settled.clone();
        pop(&t, &mut popped[0], "✓", Some(t0), now, POP_MS);
        let mark = |l: &Line<'static>| l.spans.iter().find(|s| s.content == "✓").unwrap().style;
        let mut mid = settled.clone();
        row(&t, &mut mid, 0, &rf, now);
        assert_eq!(mark(&mid[0]), mark(&popped[0]), "the mark keeps its pop colour");
        assert_ne!(mark(&mid[0]).fg, Some(fg_of(&t, t.todo_running())));
        assert_eq!(text(&mid[0]), text(&settled[0]));
        // the words are still wiping
        assert!(mid[0].spans.iter().skip(3).any(|s| s.style != t.dim()));
    }

    #[test]
    fn the_bar_never_steps_back_when_drawn_before_the_tick() {
        let t0 = Instant::now();
        let mut fx = GoalFx::default();
        fx.note(&view(&[it("a", TodoStatus::Pending), it("b", TodoStatus::Pending)]), t0, true);
        let b = [it("a", TodoStatus::Done), it("b", TodoStatus::Pending)];
        // a draw between the event and the tick keeps the old figure
        let mut shown = vec![fx.bar_shown(0.5, at(t0, 1000))];
        assert_eq!(shown[0], 0.0);
        fx.note(&view(&b), at(t0, 1008), true);
        for ms in [1008, 1050, 1100, 1200, 1308, 1400] {
            shown.push(fx.bar_shown(0.5, at(t0, ms)));
        }
        assert!(shown.windows(2).all(|w| w[0] <= w[1]), "{shown:?}");
        assert_eq!(shown.last(), Some(&0.5));
    }

    #[test]
    fn a_new_row_fades_in_and_a_started_task_takes_the_colour() {
        let t = Theme::dark();
        let t0 = Instant::now();
        let settled =
            vec![Line::from(vec![Span::styled(" ▸ ", t.todo_running()), Span::styled("go", t.todo_running())])];
        let born = RowFx {
            text: "go".into(),
            status: TodoStatus::InProgress,
            from: TodoStatus::InProgress,
            since: None,
            born: Some(t0),
        };
        let mut l = settled.clone();
        row(&t, &mut l, 0, &born, at(t0, 0));
        assert_eq!(l[0].spans[1].style.fg, Some(t.bg));
        let mut l = settled.clone();
        row(&t, &mut l, 0, &born, at(t0, BORN_MS));
        assert_eq!(l, settled);
        let start = RowFx { since: Some(t0), born: None, from: TodoStatus::Pending, ..born };
        let mut l = settled.clone();
        row(&t, &mut l, 0, &start, at(t0, RUN_MS / 2));
        assert_ne!(l, settled);
        let mut l = settled.clone();
        row(&t, &mut l, 0, &start, at(t0, RUN_MS));
        assert_eq!(l, settled);
    }
}
