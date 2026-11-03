//! The frame scheduler: when to tick, when to draw and how long to sleep.
//! Pure functions of an injected clock, so the run loop only has to obey it.
//! Nothing changed: no wakeup but the slow tick. Something moves: frames at ~60 fps.
//! A burst of changes (streaming deltas) is drawn as one frame.

use std::io::Stdout;
use std::time::{Duration, Instant};

use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// The shortest time between two frames (~60 fps).
pub const FRAME: Duration = Duration::from_millis(16);
/// The spinner beat: one step of `App::spinner`.
pub const BEAT: Duration = Duration::from_millis(50);
/// What an idle screen still checks now and then (the git branch, a status command).
pub const SLOW: Duration = Duration::from_secs(5);
/// Busy with `[ui] animations=false`: nothing moves, only the elapsed clock needs a redraw.
pub const STILL: Duration = Duration::from_secs(1);

/// What the screen needs from the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    /// Nothing moves.
    Idle,
    /// A spinner or a toast: one step per beat.
    Beat,
    /// A glide, a fade or a pulse: a frame every ~16 ms.
    Motion,
}

/// What the next pass of the loop must do about the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    /// Call `App::tick`.
    pub run: bool,
    /// ... and this tick steps the spinner.
    pub beat: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Pacer {
    last_draw: Option<Instant>,
    last_tick: Instant,
    last_beat: Instant,
    /// `[ui] animations`: off means no frame faster than the beat.
    animations: bool,
    /// Something happened while the clock rested: the next tick comes within a frame, so a
    /// window or a fade that the event started moves at once instead of after the slow tick.
    kicked: bool,
}

impl Pacer {
    pub fn new(now: Instant, animations: bool) -> Pacer {
        Pacer { last_draw: None, last_tick: now, last_beat: now, animations, kicked: false }
    }

    /// An event came while resting: the next tick is due one frame after the last.
    pub fn kick(&mut self) {
        self.kicked = true;
    }

    /// How often the tick runs for this load (the slow tick when idle).
    fn cadence(&self, load: Load) -> Duration {
        if self.kicked {
            return FRAME;
        }
        match (load, self.animations) {
            (Load::Idle, _) => SLOW,
            (Load::Beat, true) => BEAT,
            (Load::Motion, true) => FRAME,
            (Load::Beat | Load::Motion, false) => STILL,
        }
    }

    /// Whether the tick is due now; marks it as run.
    pub fn tick(&mut self, now: Instant, load: Load) -> Tick {
        if now.saturating_duration_since(self.last_tick) < self.cadence(load) {
            return Tick { run: false, beat: false };
        }
        self.last_tick = now;
        self.kicked = false;
        // With animations off the spinner never steps: the tick only refreshes the clock.
        let beat = self.animations && now.saturating_duration_since(self.last_beat) >= BEAT;
        if beat {
            self.last_beat = now;
        }
        Tick { run: true, beat }
    }

    /// Whether to draw now (something changed and the last frame is old enough); marks it.
    pub fn draw(&mut self, now: Instant, dirty: bool) -> bool {
        if !dirty || self.last_draw.is_some_and(|t| now.saturating_duration_since(t) < FRAME) {
            return false;
        }
        self.last_draw = Some(now);
        true
    }

    /// How long to sleep before the next thing to do; `None`: nothing is timed, wait for input.
    /// `deadline` is a timed change of the loop itself (a paste that ends).
    pub fn sleep(&self, now: Instant, load: Load, dirty: bool, deadline: Option<Instant>) -> Option<Duration> {
        let mut at: Option<Instant> = deadline;
        let mut soon = |t: Instant| at = Some(at.map_or(t, |a| a.min(t)));
        if dirty {
            soon(self.last_draw.map_or(now, |t| t + FRAME));
        }
        soon(self.last_tick + self.cadence(load));
        at.map(|t| t.saturating_duration_since(now))
    }
}

/// Draws one frame of the real screen inside a synchronized update (DEC 2026), so a terminal
/// that knows it never shows half a frame; the others ignore the sequences.
pub fn draw_synced(term: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut crate::App) -> anyhow::Result<()> {
    crossterm::queue!(term.backend_mut(), BeginSynchronizedUpdate)?;
    let drawn = crate::draw_screen(term, app);
    // the end is written even when the frame failed, so the terminal is never left waiting
    crossterm::execute!(term.backend_mut(), EndSynchronizedUpdate)?;
    drawn
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn an_event_at_rest_gets_its_tick_within_a_frame() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, true);
        // resting for a second, then a key opens a window that moves
        assert!(!p.tick(t0 + ms(1000), Load::Idle).run);
        p.kick();
        assert!(p.tick(t0 + ms(1000), Load::Idle).run, "the tick comes at once, not after the slow tick");
        // the kick is spent: resting again waits for the slow tick
        assert_eq!(p.sleep(t0 + ms(1000), Load::Idle, false, None), Some(SLOW));
        // a kick right after a tick waits one frame, not five seconds
        p.kick();
        assert!(!p.tick(t0 + ms(1005), Load::Idle).run);
        assert_eq!(p.sleep(t0 + ms(1005), Load::Idle, false, None), Some(FRAME - ms(5)));
    }

    #[test]
    fn idle_sleeps_until_the_slow_tick_only() {
        let t0 = Instant::now();
        let p = Pacer::new(t0, true);
        assert_eq!(p.sleep(t0, Load::Idle, false, None), Some(SLOW));
        assert_eq!(p.sleep(t0 + ms(1000), Load::Idle, false, None), Some(SLOW - ms(1000)));
    }

    #[test]
    fn idle_never_ticks_or_draws_between_slow_ticks() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, true);
        for i in 1..100 {
            let now = t0 + ms(i * 40);
            if now < t0 + SLOW {
                assert!(!p.tick(now, Load::Idle).run);
                assert!(!p.draw(now, false));
            }
        }
        assert!(p.tick(t0 + SLOW, Load::Idle).run);
    }

    #[test]
    fn motion_runs_at_sixteen_ms() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, true);
        assert_eq!(p.sleep(t0, Load::Motion, false, None), Some(FRAME));
        let mut ticks = 0;
        for i in 1..=100 {
            if p.tick(t0 + ms(i), Load::Motion).run {
                ticks += 1;
            }
        }
        assert_eq!(ticks, 100 / 16);
    }

    #[test]
    fn motion_with_animations_off_is_only_the_clock() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, false);
        assert_eq!(p.sleep(t0, Load::Motion, false, None), Some(STILL));
        assert!(!p.tick(t0 + ms(20), Load::Motion).run);
        assert!(!p.tick(t0 + ms(50), Load::Motion).run);
        assert!(!p.tick(t0 + ms(999), Load::Motion).run);
        let t = p.tick(t0 + ms(1000), Load::Motion);
        assert!(t.run && !t.beat);
    }

    #[test]
    fn busy_with_animations_off_wakes_once_a_second_and_never_spins() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, false);
        assert_eq!(p.sleep(t0, Load::Beat, false, None), Some(STILL));
        let ticks: Vec<Tick> = (1..=3000).map(|i| p.tick(t0 + ms(i), Load::Beat)).collect();
        assert_eq!(ticks.iter().filter(|t| t.run).count(), 3);
        assert!(ticks.iter().all(|t| !t.beat));
    }

    #[test]
    fn busy_with_animations_on_keeps_the_beat() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, true);
        assert_eq!(p.sleep(t0, Load::Beat, false, None), Some(BEAT));
        assert!(p.tick(t0 + ms(50), Load::Beat).beat);
    }

    #[test]
    fn the_spinner_steps_at_most_once_per_beat() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, true);
        let beats = (1..=200).filter(|i| p.tick(t0 + ms(*i), Load::Motion).beat).count();
        assert!((3..=4).contains(&beats), "{beats}");
    }

    #[test]
    fn a_burst_of_changes_is_one_frame() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, true);
        assert!(p.draw(t0, true));
        for i in 1..=10 {
            assert!(!p.draw(t0 + ms(i), true));
        }
        assert_eq!(p.sleep(t0 + ms(10), Load::Idle, true, None), Some(FRAME - ms(10)));
        assert!(p.draw(t0 + ms(16), true));
        assert!(!p.draw(t0 + ms(17), false));
    }

    #[test]
    fn the_first_change_is_drawn_at_once() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0, true);
        assert_eq!(p.sleep(t0, Load::Idle, true, None), Some(Duration::ZERO));
        assert!(p.draw(t0, true));
    }

    #[test]
    fn a_deadline_wakes_the_loop_early() {
        let t0 = Instant::now();
        let p = Pacer::new(t0, true);
        assert_eq!(p.sleep(t0, Load::Idle, false, Some(t0 + ms(300))), Some(ms(300)));
    }
}
