//! Which lifetime the prompt cache is written for. The short one (5 minutes) is renewed by every request that
//! starts inside it, so a conversation that never pauses long pays nothing for more. A conversation that has
//! paused longer than that pays a full re-write of its prefix after each pause, and the 1-hour write costs less
//! extra than one such re-write (see `Pricing::long_cache_pays`). Only the session's own past pauses are used.

use std::time::{Duration, Instant};

use nexo_core::pricing::Pricing;

/// How long the short cache lasts, counted from the start of the request that wrote or read it.
const SHORT: Duration = Duration::from_secs(300);

/// When the requests of one session started, to tell whether it has shown a pause longer than the short cache lasts.
#[derive(Debug, Default)]
pub(super) struct Pauses {
    last_start: Option<Instant>,
    seen: bool,
}

impl Pauses {
    /// A request starts at `at`.
    pub(super) fn start(&mut self, at: Instant) {
        if self.last_start.is_some_and(|last| at.saturating_duration_since(last) > SHORT) {
            self.seen = true;
        }
        self.last_start = Some(at);
    }

    pub(super) fn seen(&self) -> bool {
        self.seen
    }
}

/// Whether the request is written for the long cache: a context over `over` tokens (0: never), a pause already
/// seen in this session, and prices that make the hour write pay.
pub(super) fn wants_long(pause_seen: bool, tokens: u64, over: u32, prices: Option<&Pricing>) -> bool {
    over > 0 && pause_seen && tokens > u64::from(over) && prices.is_some_and(Pricing::long_cache_pays)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_like() -> Pricing {
        Pricing { input: 2.0, output: 10.0, cache_read: 0.2, cache_write: 2.5, cache_write_1h: 0.0 }
    }

    #[test]
    fn a_pause_over_five_minutes_between_two_starts_is_seen() {
        let t0 = Instant::now();
        let mut p = Pauses::default();
        p.start(t0);
        p.start(t0 + Duration::from_secs(299));
        assert!(!p.seen(), "a start inside the short cache keeps it warm");
        p.start(t0 + Duration::from_secs(299 + 301));
        assert!(p.seen());
        p.start(t0 + Duration::from_secs(700));
        assert!(p.seen(), "once seen, it stays");
    }

    #[test]
    fn the_first_request_has_no_pause() {
        let mut p = Pauses::default();
        p.start(Instant::now());
        assert!(!p.seen());
    }

    #[test]
    fn a_large_context_after_a_pause_with_paying_prices_takes_the_long_cache() {
        let prices = claude_like();
        assert!(wants_long(true, 50_001, 50_000, Some(&prices)));
    }

    #[test]
    fn no_pause_seen_keeps_the_short_cache() {
        assert!(!wants_long(false, 400_000, 50_000, Some(&claude_like())));
    }

    #[test]
    fn a_context_not_over_the_size_keeps_the_short_cache() {
        let prices = claude_like();
        assert!(!wants_long(true, 50_000, 50_000, Some(&prices)));
        assert!(!wants_long(true, 3_000, 50_000, Some(&prices)));
    }

    #[test]
    fn prices_that_do_not_pay_keep_the_short_cache() {
        // the hour write at 4.9 costs 2.4 extra, a miss 2.3
        let dear = Pricing { cache_write_1h: 4.9, ..claude_like() };
        assert!(!wants_long(true, 200_000, 50_000, Some(&dear)));
        // a vendor that charges nothing for writing has no premium to save
        let free = Pricing { input: 1.0, output: 4.0, cache_read: 0.1, cache_write: 0.0, cache_write_1h: 0.0 };
        assert!(!wants_long(true, 200_000, 50_000, Some(&free)));
        assert!(!wants_long(true, 200_000, 50_000, None));
    }

    #[test]
    fn a_size_of_zero_turns_the_long_cache_off() {
        assert!(!wants_long(true, 200_000, 0, Some(&claude_like())));
    }
}
