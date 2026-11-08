//! The fleet pulse: one line under the header with what the whole fleet is doing now, its heartbeat (events
//! per minute over the last half hour), and the figures that matter. A figure that goes up rolls to its new
//! value; a new mail, commit or test run flashes the line for a moment. Too narrow for it all, the figures
//! drop from the least to the most important, and the count of sessions that need the person is never dropped.

use std::collections::HashMap;
use std::time::Instant;

use nexo_agent::supervise::{AgentRow, EvKind, State};
use ratatui::style::{Color, Modifier};
use ratatui::text::Span;

use super::{attention, AgentsView};
use crate::motion::{fade, within, Counter};
use crate::panel_fx as px;
use crate::render::wrap::width;
use crate::theme::Theme;

/// Cells of the heartbeat.
const BEAT_CELLS: usize = 8;
/// Minutes of fleet history the heartbeat covers (the length of a session's sparkline).
const BEAT_MINUTES: usize = 24;

/// What the pulse row remembers between two draws: the rolling figures and the last flash.
#[derive(Debug, Clone, Default)]
pub(super) struct PulseFx {
    roll: HashMap<&'static str, Counter>,
    shown: HashMap<&'static str, f64>,
    last: HashMap<&'static str, f64>,
    flash: Option<Instant>,
    moving: bool,
}

impl PulseFx {
    pub(super) fn animating(&self, now: Instant) -> bool {
        self.moving || self.flashing(now)
    }

    /// The line is lit by a new mail, commit or test run.
    pub(super) fn flashing(&self, now: Instant) -> bool {
        within(self.flash, now, px::FLASH_MS)
    }
}

/// The fleet in figures.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Totals {
    pub live: usize,
    /// Events in the last minute.
    pub rate: u32,
    pub mails: usize,
    pub pass: u32,
    pub fail: u32,
    pub commits: u32,
    pub cost: f64,
    pub need: usize,
    /// Events (or tokens) per minute, summed over the sessions, the newest last.
    pub beat: Vec<u32>,
}

/// A piece of the line that is dropped as a whole.
struct Piece {
    /// Higher goes first when the line is too narrow.
    drop: u8,
    spans: Vec<Span<'static>>,
}

impl Piece {
    fn width(&self) -> usize {
        self.spans.iter().map(|s| width(&s.content)).sum()
    }
}

impl AgentsView {
    pub(super) fn totals(&self, rows: &[AgentRow]) -> Totals {
        let mut t = Totals { beat: vec![0; BEAT_MINUTES], ..Totals::default() };
        for r in rows {
            if r.state == State::Working {
                t.live += 1;
            }
            if attention(r.state) {
                t.need += 1;
            }
            for e in &r.ribbon {
                if self.clock - e.at < 60 {
                    t.rate += e.n;
                }
                if e.kind == EvKind::GitCommit {
                    t.commits += e.n;
                }
            }
            if let Some((p, f)) = r.tests {
                t.pass += p;
                t.fail += f;
            }
            t.cost += r.cost_usd.unwrap_or(0.0);
            // right-aligned: the newest minute of every session lands on the newest bucket
            let n = r.spark.len().min(BEAT_MINUTES);
            for (i, v) in r.spark[r.spark.len() - n..].iter().enumerate() {
                t.beat[BEAT_MINUTES - n + i] = t.beat[BEAT_MINUTES - n + i].saturating_add(*v);
            }
        }
        t.mails = self.live.mails.values().filter(|i| self.clock - i.row.at <= 3600).count();
        t
    }

    /// The figures to roll to, learned at each draw with motion on.
    pub(super) fn pulse_note(&mut self, now: Instant) {
        let rows = self.shown();
        let t = self.totals(&rows);
        let primed = self.live.primed;
        let figs: [(&'static str, f64); 8] = [
            ("live", t.live as f64),
            ("rate", t.rate as f64),
            ("mails", t.mails as f64),
            ("pass", t.pass as f64),
            ("fail", t.fail as f64),
            ("commits", t.commits as f64),
            ("cost", t.cost),
            ("need", t.need as f64),
        ];
        let p = &mut self.live.pulse;
        let mut moving = false;
        let mut bump = false;
        for (id, v) in figs {
            let (shown, mv) = p.roll.entry(id).or_default().sample(v, now, primed);
            p.shown.insert(id, shown);
            moving |= mv;
            let before = p.last.insert(id, v).unwrap_or(v);
            if primed && v > before && matches!(id, "mails" | "pass" | "fail" | "commits") {
                bump = true;
            }
        }
        p.moving = moving;
        // a busy fleet raises a figure at every poll: one flash, then a pause, not a flicker
        if bump && !within(p.flash, now, px::FLASH_MS + px::FLASH_GAP_MS) {
            p.flash = Some(now);
        }
    }

    /// A figure as shown now: rolling, or settled when the motion is off.
    fn fig(&self, id: &'static str, target: f64, fx: bool) -> f64 {
        if fx {
            self.live.pulse.shown.get(id).copied().unwrap_or(target)
        } else {
            target
        }
    }

    /// Whether anything in the fleet works now (the heartbeat sweeps).
    fn any_busy(&self, rows: &[AgentRow]) -> bool {
        rows.iter().any(|r| self.busy(r))
    }

    /// The pieces of the pulse line, most important last to go.
    fn pieces(&self, rows: &[AgentRow], theme: &Theme, now: Instant, fx: bool) -> Vec<Piece> {
        let a = self.ascii;
        let t = self.totals(rows);
        let flash = fx && within(self.live.pulse.flash, now, px::FLASH_MS);
        let hot = |c: Color| if flash { theme.accent } else { c };
        let n = |id, v: f64| self.fig(id, v, fx).round().max(0.0) as i64;
        let sym = |u: &'static str, asc: &'static str| if a { asc } else { u };
        let mut out: Vec<Piece> = Vec::new();

        // live + heartbeat
        let live = n("live", t.live as f64);
        let lit = (fx && self.any_busy(rows)).then(|| {
            let ms = self.opened.map_or(0, |o| now.saturating_duration_since(o).as_millis() as u64);
            (ms % px::SWEEP_MS) as usize * BEAT_CELLS / px::SWEEP_MS as usize
        });
        let buckets: Vec<u32> = t.beat.chunks(BEAT_MINUTES / BEAT_CELLS).map(|c| c.iter().sum()).collect();
        let mut beat = px::heartbeat(&buckets, BEAT_CELLS, theme, lit);
        if a {
            for s in &mut beat {
                s.content =
                    s.content.chars().map(|c| crate::ascii::fold_char(c).unwrap_or(c)).collect::<String>().into();
            }
        }
        let mut spans = vec![Span::styled(
            format!("{} {live} live ", sym("◉", "o")),
            theme.style(if live > 0 { theme.success } else { theme.dim }),
        )];
        spans.extend(beat);
        out.push(Piece { drop: 2, spans });

        // events a minute
        let rate = n("rate", t.rate as f64);
        out.push(Piece { drop: 3, spans: vec![Span::styled(format!("{rate}/min"), theme.dim())] });

        // mails of the last hour
        let mails = n("mails", t.mails as f64);
        out.push(Piece {
            drop: 4,
            spans: vec![Span::styled(format!("{} {mails}", sym("✉", "M")), theme.style(hot(theme.accent2)))],
        });

        // the last test runs
        let (pass, fail) = (n("pass", t.pass as f64), n("fail", t.fail as f64));
        if pass + fail > 0 {
            let mut spans = vec![Span::styled(format!("{}{pass}", sym("✔", "+")), theme.style(hot(theme.success)))];
            if fail > 0 {
                spans.push(Span::styled(format!(" {}{fail}", sym("✘", "x")), theme.style(theme.error)));
            }
            out.push(Piece { drop: 5, spans });
        }

        // commits
        let commits = n("commits", t.commits as f64);
        if commits > 0 {
            let word = if commits == 1 { "commit" } else { "commits" };
            out.push(Piece {
                drop: 6,
                spans: vec![Span::styled(
                    format!("{} {commits} {word}", sym("◉", "c")),
                    theme.style(hot(theme.accent)),
                )],
            });
        }

        // spend
        let cost = self.fig("cost", t.cost, fx);
        if cost >= 0.005 {
            out.push(Piece { drop: 7, spans: vec![Span::styled(format!("${cost:.2}"), theme.dim())] });
        }

        // who needs the person: always last to go
        let need = n("need", t.need as f64);
        let (color, bold) = if need > 0 { (theme.warn2, true) } else { (theme.dim2, false) };
        let mut st = theme.style(color);
        if bold {
            st = st.add_modifier(Modifier::BOLD);
        }
        out.push(Piece { drop: 0, spans: vec![Span::styled(format!("! {need} need you"), st)] });
        out
    }

    /// The pulse line in `w` columns: the pieces that fit, two blanks apart; the whole line flashes briefly
    /// when a mail, a commit or a test run is new.
    pub(super) fn pulse_line(&self, theme: &Theme, w: usize, now: Instant, fx: bool) -> Vec<Span<'static>> {
        let rows = self.shown();
        let mut pieces = self.pieces(&rows, theme, now, fx);
        let total = |p: &[Piece]| p.iter().map(Piece::width).sum::<usize>() + 2 * p.len().saturating_sub(1);
        while total(&pieces) > w {
            let Some(i) = pieces.iter().enumerate().max_by_key(|(_, p)| p.drop).map(|(i, _)| i) else { break };
            if pieces[i].drop == 0 {
                break;
            }
            pieces.remove(i);
        }
        let mut out: Vec<Span<'static>> = Vec::new();
        for (i, p) in pieces.into_iter().enumerate() {
            if i > 0 {
                out.push(Span::raw("  "));
            }
            out.extend(p.spans);
        }
        let flash = fx && within(self.live.pulse.flash, now, px::FLASH_MS);
        if flash {
            // the rule of the pulse brightens: every dim part takes the accent for a moment
            let dim = [theme.dim, theme.dim2];
            for s in &mut out {
                if s.style.fg.is_some_and(|c| dim.contains(&c)) {
                    s.style = s.style.fg(fade(theme, theme.dim, theme.accent, 0.5));
                }
            }
        }
        out
    }

    /// What the header carries on its right when there is no room for the pulse row: who needs the person,
    /// how many work.
    pub(super) fn pulse_fold(&self, rows: &[AgentRow]) -> Option<String> {
        let t = self.totals(rows);
        let a = self.ascii;
        let mut parts: Vec<String> = Vec::new();
        if t.need > 0 {
            parts.push(format!("! {}", t.need));
        }
        if t.live > 0 {
            parts.push(format!("{} {}", if a { "o" } else { "◉" }, t.live));
        }
        let pending: u32 = rows.iter().map(|r| u32::from(r.unread + r.held)).sum();
        if pending > 0 {
            parts.push(format!("{} {pending}", if a { "M" } else { "✉" }));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }
}
