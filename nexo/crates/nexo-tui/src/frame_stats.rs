//! Optional frame timings. Percentiles describe the last 4096 draws.

use std::fmt::Write as _;
use std::io::Write as _;
use std::time::Instant;

const MAX_SAMPLES: usize = 4096;

/// Environment variable that switches the report on.
pub const ENV: &str = "NEXO_FRAME_STATS";

/// Collected frame samples, in microseconds.
#[derive(Debug, Clone, Default)]
pub struct FrameStats {
    draws: Vec<u64>,
    next: usize,
    total: usize,
    slowest: Option<(&'static str, u64)>,
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
        self.total = self.total.saturating_add(1);
        if self.draws.len() < MAX_SAMPLES {
            self.draws.push(micros);
        } else {
            self.draws[self.next] = micros;
        }
        self.next = (self.next + 1) % MAX_SAMPLES;
    }

    /// Records the cost of one named section inside a draw; only the
    /// slowest one is reported.
    pub fn record_section(&mut self, name: &'static str, micros: u64) {
        if self.slowest.is_none_or(|(_, old)| micros > old) {
            self.slowest = Some((name, micros));
        }
    }

    /// Number of draws recorded so far.
    pub fn draws(&self) -> usize {
        self.total
    }

    /// Computes the summary; `None` when nothing was drawn.
    pub fn summary(&self) -> Option<Summary> {
        if self.draws.is_empty() {
            return None;
        }
        let mut sorted = self.draws.clone();
        sorted.sort_unstable();
        Some(Summary {
            draws: self.total,
            median_us: percentile(&sorted, 50),
            p99_us: percentile(&sorted, 99),
            slowest: self.slowest,
        })
    }

    /// One-line report for stderr; empty when nothing was drawn.
    pub fn report(&self) -> String {
        let mut out = String::new();
        if let Some(s) = self.summary() {
            let _ = write!(
                out,
                "frames: {} draws (last {} sampled), median {:.2} ms, p99 {:.2} ms",
                s.draws,
                self.draws.len(),
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

/// Declared before the terminal, so its report follows terminal restoration.
pub(crate) struct FrameReport(pub Option<FrameStats>);

impl FrameReport {
    pub fn new(enabled: bool) -> Self {
        Self(enabled.then(FrameStats::default))
    }

    pub fn draw<T, E>(&mut self, draw: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        let Some(stats) = &mut self.0 else { return draw() };
        let start = Instant::now();
        let result = draw();
        if result.is_ok() {
            stats.record_draw(start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
        }
        result
    }
}

impl Drop for FrameReport {
    fn drop(&mut self) {
        if let Some(stats) = &self.0 {
            let report = stats.report();
            if !report.is_empty() {
                let _ = writeln!(std::io::stderr().lock(), "{report}");
            }
        }
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
        assert_eq!(
            st.report(),
            "frames: 5 draws (last 5 sampled), median 3.00 ms, p99 50.00 ms, slowest section blocks 1.50 ms"
        );
    }

    #[test]
    fn samples_are_bounded_and_recent() {
        let mut st = FrameStats::default();
        for i in 0..MAX_SAMPLES + 10 {
            st.record_draw(i as u64);
        }
        assert_eq!(st.draws(), MAX_SAMPLES + 10);
        assert_eq!(st.draws.len(), MAX_SAMPLES);
        assert_eq!(st.draws.iter().min(), Some(&10));
    }

    #[test]
    fn optional_draw_records_only_successes() {
        let mut disabled = FrameReport::new(false);
        assert_eq!(disabled.draw(|| Ok::<_, ()>(7)), Ok(7));
        assert!(disabled.0.is_none());
        let mut enabled = FrameReport::new(true);
        assert_eq!(enabled.draw(|| Err::<(), _>("error")), Err("error"));
        assert_eq!(enabled.0.as_ref().unwrap().draws(), 0);
        assert_eq!(enabled.draw(|| Ok::<_, ()>(7)), Ok(7));
        assert_eq!(enabled.0.as_ref().unwrap().draws(), 1);
    }

    #[test]
    fn empty_stats_report_nothing() {
        let st = FrameStats::default();
        assert_eq!(st.summary(), None);
        assert_eq!(st.report(), "");
        assert_eq!(st.draws(), 0);
    }
}
