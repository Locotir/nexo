//! Spend limits: one per session (`[agent] max_cost`) and one per day across every
//! session (`[agent] max_cost_day`), in dollars. What was spent each day is kept in
//! `spend.json` in the data directory. Before each model call the agent asks [`admit`]: past
//! 80% it warns once, at the limit it stops the request before the call is made. The calls
//! already in flight count against the limit at their estimated cost, and a request whose full
//! reply the rest of the budget could not pay gets a lower output cap, so a limit is not passed
//! by what several calls started together would cost.

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
/// What went wrong with the spending file or its lock, said once to the person by whoever bills the call
/// (see [`take_problem`]). Only the first of a kind is kept, so a file that stays broken is one warning.
struct Problems {
    seen: Vec<&'static str>,
    pending: Option<String>,
}

impl Problems {
    fn note(&mut self, kind: &'static str, text: String) {
        if !self.seen.contains(&kind) {
            self.seen.push(kind);
            self.pending = Some(text);
        }
    }
}

static PROBLEMS: std::sync::Mutex<Problems> = std::sync::Mutex::new(Problems { seen: Vec::new(), pending: None });

fn problem(kind: &'static str, text: String) {
    if let Ok(mut p) = PROBLEMS.lock() {
        p.note(kind, text);
    }
}

/// The warning waiting to be shown, if any: the daily limit cannot be trusted while it stands.
pub fn take_problem() -> Option<String> {
    PROBLEMS.lock().ok().and_then(|mut p| p.pending.take())
}

/// Dollars spent today, every session together.
pub fn spent_today() -> f64 {
    match load() {
        Ok(d) => d.get(&today()).copied().unwrap_or(0.0),
        Err(e) => {
            problem(
                "read",
                format!(
                    "cannot read {} ({e}); today's spending counts as $0 and `max_cost_day` may not hold",
                    file().display()
                ),
            );
            0.0
        }
    }
}

/// Adds `cost` to today's spending; returns the new total. The read, change and write happen
/// under a lock shared by every nexo process. A file that does not parse is kept as
/// `spend.json.bak` before a fresh one is started, so the history is never silently lost.
pub fn add(cost: f64) -> f64 {
    let f = file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    // without the lock another process may write at the same time and one of the two additions is lost:
    // the write goes on, and the person is told once that the day's total may be short
    let _lock = match nexo_core::auth::lock_in(&f) {
        Ok(l) => Some(l),
        Err(e) => {
            problem(
                "lock",
                format!(
                    "cannot lock {} ({e}); spending is written without it and the day's total may miss a call",
                    f.display()
                ),
            );
            None
        }
    };
    let mut days = match load() {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
            problem(
                "parse",
                format!("{} does not parse ({e}); kept as spend.json.bak and started again, so today's total restarts at $0", f.display()),
            );
            let _ = std::fs::copy(&f, f.with_extension("json.bak"));
            BTreeMap::new()
        }
        // unreadable for another reason: count in memory only, write nothing
        Err(e) => {
            problem("read", format!("cannot read {} ({e}); this call is not added to the day's total", f.display()));
            return cost.max(0.0);
        }
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
        match std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &f)) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                problem(
                    "write",
                    format!("cannot write {} ({e}); this call is missing from the day's total", f.display()),
                );
            }
        }
    }
    total
}

/// Model calls started and not yet billed: (id, session, estimated dollars). Every session of the process
/// shares the day, so a call of one is in flight for the daily limit of all.
static FLIGHTS: std::sync::Mutex<Vec<(u64, String, f64)>> = std::sync::Mutex::new(Vec::new());
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A call in flight; dropped once it is billed (or fails), it stops counting against the limits.
#[derive(Debug, Default)]
pub struct Flight(u64);

impl Drop for Flight {
    fn drop(&mut self) {
        if self.0 != 0 {
            if let Ok(mut f) = FLIGHTS.lock() {
                f.retain(|(id, ..)| *id != self.0);
            }
        }
    }
}

/// The estimated cost of the other calls in flight: those of `session`, and those of every session.
fn in_flight(session: &str) -> (f64, f64) {
    let f = FLIGHTS.lock().map(|f| f.clone()).unwrap_or_default();
    let all = f.iter().map(|(_, _, c)| c).sum();
    (f.iter().filter(|(_, s, _)| s == session).map(|(_, _, c)| c).sum(), all)
}

/// A reply shorter than this is not worth capping to: the call goes on at this size even when the budget
/// is a little short of paying it.
const MIN_REPLY: u32 = 256;

/// Asks to start a model call: `Err` says the limit is reached (what is spent plus what is in flight);
/// `Ok` holds the output cap to send, lowered when the rest of the budget could not pay a full reply at the
/// configured output price, and the [`Flight`] to keep until the call is billed. `input_tokens` is the
/// estimated size of the request. Without known prices nothing can be estimated: the call is counted as free.
pub fn admit(
    session: &str,
    session_spent: f64,
    session_limit: Option<f64>,
    day_limit: Option<f64>,
    pricing: Option<nexo_core::pricing::Pricing>,
    input_tokens: u64,
    max_tokens: u32,
) -> Result<(u32, Flight), String> {
    admit_with(session, session_spent, spent_today(), session_limit, day_limit, pricing, input_tokens, max_tokens)
}

/// [`admit`] with the day's spending given.
#[allow(clippy::too_many_arguments)]
fn admit_with(
    session: &str,
    session_spent: f64,
    day_spent: f64,
    session_limit: Option<f64>,
    day_limit: Option<f64>,
    pricing: Option<nexo_core::pricing::Pricing>,
    input_tokens: u64,
    max_tokens: u32,
) -> Result<(u32, Flight), String> {
    let (mine, all) = in_flight(session);
    let day = day_spent + all;
    let sess = session_spent + mine;
    if let Verdict::Stop(why) = verdict(sess, day, session_limit, day_limit) {
        return Err(why);
    }
    let Some(p) = pricing.filter(|p| p.is_known()) else { return Ok((max_tokens, Flight(0))) };
    let remaining = [session_limit.map(|l| l - sess), day_limit.map(|l| l - day)]
        .into_iter()
        .zip([session_limit, day_limit])
        .filter_map(|(r, l)| r.filter(|_| l.is_some_and(|l| l > 0.0)))
        .fold(f64::INFINITY, f64::min);
    let input = p.cost(&nexo_core::stream::Usage { input: input_tokens, ..Default::default() });
    let mut max = max_tokens;
    if remaining.is_finite() && p.output > 0.0 {
        let affordable = ((remaining - input).max(0.0) * 1_000_000.0 / p.output) as u64;
        if affordable < u64::from(max) {
            max = (affordable as u32).max(MIN_REPLY.min(max_tokens));
        }
    }
    let cost = input + p.cost(&nexo_core::stream::Usage { output: u64::from(max), ..Default::default() });
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if let Ok(mut f) = FLIGHTS.lock() {
        f.push((id, session.to_string(), cost));
    }
    Ok((max, Flight(id)))
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

    const PRICES: nexo_core::pricing::Pricing = nexo_core::pricing::Pricing {
        input: 1.0,
        output: 10.0,
        cache_read: 0.0,
        cache_write: 0.0,
        cache_write_1h: 0.0,
    };

    #[test]
    fn calls_in_flight_count_against_the_limit_at_their_estimated_cost() {
        // a $1 session limit at $10 per million output: a 50k-token reply is $0.50
        let s = "sentence-in-flight";
        let ask = || admit_with(s, 0.0, 0.0, Some(1.0), None, Some(PRICES), 0, 50_000);
        let (cap, a) = ask().unwrap();
        assert_eq!(cap, 50_000);
        let (_, b) = ask().unwrap();
        let why = ask().unwrap_err();
        assert!(why.contains("this session"), "{why}");
        // a call that ends frees what it held
        drop(a);
        assert!(ask().is_ok());
        drop(b);
    }

    #[test]
    fn a_reply_the_rest_of_the_budget_could_not_pay_is_asked_for_shorter() {
        // $0.25 left at $10 per million output pays 25k tokens
        let (cap, _f) = admit_with("sentence-cap", 0.75, 0.0, Some(1.0), None, Some(PRICES), 0, 64_000).unwrap();
        assert_eq!(cap, 25_000);
        let (cap, _g) =
            admit_with("sentence-cap-in", 0.75, 0.0, Some(1.0), None, Some(PRICES), 50_000, 64_000).unwrap();
        assert_eq!(cap, 20_000, "the request's input is paid first");
        // a reply that fits is left alone
        let (cap, _i) = admit_with("sentence-cap-fit", 0.0, 0.0, Some(10.0), None, Some(PRICES), 0, 64_000).unwrap();
        assert_eq!(cap, 64_000);
    }

    #[test]
    fn a_nearly_spent_budget_still_gets_a_small_reply_and_unknown_prices_change_nothing() {
        let (cap, _f) = admit_with("sentence-floor", 0.9999, 0.0, Some(1.0), None, Some(PRICES), 0, 64_000).unwrap();
        assert_eq!(cap, MIN_REPLY);
        let (cap, _g) = admit_with("sentence-free", 0.5, 0.0, Some(1.0), None, None, 1_000_000, 64_000).unwrap();
        assert_eq!(cap, 64_000, "no configured prices: nothing to estimate");
        assert!(admit_with("sentence-none", 5.0, 0.0, None, None, Some(PRICES), 0, 64_000).is_ok(), "no limit, no cap");
    }

    #[test]
    fn a_failure_of_the_spending_file_is_one_warning_not_silence() {
        let mut p = Problems { seen: Vec::new(), pending: None };
        p.note("lock", "cannot lock".into());
        assert_eq!(p.pending.take().as_deref(), Some("cannot lock"));
        p.note("lock", "cannot lock again".into());
        assert_eq!(p.pending, None, "the same kind is said once");
        p.note("read", "cannot read".into());
        assert_eq!(p.pending.take().as_deref(), Some("cannot read"));
    }

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
