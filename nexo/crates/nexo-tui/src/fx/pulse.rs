//! The work line reacts to what happens: each event (a sub-agent launched or ended, a skill loaded, a tool
//! started or answered, the person's message, text streaming, the end of a turn) sends one short pulse along
//! the rule under the activity line. What each event does is one row of `TABLE`; pulses decay by the frame
//! clock, overlap freely, and live in a fixed array (nothing is allocated, nothing is drawn after they die).
//! Colour only: they tint the `─` cells and never move a cell. Pure functions of the time they are asked
//! about; the caller injects `now`.

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::motion::fade;
use crate::theme::Theme;

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    SubLaunched,
    SubDone,
    SubFailed,
    SkillLoaded,
    ToolStarted,
    ToolProgress,
    ToolAnswered,
    ToolFailed,
    UserSent,
    UserQueued,
    Streaming,
    TurnDone,
    TurnCancelled,
    TurnFailed,
}

/// The shape a pulse draws along the rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    /// A comet crossing the rule (right to left when `rev`), `width` cells long.
    Run { rev: bool, width: f32 },
    /// Two comets leave the middle for the ends.
    Split,
    /// Two comets come from the ends into the middle.
    Merge,
    /// A ring spreads both ways from a point, `at` per cent along the rule.
    Ripple { at: u8 },
    /// The whole rule lights and settles.
    Flash,
}

impl crate::app::App {
    /// A cue happened: starts its pulse when the motion is on (and, for a cue that is only live, while the agent works).
    pub(crate) fn pulse(&mut self, cue: Cue) {
        if self.fx_on() && (self.busy || !cue.row().live_only) {
            let now = self.now();
            self.fx.pulses.push(cue, now);
        }
    }

    /// The cue an agent event is, if it is one: the one place events meet the table.
    pub(crate) fn pulse_for(&mut self, ev: &nexo_core::agent::AgentEvent) {
        use nexo_core::agent::{AgentEvent as E, TurnEnd};
        let cue = match ev {
            E::TextDelta { .. } | E::ReasoningDelta { .. } => Cue::Streaming,
            E::ToolCall { name, .. } if name == "skill" => Cue::SkillLoaded,
            E::ToolCall { .. } => Cue::ToolStarted,
            E::ToolDone { output, .. } if output.is_error => Cue::ToolFailed,
            E::ToolDone { .. } => Cue::ToolAnswered,
            E::TurnEnd { reason: TurnEnd::Done } => Cue::TurnDone,
            E::TurnEnd { reason: TurnEnd::Interrupted } => Cue::TurnCancelled,
            E::TurnEnd { .. } => Cue::TurnFailed,
            E::SubState { info } => {
                use nexo_core::subs::SubState;
                let was = self.sub_runs.iter().find(|r| r.info.id == info.id).map(|r| r.info.state);
                match (was, info.state) {
                    (None, s) if s.running() => Cue::SubLaunched,
                    (Some(w), SubState::Ok) if w.running() => Cue::SubDone,
                    (Some(w), SubState::Failed | SubState::Killed) if w.running() => Cue::SubFailed,
                    _ => return,
                }
            }
            _ => return,
        };
        self.pulse(cue);
    }
}

/// The theme colour a pulse takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Accent2,
    Success,
    Error,
    Warn,
    Info,
}

/// One row of the table: what the event does.
#[derive(Debug, Clone, Copy)]
pub struct Effect {
    pub cue: Cue,
    pub shape: Shape,
    pub tone: Tone,
    /// How long it lives.
    pub ms: u64,
    /// How strongly it tints at its brightest, 0..1.
    pub peak: f32,
    /// A cue of this kind again within this many ms is ignored (streaming sends a delta per token).
    pub gap_ms: u64,
    /// Only while the agent works (a replayed history does not light the rule).
    pub live_only: bool,
}

const fn fx(cue: Cue, shape: Shape, tone: Tone, ms: u64, peak: f32, gap_ms: u64, live_only: bool) -> Effect {
    Effect { cue, shape, tone, ms, peak, gap_ms, live_only }
}

/// Every event and its pulse. One row per `Cue`, in the order of the enum.
pub const TABLE: [Effect; 14] = [
    fx(Cue::SubLaunched, Shape::Split, Tone::Accent2, 900, 0.95, 0, false),
    fx(Cue::SubDone, Shape::Merge, Tone::Success, 800, 0.9, 0, false),
    fx(Cue::SubFailed, Shape::Merge, Tone::Error, 900, 0.95, 0, false),
    fx(Cue::SkillLoaded, Shape::Ripple { at: 15 }, Tone::Warn, 800, 0.85, 0, true),
    fx(Cue::ToolStarted, Shape::Run { rev: false, width: 10.0 }, Tone::Accent, 600, 0.9, 80, true),
    fx(Cue::ToolProgress, Shape::Ripple { at: 85 }, Tone::Info, 350, 0.7, 800, true),
    fx(Cue::ToolAnswered, Shape::Run { rev: true, width: 7.0 }, Tone::Info, 500, 0.8, 80, true),
    fx(Cue::ToolFailed, Shape::Flash, Tone::Error, 700, 0.7, 0, true),
    fx(Cue::UserSent, Shape::Ripple { at: 90 }, Tone::Accent2, 750, 0.9, 0, false),
    fx(Cue::UserQueued, Shape::Ripple { at: 90 }, Tone::Warn, 750, 0.8, 0, false),
    fx(Cue::Streaming, Shape::Run { rev: false, width: 5.0 }, Tone::Accent2, 380, 0.6, 260, true),
    fx(Cue::TurnDone, Shape::Flash, Tone::Success, 700, 0.5, 0, false),
    fx(Cue::TurnCancelled, Shape::Flash, Tone::Warn, 600, 0.5, 0, false),
    fx(Cue::TurnFailed, Shape::Flash, Tone::Error, 800, 0.7, 0, false),
];

impl Cue {
    fn row(self) -> &'static Effect {
        &TABLE[self as usize]
    }
}

/// Pulses alive at once; an extra one takes the place of the oldest.
const SLOTS: usize = 8;

/// The pulses of the work line: a fixed array, and when each kind of cue last came.
#[derive(Debug, Default, Clone)]
pub struct Pulses {
    live: [Option<(Cue, Instant)>; SLOTS],
    last: [Option<Instant>; TABLE.len()],
}

impl Pulses {
    /// A cue happened at `now`. Returns whether a pulse started.
    pub fn push(&mut self, cue: Cue, now: Instant) -> bool {
        let row = cue.row();
        let k = cue as usize;
        if self.last[k].is_some_and(|t| (now.saturating_duration_since(t).as_millis() as u64) < row.gap_ms) {
            return false;
        }
        self.last[k] = Some(now);
        let age = |s: &Option<(Cue, Instant)>| match s {
            None => u128::MAX,
            Some((c, t)) => {
                let a = now.saturating_duration_since(*t).as_millis();
                if a >= c.row().ms as u128 {
                    u128::MAX - 1
                } else {
                    a
                }
            }
        };
        // a free or dead slot first, else the oldest
        let slot = (0..SLOTS).max_by_key(|i| age(&self.live[*i])).unwrap_or(0);
        self.live[slot] = Some((cue, now));
        true
    }

    /// Whether any pulse is still on the rule (the frame loop keeps drawing while one is).
    pub fn alive(&self, now: Instant) -> bool {
        self.live.iter().flatten().any(|(c, t)| (now.saturating_duration_since(*t).as_millis() as u64) < c.row().ms)
    }

    /// Forgets every pulse.
    pub fn clear(&mut self) {
        *self = Pulses::default();
    }

    /// Tints the `─` cells of the row `area` with every live pulse. Allocates nothing.
    pub fn paint(&self, buf: &mut Buffer, area: Rect, theme: &Theme, now: Instant) {
        let n = area.width as usize;
        if n == 0 || theme.mono {
            return;
        }
        // the live ones: (row, progress 0..1)
        let mut act = [(&TABLE[0], 0.0f32); SLOTS];
        let mut m = 0;
        for (c, t) in self.live.iter().flatten() {
            let row = c.row();
            let ms = now.saturating_duration_since(*t).as_millis() as u64;
            if ms < row.ms {
                act[m] = (row, ms as f32 / row.ms as f32);
                m += 1;
            }
        }
        if m == 0 {
            return;
        }
        for x in 0..n {
            let Some(cell) = buf.cell_mut((area.x + x as u16, area.y)) else { continue };
            if cell.symbol() != "─" {
                continue;
            }
            let mut fg = cell.fg;
            for (row, t) in &act[..m] {
                let k = level(row.shape, x as f32 + 0.5, n as f32, *t) * row.peak;
                if k > 0.01 {
                    fg = fade(theme, fg, tone(theme, row.tone), k);
                }
            }
            cell.fg = fg;
        }
    }
}

fn tone(theme: &Theme, t: Tone) -> ratatui::style::Color {
    match t {
        Tone::Accent => theme.accent,
        Tone::Accent2 => theme.accent2,
        Tone::Success => theme.success,
        Tone::Error => theme.error,
        Tone::Warn => theme.warn,
        Tone::Info => theme.info,
    }
}

/// A comet's light at distance `d` behind its head: full at the head, none `w` cells back.
fn trail(d: f32, w: f32) -> f32 {
    if d < 0.0 || d >= w {
        0.0
    } else {
        1.0 - d / w
    }
}

/// How lit the cell at `x` of a rule `n` cells wide is, `t` (0..1) of the way through the pulse.
pub fn level(shape: Shape, x: f32, n: f32, t: f32) -> f32 {
    let fade_out = 1.0 - t * t;
    match shape {
        Shape::Run { rev, width } => {
            let x = if rev { n - x } else { x };
            trail(t * (n + width) - x, width) * fade_out
        }
        Shape::Split | Shape::Merge => {
            let c = n / 2.0;
            let w = 6.0;
            let dist = if shape == Shape::Split { (x - c).abs() } else { c - (x - c).abs() };
            trail(t * (c + w) - dist, w) * fade_out
        }
        Shape::Ripple { at } => {
            let o = n * at as f32 / 100.0;
            let reach = o.max(n - o) + 3.0;
            let ring = ((x - o).abs() - t * reach).abs();
            if ring >= 3.0 {
                0.0
            } else {
                (1.0 - ring / 3.0) * fade_out
            }
        }
        Shape::Flash => 4.0 * t * (1.0 - t),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn the_table_has_one_row_per_cue_in_order() {
        for (i, e) in TABLE.iter().enumerate() {
            assert_eq!(e.cue as usize, i, "{:?} is out of place", e.cue);
            assert!(e.ms > 0 && e.peak > 0.0 && e.peak <= 1.0);
        }
    }

    #[test]
    fn a_cue_starts_one_pulse_that_expires() {
        let t0 = Instant::now();
        let mut p = Pulses::default();
        assert!(!p.alive(t0));
        assert!(p.push(Cue::SubLaunched, t0));
        assert!(p.alive(at(t0, 899)));
        assert!(!p.alive(at(t0, 900)), "gone at its time");
    }

    #[test]
    fn pulses_overlap_and_the_oldest_makes_room() {
        let t0 = Instant::now();
        let mut p = Pulses::default();
        p.push(Cue::SubLaunched, t0);
        p.push(Cue::UserSent, at(t0, 100));
        assert_eq!(p.live.iter().flatten().count(), 2);
        // the short one ends, the long one still lives
        p.push(Cue::ToolFailed, at(t0, 200));
        assert!(p.alive(at(t0, 850)));
        for i in 0..20 {
            p.push(Cue::SubDone, at(t0, 300 + i));
        }
        assert!(p.live.iter().flatten().count() <= SLOTS);
        // a dead slot is reused before a live pulse is dropped
        let mut q = Pulses::default();
        q.push(Cue::ToolStarted, t0);
        for i in 0..SLOTS - 1 {
            q.push(Cue::SubLaunched, at(t0, 1000 + i as u64));
        }
        assert_eq!(q.live.iter().flatten().count(), SLOTS);
    }

    #[test]
    fn progress_pulses_expire_before_the_global_gap() {
        let t0 = Instant::now();
        let mut p = Pulses::default();
        assert!(p.push(Cue::ToolProgress, t0));
        assert!(p.alive(at(t0, 349)));
        assert!(!p.alive(at(t0, 350)));
        assert!(!p.push(Cue::ToolProgress, at(t0, 799)));
        assert!(p.push(Cue::ToolProgress, at(t0, 800)));
    }

    #[test]
    fn a_chatty_cue_is_thinned_by_its_gap() {
        let t0 = Instant::now();
        let mut p = Pulses::default();
        let started = (0..100).filter(|i| p.push(Cue::Streaming, at(t0, *i * 10))).count();
        assert!((3..=5).contains(&started), "{started} pulses in a second of tokens");
    }

    #[test]
    fn each_shape_moves_and_dies_out() {
        let n = 60.0;
        let lit = |s: Shape, t: f32| (0..60).map(|i| level(s, i as f32 + 0.5, n, t)).fold(0.0f32, f32::max);
        for s in [
            Shape::Run { rev: false, width: 8.0 },
            Shape::Run { rev: true, width: 8.0 },
            Shape::Split,
            Shape::Merge,
            Shape::Ripple { at: 90 },
            Shape::Flash,
        ] {
            assert!(lit(s, 0.4) > 0.2, "{s:?} is on the rule mid-way");
            assert!(lit(s, 1.0) < 0.05, "{s:?} is gone at the end");
        }
        // a run goes left to right, its reverse right to left
        let peak_x = |rev: bool, t: f32| {
            (0..60).max_by(|a, b| {
                let f = |i: &i32| level(Shape::Run { rev, width: 8.0 }, *i as f32 + 0.5, n, t);
                f(a).total_cmp(&f(b))
            })
        };
        assert!(peak_x(false, 0.3) < peak_x(false, 0.6));
        assert!(peak_x(true, 0.3) > peak_x(true, 0.6));
        // a split starts in the middle, a merge ends there
        assert!(level(Shape::Split, 30.0, n, 0.05) > level(Shape::Split, 5.0, n, 0.05));
        assert!(level(Shape::Merge, 5.0, n, 0.2) > level(Shape::Merge, 30.0, n, 0.2));
    }

    fn rule(w: u16) -> (Buffer, Rect) {
        let area = Rect::new(0, 0, w, 1);
        let mut b = Buffer::empty(area);
        for x in 0..w {
            b[(x, 0)].set_symbol("─").set_fg(ratatui::style::Color::Rgb(60, 60, 60));
        }
        (b, area)
    }

    fn theme() -> Theme {
        Theme::pick("dark")
    }

    #[test]
    fn the_rule_is_tinted_while_a_pulse_lives_and_plain_after() {
        let t0 = Instant::now();
        let th = theme();
        let mut p = Pulses::default();
        p.push(Cue::ToolFailed, t0);
        let (plain, area) = rule(50);
        for (ms, tinted) in [(0, false), (350, true), (600, true), (700, false), (2000, false)] {
            let mut b = plain.clone();
            p.paint(&mut b, area, &th, at(t0, ms));
            assert_eq!(b != plain, tinted, "at {ms} ms");
        }
        // text cells are never touched
        let (mut b, area) = rule(50);
        b[(10, 0)].set_symbol("x");
        let before = b[(10, 0)].clone();
        p.paint(&mut b, area, &th, at(t0, 350));
        assert_eq!(b[(10, 0)], before);
    }

    #[test]
    fn a_mono_theme_and_an_empty_row_draw_nothing() {
        let t0 = Instant::now();
        let mut th = theme();
        let mut p = Pulses::default();
        p.push(Cue::ToolFailed, t0);
        let (plain, area) = rule(20);
        let mut b = plain.clone();
        th.mono = true;
        p.paint(&mut b, area, &th, at(t0, 300));
        assert_eq!(b, plain);
        let mut z = Buffer::empty(Rect::new(0, 0, 0, 1));
        p.paint(&mut z, Rect::new(0, 0, 0, 1), &theme(), at(t0, 300));
    }

    #[test]
    fn a_frame_of_the_bar_costs_microseconds() {
        let t0 = Instant::now();
        let th = theme();
        let mut p = Pulses::default();
        for (i, c) in
            [Cue::SubLaunched, Cue::UserSent, Cue::ToolStarted, Cue::Streaming, Cue::SkillLoaded, Cue::ToolFailed]
                .into_iter()
                .enumerate()
        {
            p.push(c, at(t0, i as u64 * 20));
        }
        // the worst case: a very wide terminal with six pulses at once
        let (plain, area) = rule(300);
        let mut b = plain.clone();
        let frames = 2000u32;
        let start = Instant::now();
        for i in 0..frames {
            b.clone_from(&plain);
            p.paint(&mut b, area, &th, at(t0, 150 + (i as u64 % 400)));
        }
        let per = start.elapsed() / frames;
        println!("work-line pulses: {per:?} per frame (300 cells, 6 pulses)");
        assert!(per < Duration::from_millis(2), "a pulse frame took {per:?}");
        // and nothing at all when none lives
        let start = Instant::now();
        for _ in 0..frames {
            p.paint(&mut b, area, &th, at(t0, 5000));
        }
        let idle = start.elapsed() / frames;
        println!("work-line pulses idle: {idle:?} per frame");
        assert!(idle < Duration::from_micros(200), "an idle bar took {idle:?}");
    }
}
