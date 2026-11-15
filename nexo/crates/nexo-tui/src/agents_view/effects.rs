//! What moves in `/panel` besides the glyphs: a letter that flies from the footer to its row, the burst of a
//! passing test run, the squash of a compaction, the flash of an error, the pulse of a row that starts needing
//! the person, a lane's round popping its ✓ or ✗, and spend and cost rolling to their new values. An effect is
//! born at the first draw that sees its cause and is a pure function of the time since; at rest (no motion, a
//! mono theme, nothing live) nothing asks for a frame and nothing changes between two draws.

use std::collections::HashMap;
use std::time::Instant;

use nexo_agent::supervise::{AgentRow, Ev, EvKind, MailRow, Secs, State};
use nexo_core::mail::Stage;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::Frame;

use super::pulse::PulseFx;
use super::{needs_you, waits, works, AgentsView, CHIP_MS, ENTER_MS, FLIGHT_MS, NEED_MS, POP_MS, TRAIL};
use crate::anim::{progress, PULSE_MS};
use crate::motion::{ease_in_out, fade, route, within, Counter};
use crate::panel_fx as px;
use crate::render::wrap::width;
use crate::theme::Theme;

/// A one-shot effect on a session's row, born when its event was first seen.
#[derive(Debug, Clone)]
pub(super) struct Burst {
    pub key: String,
    pub kind: EvKind,
    pub born: Instant,
}

/// A mail as last seen, and when it reached that stage.
#[derive(Debug, Clone)]
pub(super) struct MailInfo {
    pub row: MailRow,
    pub stage_at: Secs,
}

/// A letter that will leave at the next draw.
#[derive(Debug, Clone)]
pub(super) struct Wish {
    pub from: Option<String>,
    pub to: String,
    pub count: u32,
    pub bounce: bool,
}

/// A letter in the air.
#[derive(Debug, Clone)]
pub(super) struct Letter {
    pub from: Option<String>,
    pub to: String,
    pub born: Instant,
    pub delay_ms: u64,
    pub count: u32,
    pub bounce: bool,
}

/// Everything the effects remember between two draws.
#[derive(Debug, Clone, Default)]
pub(super) struct Fleet {
    /// Set by the first draw with motion on: what is already there does not play.
    pub primed: bool,
    pub last_ev: HashMap<String, Option<(Secs, EvKind, u32)>>,
    pub bursts: Vec<Burst>,
    pub mails: HashMap<String, MailInfo>,
    pub wishes: Vec<Wish>,
    pub letters: Vec<Letter>,
    pub todo: HashMap<String, Counter>,
    pub todo_shown: HashMap<String, f64>,
    pub todo_moving: bool,
    /// Spend and cost as they roll to new values, by row key.
    pub money: HashMap<String, Counter>,
    pub money_shown: HashMap<String, f64>,
    pub pulse: PulseFx,
    /// The last draw had the motion off (animations disabled, or a mono theme): nothing asks for a frame.
    pub off: bool,
}

impl Fleet {
    /// Forgets every running effect (the motion went off); what the mail table knows stays.
    fn calm(&mut self) {
        self.primed = false;
        self.last_ev.clear();
        self.bursts.clear();
        self.wishes.clear();
        self.letters.clear();
        self.todo.clear();
        self.todo_shown.clear();
        self.todo_moving = false;
        self.money.clear();
        self.money_shown.clear();
        self.pulse = PulseFx::default();
    }
}

fn is_bad(s: Stage) -> bool {
    matches!(s, Stage::Refused | Stage::Expired | Stage::Failed)
}

/// How long an event's one-shot effect lives; None for the kinds that have none.
pub(super) fn burst_ms(kind: EvKind) -> Option<u64> {
    Some(match kind {
        EvKind::TestsPass => px::BURST_MS,
        EvKind::TestsFail => px::SHAKE_MS,
        EvKind::GitCommit => px::COMMIT_MS,
        EvKind::GitPush => px::PUSH_MS,
        EvKind::Compact => px::SQUASH_MS,
        EvKind::Error => px::ERROR_MS,
        _ => return None,
    })
}

/// The events of `ribbon` that came after the one that was newest at the last look.
fn fresh(ribbon: &[Ev], old: Option<Option<(Secs, EvKind, u32)>>, clock: Secs) -> &[Ev] {
    let from = |i: Option<usize>| &ribbon[i.unwrap_or(ribbon.len())..];
    match old {
        // a session that just appeared: what happened in the last seconds
        None => from(ribbon.iter().position(|e| e.at >= clock - 3)),
        Some(None) => ribbon,
        Some(Some((at, kind, n))) => match ribbon.iter().rposition(|e| e.at == at && e.kind == kind) {
            Some(i) if ribbon[i].n > n => &ribbon[i..],
            Some(i) => &ribbon[i + 1..],
            None => from(ribbon.iter().position(|e| e.at > at)),
        },
    }
}

fn ms_since(at: Option<Instant>, now: Instant) -> u64 {
    at.map_or(0, |t| now.saturating_duration_since(t).as_millis() as u64)
}

/// A glyph put on the buffer at `cell`, keeping the ground. It never lands on the right half of a wide glyph
/// (the terminal would not show it, and clearing it later would cut the glyph in two), and over the left half
/// it blanks the right one.
fn put(f: &mut Frame, cell: (u16, u16), sym: &str, color: Color, bold: bool) {
    let buf = f.buffer_mut();
    if cell.0 > buf.area.x && buf.cell((cell.0 - 1, cell.1)).is_some_and(|c| width(c.symbol()) > 1) {
        return;
    }
    let Some(c) = buf.cell_mut(cell) else { return };
    let wide = width(c.symbol()) > 1;
    c.set_symbol(sym);
    c.set_fg(color);
    if bold {
        c.set_style(Style::default().add_modifier(Modifier::BOLD));
    }
    if wide {
        if let Some(n) = buf.cell_mut((cell.0 + 1, cell.1)) {
            n.set_symbol(" ");
        }
    }
}

impl AgentsView {
    // ---------------------------------------------------------------- what is live

    /// Whether this session has a live slot that spins: it works, and its newest event is a call in progress.
    pub(super) fn busy(&self, row: &AgentRow) -> bool {
        row.state == State::Working
            && row.ribbon.last().is_some_and(|e| px::live_frame(e.kind, 0, false).is_some() && self.clock - e.at <= 120)
    }

    /// Milliseconds since the view first drew (the phase of everything that breathes).
    pub(super) fn since_open(&self, now: Instant) -> u64 {
        ms_since(self.opened, now)
    }

    /// Sub-agents of `row` that still run.
    pub(super) fn running_subs(row: &AgentRow) -> usize {
        row.subs.iter().filter(|(_, s)| *s != State::Done).count()
    }

    /// Whether something moves on its own (a breathing glyph, a row sliding in, a glyph changing colour, a
    /// pulse, a letter, a burst, a pop, a figure rolling): the loop keeps drawing. At rest it is false, so
    /// nothing repaints.
    pub fn animating(&self, now: Instant) -> bool {
        // a static page (the policy) and a draw with the motion off move nothing, whatever the sessions do
        if self.still || self.live.off || self.policy_page {
            return false;
        }
        let l = &self.live;
        self.rows.iter().any(|r| waits(r) || works(r) || r.retry_secs.is_some() || Self::running_subs(r) > 0)
            || self.fx.values().any(|x| {
                within(Some(x.born), now, x.delay_ms + ENTER_MS)
                    || within(x.changed, now, CHIP_MS)
                    || within(x.needs, now, NEED_MS)
                    || x.round.is_some_and(|(t, _)| within(Some(t), now, POP_MS))
            })
            || within(self.need_blink, now, NEED_MS)
            || self.fly_key.is_some()
            || self.flight.as_ref().is_some_and(|(_, t)| within(Some(*t), now, FLIGHT_MS))
            || self.pulse_at.as_ref().is_some_and(|(_, t)| now < *t || within(Some(*t), now, PULSE_MS))
            || !l.wishes.is_empty()
            || l.letters.iter().any(|x| within(Some(x.born), now, x.delay_ms + px::LETTER_MS.max(px::EDGE_MS)))
            || l.bursts.iter().any(|b| burst_ms(b.kind).is_some_and(|ms| within(Some(b.born), now, ms)))
            || l.todo_moving
            || l.pulse.animating(now)
    }

    /// Learns what changed since the last draw: new rows (they slide in), new states (the glyph cross-fades),
    /// a row that starts needing the person (it pulses, NEEDS YOU blinks), a lane's round that ended (it pops),
    /// a nudge just sent (its row pulses), new events and mails (their effects start). Without motion nothing
    /// is kept.
    pub(super) fn note(&mut self, now: Instant, fx: bool) {
        if self.opened.is_none() {
            self.opened = Some(now);
        }
        self.live.off = !fx;
        if !fx {
            self.fx.clear();
            self.fly_key = None;
            self.flight = None;
            self.pulse_at = None;
            self.need_blink = None;
            self.live.calm();
            return;
        }
        let first = self.fx.is_empty();
        let primed = self.live.primed;
        let mut blink = false;
        self.fx.retain(|k, _| self.rows.iter().any(|r| &r.key == k));
        for (i, r) in self.rows.iter().enumerate() {
            let needing = needs_you(r);
            let round = r.lane.as_ref().and_then(|l| l.last_round.as_ref()).map(|(at, word, _)| (*at, word == "ok"));
            match self.fx.get_mut(&r.key) {
                Some(x) => {
                    if super::chip(x.state).1 != super::chip(r.state).1 {
                        x.prev = x.state;
                        x.changed = Some(now);
                    }
                    x.state = r.state;
                    if needing && !x.needing {
                        x.needs = Some(now);
                        blink = true;
                    }
                    x.needing = needing;
                    if let Some((at, ok)) = round {
                        if x.last_round != Some(at) {
                            x.round = Some((now, ok));
                        }
                    }
                    x.last_round = round.map(|(at, _)| at);
                }
                None => {
                    let delay_ms = if first { (i as u64 * super::STAGGER_MS).min(super::STAGGER_MAX) } else { 0 };
                    let fresh_need = primed && needing;
                    blink |= fresh_need;
                    self.fx.insert(
                        r.key.clone(),
                        super::RowFx {
                            born: now,
                            delay_ms,
                            state: r.state,
                            prev: r.state,
                            changed: None,
                            needs: fresh_need.then_some(now),
                            round: None,
                            needing,
                            last_round: round.map(|(at, _)| at),
                        },
                    );
                }
            }
        }
        if blink {
            self.need_blink = Some(now);
        }
        if let Some(k) = self.fly_key.take() {
            // the row pulses when the letter lands
            self.pulse_at = Some((k.clone(), now + std::time::Duration::from_millis(FLIGHT_MS)));
            self.flight = Some((k, now));
        }
        self.live_note(now);
    }

    /// New events, letters, the rolling figures and the pulse row, as of this draw.
    fn live_note(&mut self, now: Instant) {
        let primed = self.live.primed;
        let clock = self.clock;
        self.live.bursts.retain(|b| burst_ms(b.kind).is_some_and(|ms| within(Some(b.born), now, ms)));
        self.live.letters.retain(|l| within(Some(l.born), now, l.delay_ms + px::LETTER_MS.max(px::EDGE_MS)));
        self.live.last_ev.retain(|k, _| self.rows.iter().any(|r| &r.key == k));
        for r in &self.rows {
            let sig = r.ribbon.last().map(|e| (e.at, e.kind, e.n));
            let old = self.live.last_ev.insert(r.key.clone(), sig);
            if !primed {
                continue;
            }
            for e in fresh(&r.ribbon, old, clock) {
                if burst_ms(e.kind).is_some() {
                    self.live.bursts.push(Burst { key: r.key.clone(), kind: e.kind, born: now });
                }
            }
        }
        // letters that were wished for since the last draw leave now, three at a time
        let wishes = std::mem::take(&mut self.live.wishes);
        for (i, w) in wishes.into_iter().take(px::MAX_LETTERS).enumerate() {
            let delay_ms = if i < 3 { 0 } else { (i as u64 - 2) * px::STAGGER_MS };
            self.live.letters.push(Letter {
                from: w.from,
                to: w.to,
                born: now,
                delay_ms,
                count: w.count,
                bounce: w.bounce,
            });
        }
        // todo bars, spend and cost roll to their new values
        let mut moving = false;
        let mut have: Vec<String> = Vec::new();
        let mut paid: Vec<String> = Vec::new();
        for r in &self.rows {
            if let Some((d, t)) = r.todo.filter(|(_, t)| *t > 0) {
                let c = self.live.todo.entry(r.key.clone()).or_default();
                let (v, mv) = c.sample(d as f64 / t as f64, now, primed);
                self.live.todo_shown.insert(r.key.clone(), v);
                moving |= mv;
                have.push(r.key.clone());
            }
            let money = r.lane.as_ref().map(|l| l.spend_today).or(r.cost_usd);
            if let Some(m) = money {
                let c = self.live.money.entry(r.key.clone()).or_default();
                let (v, mv) = c.sample(m, now, primed);
                self.live.money_shown.insert(r.key.clone(), v);
                moving |= mv;
                paid.push(r.key.clone());
            }
        }
        self.live.todo.retain(|k, _| have.contains(k));
        self.live.todo_shown.retain(|k, _| have.contains(k));
        self.live.money.retain(|k, _| paid.contains(k));
        self.live.money_shown.retain(|k, _| paid.contains(k));
        self.live.todo_moving = moving;
        self.pulse_note(now);
        self.live.primed = true;
    }

    /// A row's spend or cost as shown now: rolling towards `target`, or `target` with the motion off.
    pub(super) fn rolled(&self, key: &str, target: f64, fx: bool) -> f64 {
        if fx {
            self.live.money_shown.get(key).copied().unwrap_or(target)
        } else {
            target
        }
    }

    // ---------------------------------------------------------------- mail

    /// A letter flies between the cards of the sender and the receiver of each mail that appeared since the
    /// last look (from the header when the sender has no card); a mail that fails turns back half way.
    pub fn fly_new_mail(&mut self, feed: Vec<MailRow>, rows: &[AgentRow]) {
        let at = self.clock;
        self.ingest_mail(feed, rows, at);
    }

    pub(super) fn ingest_mail(&mut self, feed: Vec<MailRow>, rows: &[AgentRow], at: Secs) {
        let first = self.mails_seen.is_none();
        let seen = self.mails_seen.get_or_insert_with(Default::default);
        let find = |who: &str| {
            rows.iter()
                .find(|r| r.key == who)
                .or_else(|| rows.iter().find(|r| r.name.eq_ignore_ascii_case(who)))
                .map(|r| r.key.clone())
        };
        let mut legacy: Option<String> = None;
        let mut out: Vec<(Option<String>, String, bool)> = Vec::new();
        for m in feed {
            let new = seen.insert(m.id.clone());
            let prev = self.live.mails.get(&m.id).map(|i| (i.row.stage, i.stage_at));
            let (stage_at, flies, bounces) = match prev {
                None => (
                    if first { m.at } else { at },
                    new && !first && !is_bad(m.stage) && m.stage != Stage::Held,
                    new && !first && is_bad(m.stage),
                ),
                Some((p, _)) if p != m.stage => {
                    (at, p == Stage::Held && !is_bad(m.stage), !is_bad(p) && is_bad(m.stage))
                }
                Some((_, t)) => (t, false, false),
            };
            self.live.mails.insert(m.id.clone(), MailInfo { row: m.clone(), stage_at });
            if !(flies || bounces) {
                continue;
            }
            let Some(to) = find(&m.to) else { continue };
            match find(&m.from) {
                Some(f) if f != to => out.push((Some(f), to, bounces)),
                Some(_) => {}
                None if flies => legacy = Some(to),
                None => {}
            }
        }
        // more than five from one sender in one look: a single letter that says how many
        let mut wishes: Vec<Wish> = Vec::new();
        for (from, to, bounce) in &out {
            let many = from.is_some() && !bounce && out.iter().filter(|o| o.0 == *from && !o.2).count() > 5;
            match wishes.iter_mut().find(|w| many && w.from == *from && !w.bounce) {
                Some(w) => {
                    w.count += 1;
                    w.to = to.clone();
                }
                None => wishes.push(Wish { from: from.clone(), to: to.clone(), count: 1, bounce: *bounce }),
            }
        }
        self.live.wishes.extend(wishes);
        if let Some(k) = legacy {
            self.send_letter(&k);
        }
    }

    /// Sends a letter from the header to a session's card (tests inject one with it). It leaves at the next
    /// draw, and with the motion off it never shows.
    pub fn send_letter(&mut self, key: &str) {
        self.fly_key = Some(key.to_string());
    }

    /// Milliseconds since a letter for this session landed on its row, while the landing lasts.
    pub(super) fn landing_ms(&self, key: &str, now: Instant) -> Option<u64> {
        self.live
            .letters
            .iter()
            .filter(|l| l.to == key && !l.bounce && now >= l.born + std::time::Duration::from_millis(l.delay_ms))
            .filter_map(|l| (ms_since(Some(l.born), now).saturating_sub(l.delay_ms)).checked_sub(px::HOP_MS))
            .find(|ms| *ms <= px::LAND_MS)
    }

    /// Milliseconds since a letter left this session's card, while the lift lasts.
    pub(super) fn lifting_ms(&self, key: &str, now: Instant) -> Option<u64> {
        self.live
            .letters
            .iter()
            .filter(|l| l.from.as_deref() == Some(key) && now >= l.born + std::time::Duration::from_millis(l.delay_ms))
            .map(|l| ms_since(Some(l.born), now).saturating_sub(l.delay_ms))
            .find(|ms| *ms < px::LIFT_MS)
    }

    /// The mail badge of a session: `✉n` for the mails waiting for it (`✓n` for a moment as a letter lands).
    pub(super) fn badge(&self, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) -> Option<Span<'static>> {
        let n = row.unread + row.held;
        if n == 0 {
            return None;
        }
        let landed = fx && self.landing_ms(&row.key, now).is_some_and(|ms| ms < 400);
        let glyph = match (landed, self.ascii) {
            (true, false) => "✓",
            (true, true) => "+",
            (false, false) => "✉",
            (false, true) => "M",
        };
        let base = if row.held > 0 { theme.warn2 } else { theme.accent2 };
        let color = if landed {
            theme.success
        } else if fx {
            let breath = px::breath(ms_since(self.opened, now), px::WAIT_BREATH_MS);
            fade(theme, base, theme.dim, 0.5 * breath)
        } else {
            base
        };
        Some(Span::styled(format!("{glyph}{}", n.min(99)), theme.bold(color)))
    }

    /// What the sender's card says about its last mail for twenty seconds: `✉ api ✓✓ 12s`.
    pub(super) fn receipt(&self, row: &AgentRow, theme: &Theme) -> Option<Span<'static>> {
        let a = self.ascii;
        let (info, age) = self
            .live
            .mails
            .values()
            .filter(|i| i.row.from == row.key || i.row.from.eq_ignore_ascii_case(&row.name))
            .map(|i| (i, (self.clock - i.stage_at).max(0)))
            .filter(|(_, age)| *age <= px::RECEIPT_SECS)
            .min_by_key(|(_, age)| *age)?;
        let peer = self
            .rows
            .iter()
            .find(|r| r.key == info.row.to || r.name.eq_ignore_ascii_case(&info.row.to))
            .map_or(info.row.to.as_str(), |r| r.name.as_str());
        let peer = crate::render::wrap::truncate(peer, 10);
        let (mark, color) = match info.row.stage {
            Stage::Sent => (if a { "." } else { "·" }, theme.accent2),
            Stage::Held => ("held", theme.warn2),
            Stage::Delivered => (if a { "+" } else { "✓" }, theme.accent),
            Stage::Read => (if a { "++" } else { "✓✓" }, theme.success),
            Stage::Refused => (if a { "x refused" } else { "✗ refused" }, theme.error),
            Stage::Expired => (if a { "x expired" } else { "✗ expired" }, theme.error),
            Stage::Failed => (if a { "x failed" } else { "✗ failed" }, theme.error),
        };
        let mail = if a { "M" } else { "✉" };
        Some(Span::styled(format!("{mail} {peer} {mark} {}s", age), theme.style(color)))
    }

    // ---------------------------------------------------------------- row effects

    /// How squashed the row of `key` is (a compaction just happened).
    pub(super) fn squash_of(&self, key: &str, now: Instant) -> f32 {
        self.live
            .bursts
            .iter()
            .filter(|b| b.key == key && b.kind == EvKind::Compact)
            .map(|b| px::squash_level(ms_since(Some(b.born), now)))
            .fold(0.0, f32::max)
    }

    /// The wash a row's line takes now and how strong (0..1): an error's flash, a commit, a passing or failing
    /// run, a letter lifting off or landing, the pulse of a row that starts needing the person, a lane's round
    /// popping. The strongest wins; none at rest.
    pub(super) fn row_tint(&self, key: &str, theme: &Theme, now: Instant) -> Option<(Color, f32)> {
        let mut best: Option<(Color, f32)> = None;
        let mut take = |c: Color, s: f32| {
            if s > 0.0 && best.is_none_or(|(_, b)| s > b) {
                best = Some((c, s));
            }
        };
        for b in self.live.bursts.iter().filter(|b| b.key == key) {
            let ms = ms_since(Some(b.born), now);
            match b.kind {
                EvKind::Error => take(theme.error, px::hump_ms(ms, px::ERROR_MS)),
                EvKind::TestsFail => take(theme.error, 0.7 * px::hump_ms(ms, px::SHAKE_MS)),
                EvKind::TestsPass => take(theme.success, 0.6 * px::hump_ms(ms, px::BURST_MS)),
                EvKind::GitCommit => take(theme.accent, 0.6 * px::hump_ms(ms, px::COMMIT_MS)),
                _ => {}
            }
        }
        if let Some(ms) = self.landing_ms(key, now) {
            take(theme.accent2, 0.7 * px::hump_ms(ms, px::LAND_MS));
        }
        if let Some(ms) = self.lifting_ms(key, now) {
            take(theme.accent2, 0.6 * px::hump_ms(ms, px::LIFT_MS));
        }
        if let Some((k, t)) = &self.pulse_at {
            if k == key && now >= *t {
                take(theme.accent2, 0.7 * px::hump_ms(ms_since(Some(*t), now), PULSE_MS));
            }
        }
        if let Some(x) = self.fx.get(key) {
            if let Some(t) = x.needs {
                take(theme.warn2, 0.8 * px::hump_ms(ms_since(Some(t), now), NEED_MS));
            }
            if let Some((t, ok)) = x.round {
                take(if ok { theme.success } else { theme.error }, 0.6 * px::hump_ms(ms_since(Some(t), now), POP_MS));
            }
        }
        best
    }

    /// The effects drawn over a row's line after its text: the particles of a passing run around the glyph
    /// (on blank cells of the tree only), the shake of a failing one, the squash of a compaction. `line` runs
    /// from the glyph's cell to the line's end; `tree` bounds the particles.
    pub(super) fn draw_row_fx(&self, f: &mut Frame, line: Rect, tree: Rect, key: &str, theme: &Theme, now: Instant) {
        let (at, end) = ((line.x, line.y), line.x + line.width);
        let a = self.ascii;
        let inside = |x: i32, y: i32| {
            x >= tree.x as i32
                && x < (tree.x + tree.width) as i32
                && y >= tree.y as i32
                && y < (tree.y + tree.height) as i32
        };
        for b in self.live.bursts.iter().filter(|b| b.key == key) {
            let ms = ms_since(Some(b.born), now);
            let (cx, cy) = (at.0 as i32, at.1 as i32);
            match b.kind {
                EvKind::TestsPass => {
                    let c = fade(theme, theme.success, theme.dim2, ms as f32 / px::BURST_MS as f32);
                    for (dx, dy) in px::particles(ms) {
                        let (x, y) = (cx + dx, cy + dy);
                        if !inside(x, y) {
                            continue;
                        }
                        if f.buffer_mut().cell((x as u16, y as u16)).is_some_and(|c| c.symbol() == " ") {
                            put(f, (x as u16, y as u16), if a { "." } else { "∙" }, c, false);
                        }
                    }
                }
                EvKind::TestsFail => {
                    let dx = px::shake(ms);
                    if dx != 0 && inside(cx + dx, cy) {
                        let sym = f.buffer_mut().cell((at.0, at.1)).map(|c| c.symbol().to_string());
                        if let Some(sym) = sym {
                            put(f, at, " ", theme.error, false);
                            put(f, ((cx + dx) as u16, at.1), &sym, theme.error, true);
                        }
                    }
                }
                _ => {}
            }
        }
        // a compaction presses the row: its glyph flattens and its text sinks towards the dim colour
        let sq = self.squash_of(key, now);
        if sq > 0.0 {
            put(f, at, if a { "=" } else { "▬" }, theme.accent2, true);
            let buf = f.buffer_mut();
            for x in at.0 + 1..end {
                if let Some(c) = buf.cell_mut((x, at.1)) {
                    let fg = c.fg;
                    if fg != Color::Reset {
                        c.set_fg(fade(theme, fg, theme.dim2, sq));
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------------- the letters

    /// The glyph of a letter in flight: one cell wide in every terminal that draws the text form, else a block.
    fn letter_glyph(&self) -> &'static str {
        if self.ascii {
            "M"
        } else if width("✉") == 1 {
            "✉"
        } else {
            "▪"
        }
    }

    /// The letters of mail between rows: each hops in the padding column left of the tree, from the sender's
    /// row (or the footer rule) to the receiver's. A row out of sight is flown towards: a `▲✉` or `▼✉` stays at
    /// the tree's edge for a moment. A row folded away sends and receives on the line that holds it (its parent
    /// or its group's header). A mail that fails turns back half way.
    pub(super) fn draw_flights(&self, f: &mut Frame, rule: Rect, body: Rect, theme: &Theme, now: Instant) {
        let land = self.landings();
        let rect_of = |key: &str| {
            let i = land.find(key)?;
            self.lines.iter().find(|(c, _)| *c == i).map(|(_, r)| (i, *r))
        };
        let a = self.ascii;
        let glyph = self.letter_glyph();
        for l in &self.live.letters {
            if now < l.born + std::time::Duration::from_millis(l.delay_ms) {
                continue;
            }
            let ms = ms_since(Some(l.born), now).saturating_sub(l.delay_ms);
            let Some(ti) = land.find(&l.to) else { continue };
            let from = l.from.as_deref().and_then(rect_of);
            let Some((_, to_r)) = rect_of(&l.to) else {
                if ms < px::EDGE_MS && !self.lines.is_empty() && body.width >= 4 && body.height > 0 {
                    let above = self.lines.iter().all(|(c, _)| *c > ti);
                    let y = if above { body.y } else { body.y + body.height.saturating_sub(1) };
                    let x = body.x + body.width.saturating_sub(3);
                    let arrow = match (above, a) {
                        (true, false) => "▲",
                        (false, false) => "▼",
                        (true, true) => "^",
                        (false, true) => "v",
                    };
                    put(f, (x, y), arrow, theme.accent2, true);
                    put(f, (x + 1, y), glyph, theme.accent2, true);
                }
                continue;
            };
            if ms >= px::HOP_MS {
                continue;
            }
            let t = ease_in_out(ms as f64 / px::HOP_MS as f64) as f32;
            let t = if l.bounce { px::bounce_p(ms * px::BOUNCE_MS / px::HOP_MS) * 2.0 } else { t };
            let x = body.x.saturating_sub(1);
            let y0 = from.map_or(rule.y, |(_, r)| r.y);
            let y = (y0 as f32 + (to_r.y as f32 - y0 as f32) * t.min(1.0)).round() as u16;
            let (g, c) = if l.bounce && ms * 2 >= px::HOP_MS {
                (px::glyph(EvKind::Error, a), theme.error)
            } else {
                (glyph, theme.accent2)
            };
            put(f, (x, y), g, c, true);
            if l.count > 1 && x + 1 < body.x + body.width {
                for (k, ch) in format!("x{}", l.count).chars().enumerate() {
                    let cx = x + 1 + k as u16;
                    if cx >= body.x + body.width {
                        break;
                    }
                    put(f, (cx, y), &ch.to_string(), theme.accent2, false);
                }
            }
        }
    }

    /// The letter of a message the person sent: it leaves the footer rule in the padding column left of the
    /// tree and climbs to the row it is for, leaving a streak that fades behind it. It is gone on landing, and
    /// the row pulses. A row out of sight is flown to the edge of the tree where the scroll is; a row folded
    /// away, to the line that holds it (its parent or its group's header).
    pub(super) fn draw_letter(&self, f: &mut Frame, rule: Rect, body: Rect, theme: &Theme, now: Instant) {
        let Some((key, at)) = &self.flight else { return };
        if !within(Some(*at), now, FLIGHT_MS) {
            return;
        }
        let Some(target) = self.landing(key) else { return };
        let x = body.x.saturating_sub(1);
        let to = match self.lines.iter().find(|(i, _)| *i == target) {
            Some((_, r)) => (x, r.y),
            None if self.lines.iter().all(|(i, _)| *i < target) => (x, body.y + body.height.saturating_sub(1)),
            None => (x, body.y),
        };
        let t = ease_in_out(progress(Some(*at), now, FLIGHT_MS)) as f32;
        let (head, behind) = route((x, rule.y), to, t, TRAIL);
        let ground = theme.modal_bg;
        for (k, ((cx, cy), across)) in behind.iter().enumerate() {
            let strength = 1.0 - (k as f32 + 1.0) / (TRAIL as f32 + 1.0);
            put(f, (*cx, *cy), if *across { "━" } else { "┃" }, fade(theme, ground, theme.accent2, strength), false);
        }
        put(f, head, self.letter_glyph(), theme.accent2, true);
    }
}

#[cfg(test)]
mod tests;
