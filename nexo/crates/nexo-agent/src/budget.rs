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

/// The machine's UTC offset, read once. Reading it needs a single-threaded process, so
/// [`init_local_offset`] runs first thing in `main`, before the async runtime starts.
static OFFSET: std::sync::OnceLock<Option<time::UtcOffset>> = std::sync::OnceLock::new();

/// Reads the local UTC offset while the process still has one thread. Without it the day
/// rolls over at UTC midnight.
pub fn init_local_offset() {
    let _ = OFFSET.set(time::UtcOffset::current_local_offset().ok());
}

/// Today, as the file keys it (`2026-10-28`, local time where known).
pub fn today() -> String {
    let utc = time::OffsetDateTime::now_utc();
    let now = match OFFSET.get().copied().flatten() {
        Some(o) => utc.to_offset(o),
        None => utc,
    };
    format!("{:04}-{:02}-{:02}", now.year(), u8::from(now.month()), now.day())
}

/// The days on disk. A missing file is empty; a file that does not parse is an error, so
/// it is never written over.
fn load() -> std::io::Result<BTreeMap<String, f64>> {
    match std::fs::read_to_string(file()) {
        Ok(t) if t.trim().is_empty() => Ok(BTreeMap::new()),
        Ok(t) => serde_json::from_str(&t).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e),
    }
}

/// Dollars spent today, every session together.
pub fn spent_today() -> f64 {
    load().unwrap_or_default().get(&today()).copied().unwrap_or(0.0)
}

/// Adds `cost` to today's spending; returns the new total. The read, change and write happen
/// under a lock shared by every nexo process. A file that does not parse is kept as
/// `spend.json.bak` before a fresh one is started, so the history is never silently lost.
pub fn add(cost: f64) -> f64 {
    let f = file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _lock = nexo_core::auth::lock_in(&f).ok();
    let mut days = match load() {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            let _ = std::fs::copy(&f, f.with_extension("json.bak"));
            BTreeMap::new()
        }
        // unreadable for another reason: count in memory only, write nothing
        Err(_) => return cost.max(0.0),
    };
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
    if let Ok(text) = serde_json::to_string_pretty(&days) {
        let tmp = f.with_extension(format!("json.{}.tmp", std::process::id()));
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &f).is_err() {
            let _ = std::fs::remove_file(&tmp);
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
        // many writers at once lose nothing
        let hs: Vec<_> = (0..8).map(|_| std::thread::spawn(|| (0..5).for_each(|_| _ = add(0.01)))).collect();
        hs.into_iter().for_each(|h| h.join().unwrap());
        assert!((spent_today() - 1.15).abs() < 1e-9, "{}", spent_today());
        // a file that does not parse is kept as a backup, not wiped
        std::fs::write(d.path().join("spend.json"), "{ not json").unwrap();
        assert_eq!(spent_today(), 0.0);
        assert!((add(0.1) - 0.1).abs() < 1e-12);
        let bak = std::fs::read_to_string(d.path().join("spend.json.bak")).unwrap();
        assert_eq!(bak, "{ not json");
        std::env::remove_var("NEXO_DATA_DIR");
    }
}
