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
        t_moving || c_moving || glow || self.toasts.alive(now)
    }

    /// Called when a turn ends.
    pub fn turn_glow(&mut self) {
        if self.opts.animations {
            self.fx.turn_done = Some(self.now());
        }
    }
}
