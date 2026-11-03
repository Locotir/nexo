//! Small pieces of motion: a value that eases towards a target over a few
//! frames, and a pulse that beats a few times and settles. Both are pure
//! functions of the time they are asked about, so the tick only has to ask
//! whether something still moves to know whether to draw again.

use std::time::Instant;

/// How long the transcript takes to glide to where a scroll sends it.
pub const SCROLL_MS: u64 = 180;
/// How long the thumb and the "new" pill beat when something arrives out of sight.
pub const PULSE_MS: u64 = 1200;
/// One beat of a pulse.
const BEAT_MS: f32 = 400.0;
/// How long a palette or a dialog takes to open.
pub const POP_MS: u64 = 140;

/// Cubic ease-out: fast at first, settling softly. `t` is clamped to 0..1.
pub fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// 0..1 of the way through `ms` since `at`; 1 when there is no start.
pub fn progress(at: Option<Instant>, now: Instant, ms: u64) -> f64 {
    match at {
        Some(at) if ms > 0 => (now.saturating_duration_since(at).as_millis() as f64 / ms as f64).min(1.0),
        _ => 1.0,
    }
}

/// A beat that rises and falls (0..1..0) every `BEAT_MS`, for `PULSE_MS`
/// after `at`; 0 before and after.
pub fn pulse(at: Option<Instant>, now: Instant) -> f32 {
    let Some(at) = at else { return 0.0 };
    let ms = now.saturating_duration_since(at).as_millis() as f32;
    if ms >= PULSE_MS as f32 {
        return 0.0;
    }
    let phase = ms / BEAT_MS * std::f32::consts::TAU;
    0.5 - 0.5 * phase.cos()
}

/// A number that glides to its target: a new target starts from wherever the
/// glide is now, so a burst of wheel steps reads as one smooth motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tween {
    from: f64,
    to: f64,
    start: Option<Instant>,
    ms: u64,
}

impl Tween {
    pub fn new(value: f64, ms: u64) -> Tween {
        Tween { from: value, to: value, start: None, ms }
    }

    /// Where the glide is at `now`.
    pub fn value_at(&self, now: Instant) -> f64 {
        match self.start {
            None => self.to,
            Some(_) => {
                let t = ease_out(progress(self.start, now, self.ms));
                self.from + (self.to - self.from) * t
            }
        }
    }

    pub fn target(&self) -> f64 {
        self.to
    }

    /// Glide to `to` from wherever the value is at `now`.
    pub fn retarget(&mut self, to: f64, now: Instant) {
        if to == self.to {
            return;
        }
        self.from = self.value_at(now);
        self.to = to;
        self.start = Some(now);
    }

    /// Be at `value` at once.
    pub fn snap(&mut self, value: f64) {
        self.from = value;
        self.to = value;
        self.start = None;
    }

    /// Still on its way at `now` (without forgetting a finished start).
    pub fn is_moving(&self, now: Instant) -> bool {
        self.start.is_some() && progress(self.start, now, self.ms) < 1.0
    }

    /// Still on its way at `now`; a finished glide forgets its start.
    pub fn moving(&mut self, now: Instant) -> bool {
        if self.start.is_some() && progress(self.start, now, self.ms) >= 1.0 {
            self.snap(self.to);
        }
        self.start.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn ease_out_starts_fast_ends_at_one_and_clamps() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(-3.0), 0.0);
        assert_eq!(ease_out(7.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "more than half the way at half the time");
        let mut last = 0.0;
        for i in 0..=20 {
            let v = ease_out(i as f64 / 20.0);
            assert!(v >= last);
            last = v;
        }
    }

    #[test]
    fn a_tween_glides_to_its_target_and_stops() {
        let t0 = Instant::now();
        let mut tw = Tween::new(0.0, 100);
        assert!(!tw.moving(t0));
        tw.retarget(100.0, t0);
        assert_eq!(tw.value_at(t0), 0.0);
        let mid = tw.value_at(t0 + Duration::from_millis(50));
        assert!(mid > 50.0 && mid < 100.0, "eases out: {mid}");
        assert!(tw.moving(t0 + Duration::from_millis(50)));
        assert_eq!(tw.value_at(t0 + Duration::from_millis(100)), 100.0);
        assert!(!tw.moving(t0 + Duration::from_millis(150)));
        assert_eq!(tw.value_at(t0 + Duration::from_millis(150)), 100.0);
    }

    #[test]
    fn a_new_target_starts_from_where_the_glide_is() {
        let t0 = Instant::now();
        let mut tw = Tween::new(0.0, 100);
        tw.retarget(100.0, t0);
        let t1 = t0 + Duration::from_millis(30);
        let here = tw.value_at(t1);
        tw.retarget(0.0, t1);
        assert!((tw.value_at(t1) - here).abs() < 1e-9, "no jump when the target changes");
        assert_eq!(tw.value_at(t1 + Duration::from_millis(100)), 0.0);
        // the same target again changes nothing
        let before = tw;
        tw.retarget(0.0, t1 + Duration::from_millis(10));
        assert_eq!(tw, before);
    }

    #[test]
    fn a_snap_lands_at_once() {
        let t0 = Instant::now();
        let mut tw = Tween::new(5.0, 100);
        tw.retarget(50.0, t0);
        tw.snap(20.0);
        assert_eq!(tw.value_at(t0), 20.0);
        assert!(!tw.moving(t0));
        assert_eq!(tw.target(), 20.0);
    }

    #[test]
    fn a_pulse_beats_then_settles() {
        let t0 = Instant::now();
        assert_eq!(pulse(None, t0), 0.0);
        assert!(pulse(Some(t0), t0) < 0.01, "starts dark");
        let peak = pulse(Some(t0), t0 + Duration::from_millis((BEAT_MS / 2.0) as u64));
        assert!(peak > 0.99, "lit half a beat in: {peak}");
        assert_eq!(pulse(Some(t0), t0 + Duration::from_millis(PULSE_MS)), 0.0);
        // a start in the future (a clock that went back) is just dark
        assert!(pulse(Some(t0 + Duration::from_secs(1)), t0) < 0.01);
    }

    #[test]
    fn progress_is_bounded() {
        let t0 = Instant::now();
        assert_eq!(progress(None, t0, 100), 1.0);
        assert_eq!(progress(Some(t0), t0, 0), 1.0);
        assert_eq!(progress(Some(t0), t0 + Duration::from_millis(50), 100), 0.5);
        assert_eq!(progress(Some(t0), t0 + Duration::from_secs(9), 100), 1.0);
    }
}
