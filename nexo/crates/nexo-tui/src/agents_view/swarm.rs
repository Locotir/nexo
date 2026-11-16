//! The swarm: a session's live sub-agents go round as satellites on the blank cells of its line in the tree,
//! and the lanes that are on go round the LANES header the same way. The text never moves: a satellite is put
//! only on a cell the line left blank, so a long step simply hides the part of the orbit it covers. The shapes
//! are pure functions in `fx::panel`; this keeps what they need between draws: when each satellite was born,
//! where on its orbit it was when its pace last changed, and when it ended.

use super::effects::{ms_since, put};
use super::*;
use crate::fx::panel::{self as px, Flight};
use nexo_agent::supervise::EvKind;

/// The swarm of the lanes goes by the LANES header's own id: the header is what they circle.
pub(super) const LANES: &str = "h:LANES";

/// One satellite as the effects remember it.
#[derive(Debug, Clone)]
pub(super) struct Sat {
    key: String,
    born: Instant,
    /// It was there before the first look: it does not grow, it is simply there.
    grown: bool,
    flight: Flight,
    /// Its angle when its pace last changed, since when, and how long a turn takes (None: it holds still).
    angle: f32,
    since: Instant,
    period: Option<u64>,
    ended: Option<Instant>,
}

impl Sat {
    fn angle_at(&self, now: Instant) -> f32 {
        px::orbit_angle(self.angle, ms_since(Some(self.since), now), self.period)
    }

    /// A new pace starts where the old one left the satellite, so it never jumps.
    fn pace(&mut self, now: Instant, period: Option<u64>) {
        if period != self.period {
            self.angle = self.angle_at(now);
            self.since = now;
            self.period = period;
        }
    }

    fn age_ms(&self, now: Instant) -> u64 {
        ms_since(Some(self.born), now) + if self.grown { px::SAT_SPAWN_MS } else { 0 }
    }

    /// Whether its end still plays (or it has not ended).
    fn alive(&self, now: Instant) -> bool {
        match self.flight {
            Flight::End { ok } => within(self.ended, now, px::end_ms(ok)),
            _ => true,
        }
    }
}

/// An orbit's first and last column.
type Cols = (f32, f32);

/// The satellites round one line, and that line's orbit.
#[derive(Debug, Clone, Default)]
pub(super) struct Swarm {
    sats: Vec<Sat>,
    /// Satellites born so far: where on the orbit the next one starts.
    count: u32,
    /// The orbit's first and last column as it eases between widths: from, to, since when.
    orbit: Option<(Cols, Cols, Instant)>,
}

impl Swarm {
    /// Whether something in it moves on its own: a satellite being born or ending, the orbit easing. One that
    /// goes round belongs to a row at work, which keeps the panel drawing anyway.
    pub(super) fn moving(&self, now: Instant) -> bool {
        self.orbit.is_some_and(|(from, to, t)| from != to && within(Some(t), now, px::ORBIT_EASE_MS))
            || self.sats.iter().any(|s| {
                (!s.grown && within(Some(s.born), now, px::SAT_SPAWN_MS)) || (s.ended.is_some() && s.alive(now))
            })
    }
}

/// One satellite as the rows say now: the line it circles, its own key, what it does, its pace.
pub(super) struct Member {
    owner: String,
    key: String,
    flight: Flight,
    period: Option<u64>,
}

/// Whether a session's last word was an error: its satellite ends with a cross.
fn failed(r: &AgentRow) -> bool {
    r.ribbon.iter().rev().take(2).any(|e| e.kind == EvKind::Error)
}

impl AgentsView {
    /// Every satellite the rows ask for: each sub-agent that has not finished, round its parent; each lane that
    /// is on, round the LANES header.
    pub(super) fn members(&self) -> Vec<Member> {
        let keys: HashSet<&str> = self.rows.iter().map(|r| r.key.as_str()).collect();
        let mut out = Vec::new();
        for r in &self.rows {
            let owner = match (&r.lane, r.parent.as_deref()) {
                (Some(l), _) if l.runner || l.state == "running" => LANES,
                (None, Some(p)) if keys.contains(p) && r.state != State::Done => p,
                _ => continue,
            };
            let busy = self.busy(r);
            let flight = if r.lane.is_some() {
                if works(r) {
                    Flight::Orbit { busy }
                } else {
                    Flight::Rest
                }
            } else {
                match r.state {
                    State::Working => Flight::Orbit { busy },
                    State::Idle { .. } => Flight::Rest,
                    _ => Flight::Wait,
                }
            };
            let rate = r.spark.last().copied().unwrap_or(0);
            let period = matches!(flight, Flight::Orbit { .. }).then(|| px::orbit_period(rate, busy));
            out.push(Member { owner: owner.to_string(), key: r.key.clone(), flight, period });
        }
        out
    }

    /// Learns the swarm as of this draw: a new member is born (grown already on the first look), one that left
    /// ends (with a cross when its last word was an error), a change of pace starts where the satellite is.
    pub(super) fn swarm_note(&mut self, now: Instant) {
        let primed = self.live.primed;
        let members = self.members();
        let live: HashSet<&str> = members.iter().map(|m| m.key.as_str()).collect();
        let rows = &self.rows;
        let swarms = &mut self.live.swarm;
        for sw in swarms.values_mut() {
            for s in sw.sats.iter_mut().filter(|s| s.ended.is_none()) {
                if !live.contains(s.key.as_str()) {
                    let ok = rows.iter().find(|r| r.key == s.key).is_none_or(|r| !failed(r));
                    s.pace(now, None);
                    s.flight = Flight::End { ok };
                    s.ended = Some(now);
                }
            }
            sw.sats.retain(|s| s.alive(now));
        }
        for m in members {
            let sw = swarms.entry(m.owner).or_default();
            if let Some(s) = sw.sats.iter_mut().find(|s| s.key == m.key && s.ended.is_none()) {
                s.pace(now, m.period);
                s.flight = m.flight;
                continue;
            }
            if sw.sats.iter().filter(|s| s.ended.is_none()).count() >= px::SAT_MAX {
                continue;
            }
            // one that comes back while its end still plays starts over
            sw.sats.retain(|s| s.key != m.key);
            let angle = sw.count as f32 * px::GOLDEN;
            sw.count += 1;
            sw.sats.push(Sat {
                key: m.key,
                born: now,
                grown: !primed,
                flight: m.flight,
                angle,
                since: now,
                period: m.period,
                ended: None,
            });
        }
        swarms.retain(|_, sw| !sw.sats.is_empty());
    }

    /// Draws the swarm of `owner` on `line`, whose blank stretch runs from its column `gap.0` up to `gap.1`: only
    /// on cells the text left blank, a cell clear of the words on each side, back to front. The orbit is kept in
    /// the line's own columns, so a row that slides in carries its satellites along instead of easing after it.
    /// `still` holds the members when the motion is off: they stand where they would be born, full size.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_swarm(
        &mut self,
        f: &mut Frame,
        owner: &str,
        line: Rect,
        gap: (usize, usize),
        theme: &Theme,
        now: Instant,
        still: Option<&[Member]>,
    ) {
        // the orbit is sized by the whole line, not by the part of it shown while it slides in, so it holds still
        let col = |c: usize| c.min(u16::MAX as usize) as u16;
        let (a, b) = (col(gap.0) + 1, col(gap.1).saturating_sub(2));
        let hi = b.saturating_sub(1);
        let lo = (a + 1).max(hi.saturating_sub(2 * px::ORBIT_R));
        if hi < lo + 2 {
            return;
        }
        let target = (lo as f32, hi as f32);
        let ground = theme.modal_bg;
        let ascii = self.ascii;
        // (layer, depth, mark): trails and pulses under the heads, the heads behind the orbit under those in front
        let mut marks: Vec<(u8, f32, px::Mark)> = Vec::new();
        let mut add = |sat: px::Sat, orbit: (f32, f32), still: bool| {
            let ms = px::sat_marks(theme, sat, orbit, ground, ascii, still);
            let n = ms.len();
            let depth = sat.angle.sin();
            marks.extend(ms.into_iter().enumerate().map(|(i, m)| (u8::from(i + 1 == n), depth, m)));
        };
        match still {
            Some(members) => {
                for (n, m) in members.iter().filter(|m| m.owner == owner).take(px::SAT_MAX).enumerate() {
                    add(px::Sat { flight: m.flight, angle: n as f32 * px::GOLDEN, age_ms: 0, end_ms: 0 }, target, true);
                }
            }
            None => {
                let Some(sw) = self.live.swarm.get_mut(owner) else { return };
                let orbit = match sw.orbit {
                    Some((from, to, t)) => {
                        let shown = px::ease_orbit(from, to, ms_since(Some(t), now));
                        if to != target {
                            sw.orbit = Some((shown, target, now));
                        }
                        shown
                    }
                    None => {
                        sw.orbit = Some((target, target, now));
                        target
                    }
                };
                for s in &sw.sats {
                    let end_ms = ms_since(s.ended, now);
                    add(
                        px::Sat { flight: s.flight, angle: s.angle_at(now), age_ms: s.age_ms(now), end_ms },
                        orbit,
                        false,
                    );
                }
            }
        }
        let buf = f.buffer_mut();
        let at = |x: u16| (line.x + x, line.y);
        let free: Vec<bool> =
            (a..=b).map(|x| x < line.width && buf.cell(at(x)).is_some_and(|c| c.symbol() == " ")).collect();
        marks.sort_by(|p, q| (p.0, p.1).partial_cmp(&(q.0, q.1)).unwrap_or(std::cmp::Ordering::Equal));
        for (_, _, m) in marks {
            let x = m.x.round();
            if x < a as f32 || x > b as f32 || !free[(x as u16 - a) as usize] {
                continue;
            }
            put(f, at(x as u16), m.glyph, m.color, m.bold);
        }
    }
}
