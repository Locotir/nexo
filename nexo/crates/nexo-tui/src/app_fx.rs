//! The motion state of the app (see `motion`): counters, the end-of-turn glow, a clock that tests can set.

use std::time::Instant;

use crate::app::App;
use crate::fx_chrome;
use crate::motion::{self, Counter};
use crate::toast::Kind;

#[derive(Debug, Default, Clone)]
pub struct Fx {
    /// Tests set this; otherwise the wall clock.
    pub clock: Option<Instant>,
    /// Tests set the `HH:MM` that new messages are stamped with; otherwise the local time.
    pub wall: Option<String>,
    pub cost: Counter,
    pub tok: Counter,
    /// When the last turn ended: the rule glows and the figures pulse once.
    pub turn_done: Option<Instant>,
    /// The curator just cut the view: the tokens it let go and when (shown by the gauge, fading).
    pub cut: Option<(u64, Instant)>,
    /// When the permission mode last changed, and the mode before: the rule over the draft wipes
    /// from the old colour to the new one.
    pub mode_at: Option<Instant>,
    pub mode_from: Option<nexo_core::permission::PermissionMode>,
    /// The slash command the draft starts with, once it names a real one, and since when: it
    /// lights up once.
    pub cmd_lit: Option<(String, Instant)>,
    /// The highlight of the palette list, of a picker list and of the selected block glides.
    pub pal_sel: motion::Glide,
    pub pick_sel: motion::Glide,
    pub block_sel: motion::Glide,
    /// The keys row: a hash of its text and since when it has been that, so a change fades in.
    pub hint_key: u64,
    pub hint_at: Option<Instant>,
    /// Search opened or closed, and when: the label fades in, the rule sweeps back.
    pub search_on: bool,
    pub search_at: Option<Instant>,
    /// The agent started working: the activity line fades in from the plain rule.
    pub working: bool,
    pub turn_start: Option<Instant>,
    /// Queued messages and when each was first drawn: a new row fades in.
    pub queue_seen: Vec<(String, Instant)>,
    /// A dialog or the palette on screen last frame (its cells) and since when it closed: the
    /// closing draws the reverse of the pop.
    pub ghost: Option<(ratatui::layout::Rect, Vec<ratatui::buffer::Cell>)>,
    pub ghost_at: Option<Instant>,
    /// The model, the reasoning level and the permission mode in the status bar: what each was at the last
    /// tick (hashed) and when it last changed, so a new one shows in the curator label's highlight.
    /// Index 3 is the git branch and 4 the folder.
    pub chg_key: [Option<u64>; 5],
    pub chg_at: [Option<Instant>; 5],
    /// When the locked home opened: the welcome card slides away.
    pub unlock_at: Option<Instant>,
    /// The diff side panel: its open fade and its changed files (see `fx_diff`).
    pub diff: crate::fx_diff::PanelFx,
    /// The welcome card has been drawn once this start, and when its lines began to come in.
    pub welcome_seen: bool,
    pub welcome_at: Option<Instant>,
    /// The context gauge easing (fill, parts, colour band).
    pub gauge: crate::fx_chrome::GaugeFx,
    /// The transcript offset last drawn and since when the view last scrolled: the thumb is lit.
    pub bar_off: Option<Option<usize>>,
    pub bar_lit: Option<Instant>,
    /// The chips of the draft (folded pastes, pasted attachments) and when each was first drawn.
    pub chip_seen: Vec<(String, Instant)>,
    /// When a draft was last rejected (a command that does not exist): the rule flashes.
    pub reject_at: Option<Instant>,
    /// The goal and to-do views: the panel opening, the bar, the marks and the rows (see `fx_goal`).
    pub goal: crate::fx_goal::GoalFx,
    /// The constellation above the welcome card (see `fx_stars`).
    pub stars: crate::fx_stars::StarsFx,
    /// The letters of the reply that arrived lately, drawn dimmer while they settle (see `fx_stream`).
    pub ink: crate::fx_stream::Ink,
}

impl App {
    /// The time every effect is drawn at.
    pub fn now(&self) -> Instant {
        self.fx.clock.unwrap_or_else(Instant::now)
    }

    /// The `HH:MM` a message created now is stamped with.
    pub fn stamp_now(&self) -> String {
        self.fx.wall.clone().unwrap_or_else(crate::app::clock)
    }

    /// A small confirmation at the top right.
    pub fn toast(&mut self, kind: Kind, text: &str) {
        let now = self.now();
        self.toasts.push(kind, text, now);
        self.dirty = true;
    }

    /// A confirmation: one channel only, the toast (the bar is left alone).
    pub fn flash_toast(&mut self, text: &str) {
        self.toast(Kind::Success, text);
    }

    /// Advances the rolling figures, the toasts and the turn-end glow; true while anything moves.
    pub fn tick_fx(&mut self) -> bool {
        self.tick_fx_with(true)
    }

    /// `tick_fx`; without `stars` the turn of the welcome figure is left out (the loop gives it the beat, not a frame rate).
    pub(crate) fn tick_fx_with(&mut self, stars: bool) -> bool {
        let now = self.now();
        let on = self.opts.animations;
        let tokens = (self.session_usage.total_input() + self.session_usage.output) as f64;
        let (t, t_moving) = self.fx.tok.sample(tokens, now, on);
        self.tok_shown = t.round() as u64;
        let (c, c_moving) = self.fx.cost.sample(self.cost, now, on);
        self.cost_shown = c;
        self.toasts.prune(now);
        let glow = on && motion::within(self.fx.turn_done, now, motion::GLOW_MS);
        let mode = on && motion::within(self.fx.mode_at, now, motion::MODE_MS);
        let cmd = on && motion::within(self.fx.cmd_lit.as_ref().map(|c| c.1), now, motion::LIT_MS);
        // the cut figure goes away by itself, animations or not
        let cut = motion::within(self.fx.cut.map(|c| c.1), now, motion::CUT_MS + 100);
        let changed = self.note_bar_changes(now);
        self.note_goal_fx(now);
        let fx = &self.fx;
        let still = on
            && !self.theme.mono
            && (fx.pal_sel.moving(now)
                || fx.pick_sel.moving(now)
                || fx.block_sel.moving(now)
                || motion::within(fx.hint_at, now, motion::GLIDE_MS)
                || motion::within(fx.search_at, now, motion::GLOW_MS)
                || motion::within(fx.turn_start, now, motion::MODE_MS)
                || motion::within(fx.ghost_at, now, crate::anim::POP_MS)
                || motion::within(fx.unlock_at, now, crate::ui::UNLOCK_MS)
                || fx.diff.moving(now)
                || self.diffview.as_ref().is_some_and(|d| d.fx.moving(now))
                || fx.gauge.moving(now)
                || self.stars_moving(now, stars)
                || self.welcome_at(now).is_some()
                || motion::within(fx.bar_lit, now, fx_chrome::BAR_MS)
                || motion::within(fx.reject_at, now, fx_chrome::REJECT_MS)
                || fx.chip_seen.iter().any(|(_, t)| motion::within(Some(*t), now, fx_chrome::CHIP_MS))
                || fx.goal.alive(now)
                || fx.ink.alive(now)
                || fx.queue_seen.iter().any(|(_, t)| motion::within(Some(*t), now, motion::LIT_MS)));
        t_moving || c_moving || glow || mode || cmd || cut || changed || still || self.toasts.alive(now)
    }

    /// Notices a new model, reasoning level or permission mode (the first look is not a change); true while
    /// the highlight of one that just changed is still showing.
    fn note_bar_changes(&mut self, now: Instant) -> bool {
        use std::hash::{Hash, Hasher};
        let hash = |s: &str| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            s.hash(&mut h);
            h.finish()
        };
        let keys = [
            hash(&format!("{}/{}{}", self.provider, self.model, self.fast)),
            hash(self.effort.as_deref().unwrap_or("")),
            hash(self.permission_mode.as_str()),
            hash(self.branch.as_deref().unwrap_or("")),
            hash(&self.cwd.to_string_lossy()),
        ];
        let no_branch = hash("");
        for (i, k) in keys.into_iter().enumerate() {
            // a branch found after the start is not a change
            let found = i == 3 && self.fx.chg_key[i] == Some(no_branch);
            if self.fx.chg_key[i].is_some_and(|old| old != k) && !found && self.fx_on() {
                self.fx.chg_at[i] = Some(now);
            }
            self.fx.chg_key[i] = Some(k);
        }
        self.note_gauge(now);
        self.note_chips(now);
        self.note_bar(now);
        self.note_welcome(now);
        self.fx.chg_at.iter().any(|t| motion::within(*t, now, motion::CUT_MS + 100))
    }

    /// Called when a turn ends.
    pub fn turn_glow(&mut self) {
        if self.opts.animations {
            self.fx.turn_done = Some(self.now());
        }
    }
}

impl App {
    /// Whether colour motion is on at all (off with `animations = false` and on a mono theme).
    pub fn fx_on(&self) -> bool {
        self.opts.animations && !self.theme.mono
    }

    /// Keeps the cells of `r` while a dialog or the palette is up, to draw it leaving.
    pub fn ghost_save(&mut self, buf: &ratatui::buffer::Buffer, r: ratatui::layout::Rect) {
        if !self.fx_on() {
            return;
        }
        let r = r.intersection(buf.area);
        let mut cells = Vec::with_capacity(r.width as usize * r.height as usize);
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                cells.push(buf[(x, y)].clone());
            }
        }
        self.fx.ghost = Some((r, cells));
        self.fx.ghost_at = None;
    }

    /// The reverse of the pop: the closed dialog's cells melt into what is under it.
    pub fn ghost_draw(&mut self, buf: &mut ratatui::buffer::Buffer) {
        let Some(at) = self.fx.ghost_at else { return };
        let now = self.now();
        let t = crate::anim::progress(Some(at), now, crate::anim::POP_MS);
        if !self.fx_on() || t >= 1.0 {
            self.fx.ghost = None;
            self.fx.ghost_at = None;
            return;
        }
        let k = crate::anim::ease_out(t) as f32;
        let Some((r, cells)) = &self.fx.ghost else { return };
        if r.intersection(buf.area) != *r {
            return;
        }
        let mut it = cells.iter();
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                let Some(g) = it.next() else { return };
                let live = &mut buf[(x, y)];
                if live.symbol() == g.symbol() && live.fg == g.fg && live.bg == g.bg {
                    continue;
                }
                let (fg, bg) =
                    (motion::fade(&self.theme, g.fg, live.fg, k), motion::fade(&self.theme, g.bg, live.bg, k));
                if k < 0.5 {
                    live.set_symbol(g.symbol());
                }
                live.fg = fg;
                live.bg = bg;
            }
        }
    }
}

impl App {
    /// Tells the gauge's easing what the context is now (the draw and the tick both do, once is enough).
    pub(crate) fn note_gauge(&mut self, now: Instant) {
        let (num, den, guessed) = self.gauge_view();
        let frac = (num as f64 / den.max(1) as f64).min(1.0);
        let colour = self.theme.level_color((frac * 100.0).round() as u64);
        let parts = self.gauge_split().filter(|_| !guessed).map(|s| [s.cached, s.fresh, s.out]);
        let on = self.fx_on();
        self.fx.gauge.note(&self.theme, parts, frac, colour, now, on);
    }

    /// A chip new in the draft is lit from now (the tick sees it before the draw does, so the loop wakes for it).
    pub(crate) fn note_chips(&mut self, now: Instant) {
        if !self.fx_on() {
            return;
        }
        let labels = self.input.chip_labels();
        self.fx.chip_seen.retain(|(l, _)| labels.contains(l));
        for l in labels {
            if !self.fx.chip_seen.iter().any(|(s, _)| *s == l) {
                self.fx.chip_seen.push((l, now));
            }
        }
    }

    /// The view was scrolled (by hand: following the end is not a scroll): the thumb is lit from now.
    pub(crate) fn note_bar(&mut self, now: Instant) {
        if !self.fx_on() {
            return;
        }
        let key = (!self.follow).then_some(self.scroll);
        if self.fx.bar_off.is_some_and(|o| o != key) {
            self.fx.bar_lit = Some(now);
        }
        self.fx.bar_off = Some(key);
    }

    /// The welcome card comes in once per start.
    pub(crate) fn note_welcome(&mut self, now: Instant) {
        if self.locked && !self.fx.welcome_seen {
            self.fx.welcome_seen = true;
            self.fx.stars.begin(now);
            self.fx.welcome_at = self.fx_on().then_some(now);
        }
    }

    /// When the welcome card's lines began to come in, while they still do. A key typed ends it.
    pub(crate) fn welcome_at(&self, now: Instant) -> Option<Instant> {
        let at = self.fx.welcome_at?;
        let live = self.fx_on()
            && self.locked
            && self.input.is_empty()
            && self.modal.is_none()
            && motion::within(Some(at), now, fx_chrome::WELCOME_MS);
        live.then_some(at)
    }
}
