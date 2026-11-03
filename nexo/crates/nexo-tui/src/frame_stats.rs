//! Per-frame timing statistics (F8).
//!
//! Pure arithmetic over samples supplied by the caller: the wall clock is
//! read only in `lib.rs`, which feeds microseconds in here and prints the
//! report on exit when `NEXO_FRAME_STATS` is set.

use std::fmt::Write as _;

/// Environment variable that switches the report on.
pub const ENV: &str = "NEXO_FRAME_STATS";

/// Collected frame samples, in microseconds.
#[derive(Debug, Clone, Default)]
pub struct FrameStats {
    draws: Vec<u64>,
    sections: Vec<(&'static str, u64)>,
}

/// Summary of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub draws: usize,
    pub median_us: u64,
    pub p99_us: u64,
    pub slowest: Option<(&'static str, u64)>,
}

impl FrameStats {
    /// Returns `true` when the report was requested through [`ENV`].
    pub fn enabled() -> bool {
        std::env::var_os(ENV).is_some_and(|v| !v.is_empty() && v != "0")
    }

    /// Records one full draw, in microseconds.
    pub fn record_draw(&mut self, micros: u64) {
        self.draws.push(micros);
    }

    /// Records the cost of one named section inside a draw; only the
    /// slowest one is reported.
    pub fn record_section(&mut self, name: &'static str, micros: u64) {
        match self.sections.iter_mut().find(|(n, _)| *n == name) {
            Some(slot) => slot.1 = slot.1.max(micros),
            None => self.sections.push((name, micros)),
        }
    }

    /// Number of draws recorded so far.
    pub fn draws(&self) -> usize {
        self.draws.len()
    }

    /// Computes the summary; `None` when nothing was drawn.
    pub fn summary(&self) -> Option<Summary> {
        if self.draws.is_empty() {
            return None;
        }
        let mut sorted = self.draws.clone();
        sorted.sort_unstable();
        Some(Summary {
            draws: sorted.len(),
            median_us: percentile(&sorted, 50),
            p99_us: percentile(&sorted, 99),
            slowest: self.sections.iter().copied().max_by_key(|(_, us)| *us),
        })
    }

    /// One-line report for stderr; empty when nothing was drawn.
    pub fn report(&self) -> String {
        let mut out = String::new();
        if let Some(s) = self.summary() {
            let _ = write!(
                out,
                "frames: {} draws, median {:.2} ms, p99 {:.2} ms",
                s.draws,
                s.median_us as f64 / 1000.0,
                s.p99_us as f64 / 1000.0
            );
            if let Some((name, us)) = s.slowest {
                let _ = write!(out, ", slowest section {} {:.2} ms", name, us as f64 / 1000.0);
            }
        }
        out
    }
}

/// Nearest-rank percentile over an ascending slice. `p` is 0..=100.
/// Returns 0 for an empty slice.
pub fn percentile(sorted: &[u64], p: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let p = u64::from(p.min(100));
    let n = sorted.len() as u64;
    // ceil(p/100 * n), at least 1
    let rank = (p * n).div_ceil(100).max(1);
    sorted[(rank - 1) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_uses_nearest_rank() {
        let s: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&s, 50), 50);
        assert_eq!(percentile(&s, 99), 99);
        assert_eq!(percentile(&s, 100), 100);
        assert_eq!(percentile(&s, 0), 1);
        assert_eq!(percentile(&[7], 99), 7);
        assert_eq!(percentile(&[], 50), 0);
    }

    #[test]
    fn summary_from_fixed_samples() {
        let mut st = FrameStats::default();
        for us in [3000, 1000, 2000, 4000, 50_000] {
            st.record_draw(us);
        }
        st.record_section("blocks", 900);
        st.record_section("status", 1200);
        st.record_section("blocks", 1500);
        let s = st.summary().expect("has draws");
        assert_eq!(s.draws, 5);
        assert_eq!(s.median_us, 3000);
        assert_eq!(s.p99_us, 50_000);
        assert_eq!(s.slowest, Some(("blocks", 1500)));
        assert_eq!(st.report(), "frames: 5 draws, median 3.00 ms, p99 50.00 ms, slowest section blocks 1.50 ms");
    }

    #[test]
    fn empty_stats_report_nothing() {
        let st = FrameStats::default();
        assert_eq!(st.summary(), None);
        assert_eq!(st.report(), "");
        assert_eq!(st.draws(), 0);
    }
}
