//! The motion state of the app (see `motion`): counters, the end-of-turn glow, a clock that tests can set.

use std::time::Instant;

use crate::app::App;
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
    pub chg_key: [Option<u64>; 3],
    pub chg_at: [Option<Instant>; 3],
    /// When the locked home opened: the welcome card slides away.
    pub unlock_at: Option<Instant>,
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
        ];
        for (i, k) in keys.into_iter().enumerate() {
            if self.fx.chg_key[i].is_some_and(|old| old != k) && self.fx_on() {
                self.fx.chg_at[i] = Some(now);
            }
            self.fx.chg_key[i] = Some(k);
        }
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
