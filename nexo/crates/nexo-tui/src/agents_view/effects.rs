//! What moves in `/panel` besides the chips: letters between cards, satellites for sub-agents, the burst of a
//! passing test run, the squash of a compaction, the flash of an error. An effect is born at the first draw that
//! sees its cause and is a pure function of the time since; at rest (no motion, a mono theme, nothing live)
//! nothing asks for a frame and nothing changes between two draws.

use std::collections::HashMap;
use std::time::Instant;

use nexo_agent::supervise::{AgentRow, Ev, EvKind, MailRow, Secs, State};
use nexo_core::mail::Stage;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::Frame;

use super::pulse::PulseFx;
use super::{attention, AgentsView, Layout, CHIP_MS, ENTER_MS, FLIGHT_MS, TRAIL};
use crate::anim::{ease_out, progress, PULSE_MS};
use crate::motion::{ease_in_out, fade, route, within, Counter};
use crate::panel_fx as px;
use crate::render::wrap::width;
use crate::theme::Theme;

/// A one-shot effect on a session's card, born when its event was first seen.
#[derive(Debug, Clone)]
pub(super) struct Burst {
    pub key: String,
    pub kind: EvKind,
    pub born: Instant,
}

/// The motion of one sub-agent's satellite.
#[derive(Debug, Clone)]
pub(super) struct SubFx {
    pub born: Instant,
    /// It appeared while the view was open: its dot flies from the chip to its slot.
    pub spawn: bool,
    pub state: State,
    pub changed: Option<Instant>,
    pub failed: bool,
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
    pub subs: HashMap<String, SubFx>,
    pub mails: HashMap<String, MailInfo>,
    pub wishes: Vec<Wish>,
    pub letters: Vec<Letter>,
    pub todo: HashMap<String, Counter>,
    pub todo_shown: HashMap<String, f64>,
    pub todo_moving: bool,
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
        self.subs.clear();
        self.wishes.clear();
        self.letters.clear();
        self.todo.clear();
        self.todo_shown.clear();
        self.todo_moving = false;
        self.pulse = PulseFx::default();
    }
}

impl SubFx {
    fn animating(&self, now: Instant) -> bool {
        (self.spawn && within(Some(self.born), now, px::SAT_SPAWN_MS))
            || self.changed.is_some_and(|t| {
                within(Some(t), now, if self.failed { px::SAT_FAIL_MS } else { px::SAT_DONE_MS + px::SAT_FADE_MS })
            })
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

    /// Milliseconds the live slot has been spinning, for a session whose slot spins.
    pub(super) fn live_phase(&self, row: &AgentRow, now: Instant, fx: bool) -> px::LivePhase {
        (fx && self.busy(row)).then(|| ms_since(self.opened, now))
    }

    /// Milliseconds since the view first drew (the phase of everything that breathes).
    pub(super) fn since_open(&self, now: Instant) -> u64 {
        ms_since(self.opened, now)
    }

    /// Sub-agents of `row` that still run.
    pub(super) fn running_subs(row: &AgentRow) -> usize {
        row.subs.iter().filter(|(_, s)| *s != State::Done).count()
    }

    /// Whether something moves on its own (a breathing chip, an entering card, a chip changing colour, the
    /// selection gliding, a pulse, a letter, a satellite, a burst, a live slot): the loop keeps drawing. At rest
    /// it is false, so nothing repaints.
    pub fn animating(&self, now: Instant) -> bool {
        // a static page (the policy) and a draw with the motion off move nothing, whatever the sessions do
        if self.still || self.live.off || self.policy_page {
            return false;
        }
        let l = &self.live;
        self.rows
            .iter()
            .any(|r| attention(r.state) || self.busy(r) || r.retry_secs.is_some() || Self::running_subs(r) > 0)
            || self.glide.moving(now)
            || self
                .fx
                .values()
                .any(|x| within(Some(x.born), now, x.delay_ms + ENTER_MS) || within(x.changed, now, CHIP_MS))
            || self.fly_key.is_some()
            || self.flight.as_ref().is_some_and(|(_, t)| within(Some(*t), now, FLIGHT_MS))
            || self.pulse_at.as_ref().is_some_and(|(_, t)| now < *t || within(Some(*t), now, PULSE_MS))
            || !l.wishes.is_empty()
            || l.letters.iter().any(|x| within(Some(x.born), now, x.delay_ms + px::LETTER_MS.max(px::EDGE_MS)))
            || l.bursts.iter().any(|b| burst_ms(b.kind).is_some_and(|ms| within(Some(b.born), now, ms)))
            || l.subs.values().any(|s| s.animating(now))
            || l.todo_moving
            || l.pulse.animating(now)
    }

    /// Learns what changed since the last draw: new sessions (their card enters), new states (the chip
    /// cross-fades), a nudge just sent (its card pulses), new events and mails (their effects start). Without
    /// motion nothing is kept.
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
            self.live.calm();
            return;
        }
        let first = self.fx.is_empty();
        self.fx.retain(|k, _| self.rows.iter().any(|r| &r.key == k));
        for (i, r) in self.rows.iter().enumerate() {
            match self.fx.get_mut(&r.key) {
                Some(x) => {
                    if super::chip(x.state).1 != super::chip(r.state).1 {
                        x.prev = x.state;
                        x.changed = Some(now);
                    }
                    x.state = r.state;
                }
                None => {
                    let delay_ms = if first { (i as u64 * super::STAGGER_MS).min(super::STAGGER_MAX) } else { 0 };
                    self.fx.insert(
                        r.key.clone(),
                        super::RowFx { born: now, delay_ms, state: r.state, prev: r.state, changed: None },
                    );
                }
            }
        }
        if let Some(k) = self.fly_key.take() {
            // the card pulses when the letter lands
            self.pulse_at = Some((k.clone(), now + std::time::Duration::from_millis(FLIGHT_MS)));
            self.flight = Some((k, now));
        }
        self.live_note(now);
    }

    /// New events, satellites, letters, the todo bars and the pulse row, as of this draw.
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
        // satellites
        let mut alive: Vec<String> = Vec::new();
        for r in &self.rows {
            for (sk, st) in &r.subs {
                let id = format!("{}/{}", r.key, sk);
                let failed = match self.rows.iter().find(|x| &x.key == sk) {
                    Some(s) => s.state == State::Looping || s.ribbon.last().is_some_and(|e| e.kind == EvKind::Error),
                    None => *st == State::Looping,
                };
                match self.live.subs.get_mut(&id) {
                    Some(s) => {
                        if std::mem::discriminant(&s.state) != std::mem::discriminant(st) {
                            s.changed = Some(now);
                            s.state = *st;
                        }
                        s.failed = failed;
                    }
                    None => {
                        self.live
                            .subs
                            .insert(id.clone(), SubFx { born: now, spawn: primed, state: *st, changed: None, failed });
                    }
                }
                alive.push(id);
            }
        }
        self.live.subs.retain(|id, _| alive.contains(id));
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
        // todo bars roll to their fill
        let mut moving = false;
        let mut have: Vec<String> = Vec::new();
        for r in &self.rows {
            if let Some((d, t)) = r.todo.filter(|(_, t)| *t > 0) {
                let c = self.live.todo.entry(r.key.clone()).or_default();
                let (v, mv) = c.sample(d as f64 / t as f64, now, primed);
                self.live.todo_shown.insert(r.key.clone(), v);
                moving |= mv;
                have.push(r.key.clone());
            }
        }
        self.live.todo.retain(|k, _| have.contains(k));
        self.live.todo_shown.retain(|k, _| have.contains(k));
        self.live.todo_moving = moving;
        self.pulse_note(now);
        self.live.primed = true;
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

    /// Milliseconds since a letter for this session landed on its card, while the landing lasts.
    pub(super) fn landing_ms(&self, key: &str, now: Instant) -> Option<u64> {
        let flight = match self.drawn {
            Layout::Cards { .. } => px::LIFT_MS + px::FLY_MS,
            _ => px::HOP_MS,
        };
        self.live
            .letters
            .iter()
            .filter(|l| l.to == key && !l.bounce && now >= l.born + std::time::Duration::from_millis(l.delay_ms))
            .filter_map(|l| (ms_since(Some(l.born), now).saturating_sub(l.delay_ms)).checked_sub(flight))
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

    // ---------------------------------------------------------------- card effects

    /// How squashed the card of `key` is (a compaction just happened).
    pub(super) fn squash_of(&self, key: &str, now: Instant) -> f32 {
        self.live
            .bursts
            .iter()
            .filter(|b| b.key == key && b.kind == EvKind::Compact)
            .map(|b| px::squash_level(ms_since(Some(b.born), now)))
            .fold(0.0, f32::max)
    }

    /// The card border with the effects on it: the lock's breath, an error's flash, a commit, a letter lifting
    /// off or landing.
    pub(super) fn border_fx(&self, row: &AgentRow, border: Color, theme: &Theme, now: Instant, fx: bool) -> Color {
        if !fx {
            return border;
        }
        let mut c = border;
        if row.state == State::WaitingPermission {
            let breath = px::breath(ms_since(self.opened, now), px::LOCK_BREATH_MS);
            c = fade(theme, c, theme.warn2, 0.7 * breath);
        }
        for b in self.live.bursts.iter().filter(|b| b.key == row.key) {
            let ms = ms_since(Some(b.born), now);
            match b.kind {
                EvKind::Error => c = fade(theme, c, theme.error, px::hump_ms(ms, px::ERROR_MS)),
                EvKind::GitCommit => c = fade(theme, c, theme.accent, px::hump_ms(ms, px::COMMIT_MS)),
                _ => {}
            }
        }
        if let Some(ms) = self.landing_ms(&row.key, now) {
            c = fade(theme, c, theme.accent2, px::hump_ms(ms, px::LAND_MS));
        }
        if let Some(ms) = self.lifting_ms(&row.key, now) {
            c = fade(theme, c, theme.accent2, px::hump_ms(ms, px::LIFT_MS));
        }
        c
    }

    /// The glow a block or a line takes while a letter lands on it.
    pub(super) fn land_glow(&self, key: &str, now: Instant) -> f32 {
        self.landing_ms(key, now).map_or(0.0, |ms| 0.6 * px::hump_ms(ms, 300))
    }

    /// The satellites of a card, drawn on its bottom border from the right corner: one dot per running
    /// sub-agent (`✓` for a moment when it is done, `✗` for five seconds when it failed), `+N` past four.
    pub(super) fn draw_sats(&self, f: &mut Frame, r: Rect, row: &AgentRow, theme: &Theme, now: Instant, fx: bool) {
        let a = self.ascii;
        let y = r.y + r.height.saturating_sub(1);
        let dot = if a { "o" } else { "●" };
        let mut slot: u16 = 0;
        let mut extra = 0usize;
        for (sk, st) in &row.subs {
            let s = self.live.subs.get(&format!("{}/{}", row.key, sk));
            let (sym, color, bold): (&str, Color, bool) = if *st != State::Done {
                let breath = if fx { px::breath(ms_since(self.opened, now), px::SAT_BREATH_MS) } else { 0.0 };
                (dot, fade(theme, theme.success, theme.dim2, breath), false)
            } else {
                match s.and_then(|s| s.changed.map(|t| (t, s.failed))) {
                    Some((t, true)) if fx && within(Some(t), now, px::SAT_FAIL_MS) => {
                        (if a { "x" } else { "✗" }, theme.error, true)
                    }
                    Some((t, false)) if fx && within(Some(t), now, px::SAT_DONE_MS + px::SAT_FADE_MS) => {
                        let ms = ms_since(Some(t), now);
                        let gone = ms.saturating_sub(px::SAT_DONE_MS) as f32 / px::SAT_FADE_MS as f32;
                        (if a { "+" } else { "✓" }, fade(theme, theme.success, theme.border, gone), false)
                    }
                    _ => continue,
                }
            };
            if slot >= 4 {
                extra += 1;
                continue;
            }
            let x = px::satellite_x(r, slot);
            slot += 1;
            if x <= r.x + 1 {
                break;
            }
            // a new sub-agent's dot flies from the chip to its slot
            let at = match s {
                Some(s) if fx && s.spawn && within(Some(s.born), now, px::SAT_SPAWN_MS) => {
                    let t = ease_out(progress(Some(s.born), now, px::SAT_SPAWN_MS));
                    let (cx, cy) = (r.x + r.width.saturating_sub(4), r.y + 1);
                    let lerp = |p: u16, q: u16| (p as f64 + (q as f64 - p as f64) * t).round() as u16;
                    (lerp(cx, x), lerp(cy, y))
                }
                _ => (x, y),
            };
            put(f, at, sym, color, bold);
        }
        if extra > 0 {
            let x = px::satellite_x(r, 4);
            if x > r.x + 2 {
                let text = format!("+{extra}");
                for (k, ch) in text.chars().enumerate() {
                    put(f, (x + k as u16, y), &ch.to_string(), theme.success, false);
                }
            }
        }
    }

    /// The effects that sit on the ribbon line of a card: the burst of a passing run, the shake of a failing
    /// one, a commit's three frames, a push's arrow. `x0` is the ribbon's first column, `y` its row.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn draw_ribbon_fx(
        &self,
        f: &mut Frame,
        card: Rect,
        x0: u16,
        y: u16,
        marks: &[(usize, EvKind, Secs)],
        row: &AgentRow,
        theme: &Theme,
        now: Instant,
    ) {
        let a = self.ascii;
        let inside = |x: i32, yy: i32| {
            x > card.x as i32
                && x < (card.x + card.width) as i32 - 1
                && yy > card.y as i32
                && yy < (card.y + card.height) as i32 - 1
        };
        for b in self.live.bursts.iter().filter(|b| b.key == row.key) {
            let ms = ms_since(Some(b.born), now);
            let Some((col, _, _)) = marks.iter().rev().find(|m| m.1 == b.kind) else { continue };
            let (cx, cy) = (x0 as i32 + *col as i32, y as i32);
            match b.kind {
                EvKind::TestsPass => {
                    let fadeout = fade(theme, theme.success, theme.dim2, ms as f32 / px::BURST_MS as f32);
                    for (dx, dy) in px::particles(ms) {
                        let (x, yy) = (cx + dx, cy + dy);
                        if !inside(x, yy) {
                            continue;
                        }
                        let blank = f.buffer_mut().cell((x as u16, yy as u16)).is_some_and(|c| c.symbol() == " ");
                        if blank {
                            put(f, (x as u16, yy as u16), if a { "." } else { "∙" }, fadeout, false);
                        }
                    }
                }
                EvKind::TestsFail => {
                    let dx = px::shake(ms);
                    if dx != 0 && inside(cx, cy) && inside(cx + dx, cy) {
                        put(f, (cx as u16, cy as u16), " ", theme.error, false);
                        put(f, ((cx + dx) as u16, cy as u16), px::glyph(EvKind::TestsFail, a), theme.error, true);
                    }
                }
                EvKind::GitCommit if ms < px::COMMIT_FRAME_MS * 3 && inside(cx, cy) => {
                    put(f, (cx as u16, cy as u16), px::commit_frame(ms, a), theme.accent, false);
                }
                EvKind::GitPush if ms < px::PUSH_MS => {
                    // the arrow comes up from the border and settles in its cell
                    let p = px::push_p(ms);
                    let (x, yy) = if p < 0.34 {
                        (cx + 1, cy + 1)
                    } else if p < 0.67 {
                        (cx + 1, cy)
                    } else {
                        (cx, cy)
                    };
                    if inside(cx, cy) && inside(x, cy) && yy < (card.y + card.height) as i32 {
                        put(f, (x as u16, yy as u16), px::glyph(EvKind::GitPush, a), theme.accent, false);
                    }
                }
                _ => {}
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

    /// The letters of mail between cards, over the body. A card out of sight is flown towards: a `▲✉` or
    /// `▼✉` stays at the body's edge for a moment.
    pub(super) fn draw_flights(
        &self,
        f: &mut Frame,
        rule: Rect,
        body: Rect,
        shown: &[AgentRow],
        theme: &Theme,
        now: Instant,
    ) {
        let rect_of = |key: &str| {
            let i = shown.iter().position(|r| r.key == key)?;
            self.cards.iter().find(|(c, _)| *c == i).map(|(_, r)| (i, *r))
        };
        let ground = theme.modal_bg;
        for l in &self.live.letters {
            if now < l.born + std::time::Duration::from_millis(l.delay_ms) {
                continue;
            }
            let ms = ms_since(Some(l.born), now).saturating_sub(l.delay_ms);
            let a = self.ascii;
            let glyph = self.letter_glyph();
            let Some(ti) = shown.iter().position(|r| r.key == l.to) else { continue };
            let from = l.from.as_deref().and_then(rect_of);
            let Some((_, to_r)) = rect_of(&l.to) else {
                // out of sight: above or below the cards that are drawn
                if ms < px::EDGE_MS && !self.cards.is_empty() && body.width >= 4 && body.height > 0 {
                    let above = self.cards.iter().all(|(c, _)| *c > ti);
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
            let lines = matches!(self.drawn, Layout::List);
            let blocks = matches!(self.drawn, Layout::Blocks { .. });
            if lines || blocks {
                if ms >= px::HOP_MS {
                    continue;
                }
                let t = ease_in_out(ms as f64 / px::HOP_MS as f64) as f32;
                let t = if l.bounce { px::bounce_p(ms * px::BOUNCE_MS / px::HOP_MS) * 2.0 } else { t };
                let head = if lines {
                    let x = body.x.saturating_sub(1);
                    let y0 = from.map_or(rule.y, |(_, r)| r.y);
                    let y = (y0 as f32 + (to_r.y as f32 - y0 as f32) * t).round() as u16;
                    (x, y)
                } else {
                    let centre = |r: Rect| (r.x + r.width / 2, r.y + r.height / 2);
                    let c0 = from.map_or((body.x, body.y), |(_, r)| centre(r));
                    px::hop(c0, centre(to_r), t, 5)
                };
                put(f, head, glyph, theme.accent2, true);
                continue;
            }
            // cards: a curve from the top border of the sender to the top border of the receiver
            let to = (to_r.x + 3, to_r.y);
            let start = match from {
                Some((_, r)) => (r.x + r.width.saturating_sub(4), r.y),
                None => (rule.x, rule.y),
            };
            let path = px::Path::new(start, to);
            if l.bounce {
                if ms >= px::BOUNCE_MS {
                    continue;
                }
                let head = path.at(px::bounce_p(ms), body);
                let (g, c) = if ms * 2 >= px::BOUNCE_MS {
                    (px::glyph(EvKind::Error, a), theme.error)
                } else {
                    (glyph, theme.accent2)
                };
                put(f, head, g, c, true);
                continue;
            }
            if ms >= px::LETTER_MS {
                continue;
            }
            for (k, cell) in px::trail(ms, &path, body).into_iter().enumerate() {
                let sym = px::trail_glyph(k, a);
                put(f, cell, sym, px::trail_color(theme, ground, k + 1), false);
            }
            let (head, g, c) = match px::leg(ms) {
                px::Leg::Land(p) => {
                    let done = p > 0.3;
                    (
                        path.at(1.0, body),
                        if done { px::glyph(EvKind::MsgRead, a) } else { glyph },
                        if done { theme.success } else { theme.accent2 },
                    )
                }
                _ => (path.at(px::path_p(ms), body), glyph, theme.accent2),
            };
            put(f, head, g, c, true);
            if l.count > 1 {
                let label = format!("x{}", l.count);
                for (k, ch) in label.chars().enumerate() {
                    // the label stays inside the body, whatever the width left of the right edge
                    let x = head.0 + 1 + k as u16;
                    if x >= body.x + body.width {
                        break;
                    }
                    put(f, (x, head.1), &ch.to_string(), theme.accent2, false);
                }
            }
        }
    }

    /// The first letter, kept as it was: it leaves the left end of the rule under the header, runs along the
    /// rule to the column of the card it is for, then down that column (the card borders and the gaps, never
    /// the text) to the middle of the card's left edge, leaving a streak that fades behind it. It is gone on
    /// landing, and the card pulses.
    pub(super) fn draw_letter(
        &self,
        f: &mut Frame,
        rule: Rect,
        body: Rect,
        shown: &[AgentRow],
        theme: &Theme,
        now: Instant,
    ) {
        let Some((key, at)) = &self.flight else { return };
        if !within(Some(*at), now, FLIGHT_MS) {
            return;
        }
        let Some(i) = shown.iter().position(|r| &r.key == key) else { return };
        // a card out of sight is flown towards: to the foot of the body (or its head) where the scroll is
        // lines carry their marks at the left edge, so the letter runs in the padding column just outside them
        let lines = self.cards.first().is_some_and(|(_, r)| r.height == 1);
        let lane = |x: u16| if lines { x.saturating_sub(1) } else { x };
        let to = match self.cards.iter().find(|(c, _)| *c == i) {
            Some((_, r)) => (lane(r.x), r.y + r.height / 2),
            None if self.cards.iter().all(|(c, _)| *c < i) => (lane(body.x), body.y + body.height.saturating_sub(1)),
            None => (lane(body.x), body.y),
        };
        let t = ease_in_out(progress(Some(*at), now, FLIGHT_MS)) as f32;
        let (head, behind) = route((lane(rule.x), rule.y), to, t, TRAIL);
        let ground = theme.modal_bg;
        for (k, ((x, y), across)) in behind.iter().enumerate() {
            let strength = 1.0 - (k as f32 + 1.0) / (TRAIL as f32 + 1.0);
            put(f, (*x, *y), if *across { "━" } else { "┃" }, fade(theme, ground, theme.accent2, strength), false);
        }
        put(f, head, self.letter_glyph(), theme.accent2, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexo_agent::supervise::EvDetail;

    fn ev(at: Secs, kind: EvKind, n: u32) -> Ev {
        Ev { at, kind, n, detail: EvDetail::None }
    }

    #[test]
    fn fresh_events_are_those_after_the_newest_one_seen() {
        let r = vec![ev(10, EvKind::Read, 1), ev(11, EvKind::Edit, 1), ev(12, EvKind::TestsPass, 1)];
        assert_eq!(fresh(&r, Some(Some((11, EvKind::Edit, 1))), 100).len(), 1);
        assert_eq!(fresh(&r, Some(Some((12, EvKind::TestsPass, 1))), 100).len(), 0);
        assert_eq!(fresh(&r, Some(None), 100).len(), 3);
        // the newest one folded another of its kind
        let r = vec![ev(10, EvKind::Edit, 3)];
        assert_eq!(fresh(&r, Some(Some((10, EvKind::Edit, 2))), 100).len(), 1);
        // a session that just appeared brings only the last seconds
        let r = vec![ev(10, EvKind::Read, 1), ev(98, EvKind::Edit, 1)];
        assert_eq!(fresh(&r, None, 100).len(), 1);
        // the one it knew has scrolled out: what is later counts
        let r = vec![ev(20, EvKind::Read, 1), ev(21, EvKind::Edit, 1)];
        assert_eq!(fresh(&r, Some(Some((10, EvKind::Run, 1))), 100).len(), 2);
    }

    #[test]
    fn only_the_kinds_with_an_effect_have_a_lifetime() {
        assert_eq!(burst_ms(EvKind::TestsPass), Some(400));
        assert_eq!(burst_ms(EvKind::Compact), Some(360));
        assert_eq!(burst_ms(EvKind::Read), None);
    }

    #[test]
    fn failing_stages_are_bad_and_the_others_are_not() {
        for s in [Stage::Refused, Stage::Expired, Stage::Failed] {
            assert!(is_bad(s));
        }
        for s in [Stage::Sent, Stage::Held, Stage::Delivered, Stage::Read] {
            assert!(!is_bad(s));
        }
    }

    // ------------------------------------------------------------------ motion audit

    use nexo_agent::supervise::SourceKind;
    use nexo_core::mail::Stage as MailStage;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use std::time::Duration;

    const NOW: i64 = 1_000_000;

    fn agent(key: &str, state: State) -> AgentRow {
        AgentRow {
            key: key.into(),
            name: key.into(),
            source: SourceKind::Nexo,
            program: "nexo".into(),
            state,
            since: NOW - 60,
            last_step: Some("Edit  src/lib.rs".into()),
            idle_secs: Some(2),
            cost_usd: None,
            model: None,
            cwd: Some(format!("/home/dev/{key}")),
            permission_mode: None,
            parent: None,
            last_action: None,
            paused: false,
            ribbon: vec![Ev { at: NOW - 3, kind: EvKind::Edit, n: 1, detail: nexo_agent::supervise::EvDetail::None }],
            spark: vec![1, 2, 3],
            todo: Some((1, 4)),
            tests: None,
            subs: vec![],
            unread: 0,
            held: 0,
            retry_secs: None,
        }
    }

    fn mail(id: &str, from: &str, to: &str) -> MailRow {
        MailRow {
            id: id.into(),
            from: from.into(),
            to: to.into(),
            summary: "ship it".into(),
            stage: MailStage::Sent,
            at: NOW,
            bytes: 7,
            n: 1,
        }
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    fn paint(v: &mut AgentsView, w: u16, h: u16, now: Instant, fx: bool) -> Buffer {
        let theme = Theme::by_name("dark").unwrap();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| v.draw(f, f.area(), &theme, now, fx)).unwrap();
        t.backend().buffer().clone()
    }

    fn row_text(b: &Buffer, y: u16, x0: u16, x1: u16) -> String {
        (x0..x1).map(|x| b[(x, y)].symbol().to_string()).collect()
    }

    #[test]
    fn a_draw_with_the_motion_off_asks_for_no_frame_even_when_the_fleet_is_busy() {
        let t0 = Instant::now();
        let mut v =
            AgentsView::with_rows(vec![agent("a", State::Working), agent("b", State::WaitingPermission)], NOW, false);
        paint(&mut v, 100, 30, t0, true);
        assert!(v.animating(at(t0, 500)), "a waiting card breathes");
        // the motion went off (theme switched to mono, animations disabled): the loop must rest
        paint(&mut v, 100, 30, at(t0, 600), false);
        assert!(!v.animating(at(t0, 700)), "a still draw must not keep the loop at full rate");
        paint(&mut v, 100, 30, at(t0, 800), true);
        assert!(v.animating(at(t0, 900)), "and it moves again when the motion is back");
    }

    #[test]
    fn a_static_page_asks_for_no_frame() {
        let t0 = Instant::now();
        let mut v = AgentsView::with_rows(vec![agent("a", State::WaitingPermission)], NOW, false);
        paint(&mut v, 100, 30, t0, true);
        assert!(v.animating(t0));
        v.policy_page = true;
        assert!(!v.animating(at(t0, 100)), "the policy page has nothing that moves");
    }

    #[test]
    fn a_burst_of_mail_flies_a_few_letters_and_the_panel_is_calm_again_within_three_seconds() {
        let t0 = Instant::now();
        let rows: Vec<AgentRow> = (0..60).map(|i| agent(&format!("s{i}"), State::Done)).collect();
        let mut v = AgentsView::with_rows(rows.clone(), NOW, false);
        v.set_rows(
            rows.iter()
                .cloned()
                .map(|mut r| {
                    r.ribbon.clear();
                    r.todo = None;
                    r
                })
                .collect(),
        );
        let rows = v.rows().to_vec();
        for ms in [0, 3000] {
            paint(&mut v, 140, 40, at(t0, ms), true);
        }
        v.fly_new_mail(vec![mail("seed", "s0", "s1")], &rows);
        paint(&mut v, 140, 40, at(t0, 3100), true);
        // thirty senders at once
        let feed: Vec<MailRow> = std::iter::once(mail("seed", "s0", "s1"))
            .chain((1..31).map(|i| mail(&format!("m{i}"), &format!("s{i}"), &format!("s{}", i + 1))))
            .collect();
        v.fly_new_mail(feed, &rows);
        let sent = at(t0, 4000);
        paint(&mut v, 140, 40, sent, true);
        assert!(v.live.letters.len() <= px::MAX_LETTERS, "{} letters in the air", v.live.letters.len());
        assert!(v.animating(at(t0, 4500)));
        // the loop draws while the panel asks for frames: when does it rest?
        let mut ms = 0;
        while v.animating(at(sent, ms)) && ms < 10_000 {
            ms += 33;
            paint(&mut v, 140, 40, at(sent, ms), true);
        }
        assert!(ms <= 3000, "still moving {ms} ms after the burst");
    }

    #[test]
    fn nothing_lands_on_the_right_half_of_a_wide_glyph_and_the_left_half_takes_both() {
        let mut t = Terminal::new(TestBackend::new(12, 2)).unwrap();
        t.draw(|f| {
            f.render_widget(ratatui::widgets::Paragraph::new("世界"), Rect::new(0, 0, 12, 1));
            put(f, (1, 0), "✉", Color::Red, true);
            assert_eq!(f.buffer_mut()[(0, 0)].symbol(), "世", "the glyph is whole");
            assert_ne!(f.buffer_mut()[(1, 0)].symbol(), "✉", "nothing sits on its right half");
            put(f, (2, 0), "✉", Color::Red, true);
            assert_eq!(f.buffer_mut()[(2, 0)].symbol(), "✉");
            assert_eq!(f.buffer_mut()[(3, 0)].symbol(), " ", "the right half of the glyph it replaced is cleared");
        })
        .unwrap();
    }

    #[test]
    fn the_count_of_a_letter_stays_inside_the_body() {
        let mut v = AgentsView::with_rows(vec![agent("a", State::Done), agent("b", State::Done)], NOW, false);
        let t0 = Instant::now();
        v.drawn = Layout::Cards { cols: 1, card_w: 20 };
        // the receiver's card ends two columns before the body does
        let body = Rect::new(0, 2, 10, 6);
        v.cards = vec![(0, Rect::new(0, 2, 4, 3)), (1, Rect::new(5, 4, 5, 4))];
        v.live.letters.push(Letter { from: None, to: "b".into(), born: t0, delay_ms: 0, count: 5, bounce: false });
        let rule = Rect::new(0, 1, 10, 1);
        let shown = v.rows().to_vec();
        let theme = Theme::by_name("dark").unwrap();
        let mut t = Terminal::new(TestBackend::new(30, 10)).unwrap();
        // landed: the head sits on the receiver's card
        let now = at(t0, px::LIFT_MS + px::FLY_MS + 10);
        t.draw(|f| v.draw_flights(f, rule, body, &shown, &theme, now)).unwrap();
        let b = t.backend().buffer();
        for y in 0..10 {
            for x in 10..30 {
                assert_eq!(b[(x, y)].symbol(), " ", "a cell outside the body was written at {x},{y}");
            }
        }
    }

    #[test]
    fn a_fail_mark_and_a_commit_mark_never_leave_their_card() {
        // a card two columns wide inside: the ribbon column is the card's own border
        let mut v = AgentsView::with_rows(vec![agent("a", State::Working)], NOW, false);
        let t0 = Instant::now();
        v.live.bursts.push(Burst { key: "a".into(), kind: EvKind::GitCommit, born: t0 });
        let theme = Theme::by_name("dark").unwrap();
        let row = v.rows()[0].clone();
        let card = Rect::new(2, 2, 6, 5);
        let marks = vec![(30usize, EvKind::GitCommit, NOW)];
        let mut t = Terminal::new(TestBackend::new(40, 10)).unwrap();
        t.draw(|f| v.draw_ribbon_fx(f, card, 3, 4, &marks, &row, &theme, t0)).unwrap();
        let b = t.backend().buffer();
        let touched = (0..10).flat_map(|y| (0..40).map(move |x| (x, y))).any(|(x, y)| b[(x, y)].symbol() != " ");
        assert!(!touched, "a mark far past the card's edge was drawn");
    }

    #[test]
    fn the_pulse_row_flashes_once_then_rests_even_when_a_figure_rises_every_poll() {
        let t0 = Instant::now();
        let mut v = AgentsView::with_rows(vec![agent("a", State::Done)], NOW, false);
        for ms in [0, 100] {
            paint(&mut v, 120, 40, at(t0, ms), true);
        }
        let mut flashes = Vec::new();
        for i in 1..=10u64 {
            let mut r = agent("a", State::Done);
            r.tests = Some((i as u32, 0));
            v.set_rows(vec![r]);
            let now = at(t0, 200 + i * 300);
            paint(&mut v, 120, 40, now, true);
            if v.live.pulse.flashing(now) {
                flashes.push(i);
            }
        }
        // a poll every 300 ms raises the figure: without a pause the row would be lit at nearly every one
        assert!(flashes.len() <= 3, "the row flickers: lit at polls {flashes:?}");
        assert!(!flashes.is_empty(), "and it still flashes for a new run");
    }

    /// Every frame of a busy moment, drawn at sizes that change under the effects: nothing is written outside
    /// the window, and the footer is the one of the settled panel.
    #[test]
    fn no_effect_leaves_the_window_or_covers_the_footer_through_resizes() {
        let t0 = Instant::now();
        let mut base: Vec<AgentRow> = (0..8).map(|i| agent(&format!("s{i}"), State::Working)).collect();
        for r in &mut base {
            r.ribbon.clear();
        }
        let mut v = AgentsView::with_rows(base.clone(), NOW, false);
        for ms in [0, 3000] {
            paint(&mut v, 150, 44, at(t0, ms), true);
        }
        let mut busy = base.clone();
        for (i, r) in busy.iter_mut().enumerate() {
            let kinds = [
                EvKind::TestsPass,
                EvKind::TestsFail,
                EvKind::GitCommit,
                EvKind::GitPush,
                EvKind::Compact,
                EvKind::Error,
            ];
            r.ribbon =
                vec![Ev { at: NOW, kind: kinds[i % kinds.len()], n: 1, detail: nexo_agent::supervise::EvDetail::None }];
            r.subs = vec![(format!("s{i}.1"), State::Working), (format!("s{i}.2"), State::Done)];
        }
        v.set_rows(busy.clone());
        v.fly_new_mail(vec![mail("seed", "s0", "s1")], &busy);
        paint(&mut v, 150, 44, at(t0, 3100), true);
        let feed: Vec<MailRow> = std::iter::once(mail("seed", "s0", "s1"))
            .chain((1..7).map(|i| mail(&format!("m{i}"), "s7", &format!("s{i}"))))
            .collect();
        v.fly_new_mail(feed, &busy);
        let sizes = [(150u16, 44u16), (100, 30), (60, 20), (40, 12), (150, 44), (30, 6)];
        for (k, ms) in (0..160u64).map(|k| (k, 4000 + k * 16)).collect::<Vec<_>>() {
            let (w, h) = sizes[(k as usize / 4) % sizes.len()];
            let b = paint(&mut v, w, h, at(t0, ms), true);
            let win = v.window;
            if win.width == 0 {
                continue;
            }
            for y in 0..h {
                for x in 0..w {
                    let inside = x >= win.x && x < win.x + win.width && y >= win.y && y < win.y + win.height;
                    if !inside {
                        assert_eq!(b[(x, y)].symbol(), " ", "{w}x{h} at {ms} ms: a cell outside the window at {x},{y}");
                    }
                }
            }
            let mut rest = v.clone();
            let settled = paint(&mut rest, w, h, at(t0, 60_000), true);
            let fy = win.y + win.height.saturating_sub(2);
            assert_eq!(
                row_text(&b, fy, win.x + 2, win.x + win.width.saturating_sub(2)),
                row_text(&settled, fy, win.x + 2, win.x + win.width.saturating_sub(2)),
                "{w}x{h} at {ms} ms: the footer was covered"
            );
        }
    }

    #[test]
    fn effects_on_two_hundred_cards_stay_cheap() {
        let t0 = Instant::now();
        let rows: Vec<AgentRow> = (0..200)
            .map(|i| {
                let mut r = agent(&format!("s{i}"), if i % 3 == 0 { State::Working } else { State::WaitingPermission });
                r.subs = vec![(format!("s{i}.1"), State::Working), (format!("s{i}.2"), State::Done)];
                r
            })
            .collect();
        let mut v = AgentsView::with_rows(rows.clone(), NOW, false);
        for ms in [0, 3000] {
            paint(&mut v, 240, 70, at(t0, ms), true);
        }
        v.fly_new_mail(vec![mail("seed", "s0", "s1")], &rows);
        let feed: Vec<MailRow> = std::iter::once(mail("seed", "s0", "s1"))
            .chain((1..150).map(|i| mail(&format!("m{i}"), &format!("s{i}"), &format!("s{}", i + 1))))
            .collect();
        v.fly_new_mail(feed, &rows);
        let start = std::time::Instant::now();
        let frames = 30u64;
        for k in 0..frames {
            paint(&mut v, 240, 70, at(t0, 3100 + k * 33), true);
            let _ = v.animating(at(t0, 3100 + k * 33));
        }
        let per = start.elapsed() / frames as u32;
        // measured in a debug build on a shared machine: a generous bound that still catches a quadratic loop
        assert!(per < Duration::from_millis(250), "{per:?} a frame with 200 cards in motion");
    }
}
