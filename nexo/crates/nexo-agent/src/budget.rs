//! Spend limits: one per session (`[agent] max_cost`) and one per day across every
//! session (`[agent] max_cost_day`), in dollars. What was spent each day is kept in
//! `spend.json` in the data directory. Before each model call the agent asks
//! [`verdict`]: past 80% it warns once, at the limit it stops the request before the
//! call is made, so a limit is never passed by more than the one call that crossed it.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Days of spending kept in the file.
const KEEP_DAYS: usize = 62;

fn file() -> PathBuf {
    nexo_core::paths::data_dir().join("spend.json")
}

/// Today, as the file keys it (`2026-10-28`, local time where known).
pub fn today() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    format!("{:04}-{:02}-{:02}", now.year(), u8::from(now.month()), now.day())
}

fn load() -> BTreeMap<String, f64> {
    std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Dollars spent today, every session together.
pub fn spent_today() -> f64 {
    load().get(&today()).copied().unwrap_or(0.0)
}

/// Adds `cost` to today's spending; returns the new total.
pub fn add(cost: f64) -> f64 {
    let mut days = load();
    let total = {
        let t = days.entry(today()).or_insert(0.0);
        *t += cost.max(0.0);
        *t
    };
    while days.len() > KEEP_DAYS {
        let first = days.keys().next().cloned();
        if let Some(k) = first {
            days.remove(&k);
        }
    }
    let f = file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(text) = serde_json::to_string_pretty(&days) {
        let tmp = f.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &f);
        }
    }
    total
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Go,
    /// Past 80% of a limit (said once).
    Warn(String),
    /// At or past a limit: no more model calls.
    Stop(String),
}

/// What the limits say of the spending so far.
pub fn verdict(session: f64, day: f64, session_limit: Option<f64>, day_limit: Option<f64>) -> Verdict {
    let checks = [("this session", session, session_limit, "max_cost"), ("today", day, day_limit, "max_cost_day")];
    for (what, spent, limit, key) in checks {
        if let Some(l) = limit.filter(|l| *l > 0.0) {
            if spent >= l {
                return Verdict::Stop(format!(
                    "spend limit reached: ${spent:.4} of ${l:.2} {what}; no more model calls. `/budget <dollars>` raises it for this session ([agent] {key} in the configuration)"
                ));
            }
        }
    }
    for (what, spent, limit, _) in checks {
        if let Some(l) = limit.filter(|l| *l > 0.0) {
            if spent >= 0.8 * l {
                return Verdict::Warn(format!(
                    "${spent:.4} of the ${l:.2} limit spent {what} ({:.0}%)",
                    100.0 * spent / l
                ));
            }
        }
    }
    Verdict::Go
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_near_and_at_the_limits() {
        assert_eq!(verdict(0.5, 0.5, None, None), Verdict::Go);
        assert_eq!(verdict(0.5, 0.5, Some(1.0), None), Verdict::Go);
        assert!(
            matches!(verdict(0.85, 0.85, Some(1.0), None), Verdict::Warn(m) if m.contains("85%") && m.contains("this session"))
        );
        assert!(matches!(verdict(1.0, 1.0, Some(1.0), None), Verdict::Stop(m) if m.contains("this session")));
        // the day's limit counts every session
        assert!(
            matches!(verdict(0.1, 3.0, Some(1.0), Some(3.0)), Verdict::Stop(m) if m.contains("today") && m.contains("max_cost_day"))
        );
        // a stop wins over a warning
        assert!(matches!(verdict(0.9, 5.0, Some(1.0), Some(5.0)), Verdict::Stop(_)));
        assert_eq!(verdict(9.0, 9.0, Some(0.0), None), Verdict::Go, "0 is no limit");
    }

    #[test]
    fn the_days_spending_adds_up_in_its_file() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("NEXO_DATA_DIR", d.path());
        assert_eq!(spent_today(), 0.0);
        add(0.25);
        let total = add(0.5);
        assert!((total - 0.75).abs() < 1e-12);
        assert!((spent_today() - 0.75).abs() < 1e-12);
        let text = std::fs::read_to_string(d.path().join("spend.json")).unwrap();
        assert!(text.contains(&today()), "{text}");
        std::env::remove_var("NEXO_DATA_DIR");
    }
}
