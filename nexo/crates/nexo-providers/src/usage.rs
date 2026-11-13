//! How much of a subscription plan is left: the windows of a ChatGPT plan (5 hours, week) and the
//! premium requests of a GitHub Copilot plan.
//!
//! Two ways in, both from what the vendors' own open-source clients read:
//!
//! - **Passive**: every response of the ChatGPT backend carries `x-codex-primary-*` and
//!   `x-codex-secondary-*` headers (and a `codex.rate_limits` stream event), Copilot's carries
//!   `x-quota-snapshot-premium_interactions`. [`crate::http`] hands the headers of every response to
//!   [`record_headers`], and the figures land in a store shared by the process.
//! - **Active**: [`fetch`] asks for them with the stored login: `GET {backend}/wham/usage` for ChatGPT
//!   (openai/codex, `codex-rs/backend-client`) and `GET api.github.com/copilot_internal/user` for Copilot
//!   (`quota_snapshots`, as the Copilot Chat extension reads it).
//!
//! SuperGrok is read from the endpoint the Grok CLI's own `/usage` reads: `GET {chat proxy}/billing?format=credits`
//! answers with the share of the credits used in the current period and when the period ends.
//!
//! Besides the plans, any provider's rate-limit headers are kept (`x-ratelimit-*` of the OpenAI-style
//! APIs, `anthropic-ratelimit-*`), per host, under the kind `limits:{host}`: what is left of the requests
//! and tokens of the current minute.
//!
//! No token is ever stored in, or printed from, anything here.

use std::collections::BTreeMap;
use std::sync::Mutex;

use nexo_core::auth::{self, Credential};
use nexo_core::{ProviderConfig, ProviderError};
use reqwest::header::HeaderMap;
use serde_json::{json, Value};

use crate::login::{self, Endpoints};

/// The two plans that can be read: ChatGPT's (`codex`) and GitHub Copilot's.
pub const CODEX: &str = "codex";
pub const COPILOT: &str = "copilot";
pub const XAI: &str = "xai";
/// The kind of a reading made from the rate-limit headers of a host: `limits:api.openai.com`.
pub const LIMITS: &str = "limits:";

/// One gauge of a plan: a window of time (`5h`, `wk`) or a count of requests (`premium`).
#[derive(Debug, Clone, PartialEq)]
pub struct Meter {
    /// Where it sits in the plan (`primary`, `secondary`, `premium`, `chat`): a new reading of a slot
    /// replaces the old one, the other slots stay.
    pub slot: String,
    /// What the person reads: `5h`, `wk`, `premium`.
    pub label: String,
    /// 0 to 100 (more when over).
    pub used_pct: f64,
    /// Requests used and granted, when the plan counts them.
    pub counts: Option<(u64, u64)>,
    /// How long the window is.
    pub window_minutes: Option<u64>,
    /// When it starts over, unix seconds.
    pub resets_at: Option<i64>,
}

impl Meter {
    /// The reading has been overtaken by its own reset: the window started over after it was taken.
    pub fn is_stale(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|r| r <= now)
    }

    /// The share still free, 0 to 100.
    pub fn left_pct(&self) -> f64 {
        (100.0 - self.used_pct).clamp(0.0, 100.0)
    }

    /// What is left: `5h 63% left` or `premium 90/300 left`.
    pub fn short(&self) -> String {
        match self.counts {
            Some((used, total)) => format!("{} {}/{total} left", self.label, total.saturating_sub(used)),
            None => format!("{} {:.0}% left", self.label, self.left_pct()),
        }
    }

    /// Seconds to the reset, if it is known and still to come.
    pub fn resets_in(&self, now: i64) -> Option<i64> {
        self.resets_at.map(|r| r - now).filter(|s| *s > 0)
    }
}

/// What the plan of one login allows and how much of it is used.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlanUsage {
    /// `codex`, `copilot`, `xai` or `limits:{host}`.
    pub kind: String,
    /// The plan's name as the vendor gives it (`plus`, `pro`, `individual`).
    pub plan: Option<String>,
    pub meters: Vec<Meter>,
    /// ChatGPT credits beyond the plan: `unlimited` or the balance.
    pub credits: Option<String>,
    /// When it was read, unix seconds.
    pub seen: i64,
}

/// One piece of the status bar: its text and the share used (for the colour).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub text: String,
    pub pct: f64,
}

impl PlanUsage {
    /// A reading made of the rate-limit headers of a host, not of a plan.
    pub fn is_limits(&self) -> bool {
        self.kind.starts_with(LIMITS)
    }

    /// The name of the plan or of the host's limits, as shown.
    pub fn name(&self) -> String {
        match self.kind.as_str() {
            CODEX => "ChatGPT".to_string(),
            COPILOT => "GitHub Copilot".to_string(),
            XAI => "SuperGrok".to_string(),
            k => match k.strip_prefix(LIMITS) {
                Some(host) => format!("{host} rate limits"),
                None => k.to_string(),
            },
        }
    }

    /// The figures that matter on one line each: the windows still running, `5h 63% left ↻2h`. The reset
    /// is left out when `with_reset` is false.
    pub fn items(&self, now: i64, with_reset: bool) -> Vec<Item> {
        self.meters
            .iter()
            .filter(|m| !m.is_stale(now))
            .map(|m| {
                let mut text = m.short();
                if let (true, Some(s)) = (with_reset, m.resets_in(now)) {
                    text.push_str(&format!(" ↻{}", short_span(s)));
                }
                Item { text, pct: m.used_pct }
            })
            .collect()
    }

    /// The full detail, one line per window and a line for the plan and the credits.
    pub fn detail(&self, now: i64) -> Vec<String> {
        let mut out = Vec::new();
        let name = self.name();
        out.push(match &self.plan {
            Some(p) => format!("{name} · {p}"),
            None => name,
        });
        for m in &self.meters {
            let used = match m.counts {
                Some((u, t)) => format!("{} of {t} left ({u} used)", t.saturating_sub(u)),
                None => format!("{:.0}% left ({:.0}% used)", m.left_pct(), m.used_pct),
            };
            let span = m.window_minutes.map(|w| format!(" ({} window)", long_span(w as i64 * 60))).unwrap_or_default();
            let reset = match (m.resets_in(now), m.resets_at) {
                (Some(s), _) => format!(", resets in {}", long_span(s)),
                (None, Some(_)) => ", has reset since".to_string(),
                (None, None) => String::new(),
            };
            out.push(format!("{}{span}: {used}{reset}", m.label));
        }
        if let Some(c) = &self.credits {
            out.push(format!("credits: {c}"));
        }
        out
    }

    /// The figures as JSON, for scripts: `{"plan": "codex", "tier": "plus", "meters": [{"label": "5h", "used_percent": 37.0, "resets_in_seconds": 7200}]}`.
    pub fn to_json(&self, now: i64) -> Value {
        let meters: Vec<Value> = self
            .meters
            .iter()
            .map(|m| {
                let mut o = json!({"label": m.label, "used_percent": m.used_pct});
                if let Some((used, limit)) = m.counts {
                    o["used"] = json!(used);
                    o["limit"] = json!(limit);
                }
                if let Some(s) = m.resets_in(now) {
                    o["resets_in_seconds"] = json!(s);
                }
                o
            })
            .collect();
        let mut v = json!({"plan": self.kind, "meters": meters});
        if let Some(t) = &self.plan {
            v["tier"] = json!(t);
        }
        v
    }

    /// A newer reading on top of this one: its slots replace the old ones, the rest stay.
    fn merge(&mut self, new: PlanUsage) {
        for m in new.meters {
            match self.meters.iter_mut().find(|o| o.slot == m.slot) {
                Some(o) => *o = m,
                None => self.meters.push(m),
            }
        }
        self.plan = new.plan.or(self.plan.take());
        self.credits = new.credits.or(self.credits.take());
        self.seen = new.seen;
        self.meters.sort_by_key(|m| slot_rank(&m.slot));
    }
}

fn slot_rank(slot: &str) -> u8 {
    match slot {
        "primary" | "premium" => 0,
        "secondary" => 1,
        _ => 2,
    }
}

/// `2h`, `2h 14m`-ish in few characters: the two biggest units, the second dropped from a day on.
pub fn short_span(secs: i64) -> String {
    let m = (secs.max(0) + 30) / 60;
    match m {
        0 => "<1m".to_string(),
        m if m < 60 => format!("{m}m"),
        m if m < 60 * 24 => match m % 60 {
            0 => format!("{}h", m / 60),
            r => format!("{}h{r}m", m / 60),
        },
        m => match (m / 60) % 24 {
            0 => format!("{}d", m / 1440),
            r => format!("{}d{r}h", m / 1440),
        },
    }
}

/// `2h 14m`, `4d 3h`: for the detail.
pub fn long_span(secs: i64) -> String {
    short_span(secs).replace('h', "h ").replace('d', "d ").trim_end().to_string()
}

/// The name a window of `minutes` goes by: 300 is `5h`, 10080 is `wk`.
pub fn window_label(minutes: Option<u64>, fallback: &str) -> String {
    match minutes {
        Some(10_080) => "wk".to_string(),
        Some(m) if m > 0 && m % 1440 == 0 && m < 10_080 => format!("{}d", m / 1440),
        Some(m) if m > 0 && m % 60 == 0 && m < 10_080 => format!("{}h", m / 60),
        Some(m) if m > 0 => format!("{m}m"),
        _ => fallback.to_string(),
    }
}

// ---- ChatGPT ----

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name)?.to_str().ok().map(str::trim)
}

fn window_from_headers(h: &HeaderMap, which: &str, now: i64) -> Option<Meter> {
    let pre = format!("x-codex-{which}");
    let used: f64 = header(h, &format!("{pre}-used-percent"))?.parse().ok().filter(|v: &f64| v.is_finite())?;
    // a slot the plan does not use comes as `0` minutes, `0` seconds and an empty `reset-at`
    let minutes = header(h, &format!("{pre}-window-minutes")).and_then(|v| v.parse::<u64>().ok()).filter(|m| *m > 0);
    // the client reads `reset-at` (unix seconds); the backend also says how many seconds are left
    let resets_at = header(h, &format!("{pre}-reset-at")).and_then(|v| v.parse::<i64>().ok()).or_else(|| {
        header(h, &format!("{pre}-reset-after-seconds"))
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|s| *s > 0)
            .map(|s| now + s)
    });
    // an all-zero window is the backend saying nothing
    if used == 0.0 && minutes.unwrap_or(0) == 0 && resets_at.is_none() {
        return None;
    }
    let fallback = if which == "primary" { "5h" } else { "wk" };
    Some(Meter {
        slot: which.to_string(),
        label: window_label(minutes, fallback),
        used_pct: used,
        counts: None,
        window_minutes: minutes,
        resets_at,
    })
}

fn credits_of(has: Option<bool>, unlimited: Option<bool>, balance: Option<&str>) -> Option<String> {
    match (has?, unlimited.unwrap_or(false)) {
        (_, true) => Some("unlimited".into()),
        (true, _) => balance.map(str::trim).filter(|b| !b.is_empty()).map(String::from),
        _ => None,
    }
}

fn flag(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// The ChatGPT backend's rate-limit headers of one response, if it carried any.
pub fn codex_from_headers(h: &HeaderMap, now: i64) -> Option<PlanUsage> {
    let meters: Vec<Meter> = ["primary", "secondary"].iter().filter_map(|w| window_from_headers(h, w, now)).collect();
    let credits = credits_of(
        header(h, "x-codex-credits-has-credits").and_then(flag),
        header(h, "x-codex-credits-unlimited").and_then(flag),
        header(h, "x-codex-credits-balance"),
    );
    if meters.is_empty() && credits.is_none() {
        return None;
    }
    let plan = header(h, "x-codex-plan-type").filter(|p| !p.is_empty()).map(String::from);
    Some(PlanUsage { kind: CODEX.into(), plan, meters, credits, seen: now })
}

/// The `codex.rate_limits` event the backend sends in its stream.
pub fn codex_from_event(v: &Value, now: i64) -> Option<PlanUsage> {
    if v["type"].as_str()? != "codex.rate_limits" {
        return None;
    }
    let win = |which: &str| -> Option<Meter> {
        let w = v["rate_limits"].get(which).filter(|w| w.is_object())?;
        let minutes = w["window_minutes"].as_u64();
        Some(Meter {
            slot: which.into(),
            label: window_label(minutes, if which == "primary" { "5h" } else { "wk" }),
            used_pct: w["used_percent"].as_f64()?,
            counts: None,
            window_minutes: minutes,
            resets_at: w["reset_at"].as_i64(),
        })
    };
    let c = &v["credits"];
    let credits = credits_of(c["has_credits"].as_bool(), c["unlimited"].as_bool(), c["balance"].as_str());
    let meters: Vec<Meter> = ["primary", "secondary"].iter().filter_map(|w| win(w)).collect();
    if meters.is_empty() && credits.is_none() {
        return None;
    }
    Some(PlanUsage { kind: CODEX.into(), plan: v["plan_type"].as_str().map(String::from), meters, credits, seen: now })
}

/// The answer of `GET /wham/usage`.
pub fn codex_from_usage(v: &Value, now: i64) -> Option<PlanUsage> {
    let win = |which: &str, slot: &str| -> Option<Meter> {
        let w = v["rate_limit"].get(which).filter(|w| w.is_object())?;
        let minutes = w["limit_window_seconds"].as_u64().map(|s| s / 60);
        let resets_at =
            w["reset_at"].as_i64().filter(|r| *r > 0).or_else(|| w["reset_after_seconds"].as_i64().map(|s| now + s));
        Some(Meter {
            slot: slot.into(),
            label: window_label(minutes, if slot == "primary" { "5h" } else { "wk" }),
            used_pct: w["used_percent"].as_f64()?,
            counts: None,
            window_minutes: minutes,
            resets_at,
        })
    };
    let meters: Vec<Meter> =
        [win("primary_window", "primary"), win("secondary_window", "secondary")].into_iter().flatten().collect();
    let c = &v["credits"];
    let credits = credits_of(c["has_credits"].as_bool(), c["unlimited"].as_bool(), c["balance"].as_str());
    let plan = v["plan_type"].as_str().map(String::from);
    if meters.is_empty() && credits.is_none() && plan.is_none() {
        return None;
    }
    Some(PlanUsage { kind: CODEX.into(), plan, meters, credits, seen: now })
}

// ---- GitHub Copilot ----

/// A date as Copilot gives it: `2026-12-01`, or a full timestamp. Unix seconds.
fn date_secs(s: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    if let Ok(t) = time::OffsetDateTime::parse(s, &Rfc3339) {
        return Some(t.unix_timestamp());
    }
    let mut p = s.get(..10)?.split('-');
    let (y, m, d) = (p.next()?.parse().ok()?, p.next()?.parse::<u8>().ok()?, p.next()?.parse().ok()?);
    let date = time::Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()?;
    Some(date.midnight().assume_utc().unix_timestamp())
}

fn quota_meter(slot: &str, label: &str, q: &Value, resets_at: Option<i64>) -> Option<Meter> {
    if q["unlimited"].as_bool().unwrap_or(false) {
        return None;
    }
    let total = q["entitlement"].as_f64().filter(|e| *e > 0.0)?;
    let left_pct = q["percent_remaining"].as_f64().or_else(|| q["remaining"].as_f64().map(|r| r / total * 100.0))?;
    let used_pct = (100.0 - left_pct).max(0.0);
    let used = (total * used_pct / 100.0).round() as u64;
    Some(Meter {
        slot: slot.into(),
        label: label.into(),
        used_pct,
        counts: Some((used, total as u64)),
        window_minutes: None,
        resets_at,
    })
}

/// The answer of `GET /copilot_internal/user`.
pub fn copilot_from_user(v: &Value, now: i64) -> Option<PlanUsage> {
    let resets_at = v["quota_reset_date_utc"]
        .as_str()
        .and_then(date_secs)
        .or_else(|| v["quota_reset_date"].as_str().and_then(date_secs));
    let snaps = &v["quota_snapshots"];
    let meters: Vec<Meter> = [
        quota_meter("premium", "premium", &snaps["premium_interactions"], resets_at),
        quota_meter("chat", "chat", &snaps["chat"], resets_at),
    ]
    .into_iter()
    .flatten()
    .collect();
    let plan = v["copilot_plan"].as_str().map(String::from);
    if meters.is_empty() && plan.is_none() {
        return None;
    }
    Some(PlanUsage { kind: COPILOT.into(), plan, meters, credits: None, seen: now })
}

/// Copilot's `x-quota-snapshot-premium_interactions` header: `ent=300&ov=0.0&ovPerm=false&rem=29.5&rst=2026-12-01T00:00:00Z`
/// (`rem` is the percentage left).
pub fn copilot_from_header(h: &HeaderMap, now: i64) -> Option<PlanUsage> {
    let raw =
        header(h, "x-quota-snapshot-premium_interactions").or_else(|| header(h, "x-quota-snapshot-premium_models"))?;
    let get = |k: &str| raw.split('&').find_map(|p| p.strip_prefix(k).and_then(|r| r.strip_prefix('=')));
    let total: f64 = get("ent")?.parse().ok().filter(|e: &f64| *e > 0.0)?;
    let left: f64 = get("rem")?.parse().ok()?;
    let q = json!({"entitlement": total, "percent_remaining": left, "unlimited": false});
    let meter = quota_meter("premium", "premium", &q, get("rst").and_then(date_secs))?;
    Some(PlanUsage { kind: COPILOT.into(), plan: None, meters: vec![meter], credits: None, seen: now })
}

// ---- SuperGrok ----

/// The answer of `GET /billing?format=credits`: `{"config": {"creditUsagePercent": 8.0,
/// "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "...", "end": "..."}, ...}}`.
pub fn xai_from_billing(v: &Value, now: i64) -> Option<PlanUsage> {
    let c = v.get("config").filter(|c| c.is_object()).unwrap_or(v);
    let used_pct = c["creditUsagePercent"].as_f64().filter(|p| p.is_finite())?;
    let at = |a: &Value, b: &Value| a.as_str().and_then(date_secs).or_else(|| b.as_str().and_then(date_secs));
    let p = &c["currentPeriod"];
    let start = at(&p["start"], &c["billingPeriodStart"]);
    let end = at(&p["end"], &c["billingPeriodEnd"]);
    let minutes = start.zip(end).map(|(a, b)| (b - a).max(0) as u64 / 60).filter(|m| *m > 0);
    let label = match p["type"].as_str().unwrap_or_default() {
        "USAGE_PERIOD_TYPE_DAILY" => "day".to_string(),
        "USAGE_PERIOD_TYPE_WEEKLY" => "wk".to_string(),
        "USAGE_PERIOD_TYPE_MONTHLY" => "mo".to_string(),
        _ => window_label(minutes, "plan"),
    };
    let meter =
        Meter { slot: "primary".into(), label, used_pct, counts: None, window_minutes: minutes, resets_at: end };
    let plan = v["subscription_tier"].as_str().or_else(|| c["subscription_tier"].as_str()).map(String::from);
    Some(PlanUsage { kind: XAI.into(), plan, meters: vec![meter], credits: None, seen: now })
}

// ---- rate-limit headers of any provider ----

/// `1s`, `6m0s`, `20ms`, `1h2m3.5s`, or plain seconds.
fn duration_secs(s: &str) -> Option<f64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<f64>() {
        return Some(n).filter(|n| n.is_finite() && *n >= 0.0);
    }
    let (mut total, mut num) = (0.0, String::new());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let unit = match (c, chars.peek()) {
            ('m', Some('s')) => {
                chars.next();
                0.001
            }
            ('s', _) => 1.0,
            ('m', _) => 60.0,
            ('h', _) => 3600.0,
            ('d', _) => 86_400.0,
            _ => return None,
        };
        total += num.parse::<f64>().ok()? * unit;
        num.clear();
    }
    num.is_empty().then_some(total)
}

fn limit_meter(h: &HeaderMap, slot: &str, label: &str, names: [&str; 3], now: i64) -> Option<Meter> {
    let [limit, remaining, reset] = names.map(|n| header(h, n));
    let limit: f64 = limit?.parse().ok().filter(|l: &f64| *l > 0.0)?;
    let left: f64 = remaining?.parse().ok().filter(|r: &f64| r.is_finite() && *r >= 0.0)?;
    let used = (limit - left).max(0.0);
    let resets_at = reset.and_then(|r| {
        date_secs(r).filter(|_| r.contains('T')).or_else(|| duration_secs(r).map(|s| now + s.ceil() as i64))
    });
    Some(Meter {
        slot: slot.into(),
        label: label.into(),
        used_pct: used / limit * 100.0,
        counts: Some((used.round() as u64, limit.round() as u64)),
        window_minutes: None,
        resets_at,
    })
}

/// What is left of the requests and tokens of the current window, from the headers of a response of
/// `host`: `x-ratelimit-{limit,remaining,reset}-{requests,tokens}` (OpenAI and the APIs that copy it) and
/// `anthropic-ratelimit-{requests,tokens,input-tokens,output-tokens}-{limit,remaining,reset}`.
pub fn limits_from_headers(host: &str, h: &HeaderMap, now: i64) -> Option<PlanUsage> {
    let mut meters = Vec::new();
    for (slot, label) in [("requests", "req"), ("tokens", "tok")] {
        let names = [
            format!("x-ratelimit-limit-{slot}"),
            format!("x-ratelimit-remaining-{slot}"),
            format!("x-ratelimit-reset-{slot}"),
        ];
        meters.extend(limit_meter(h, slot, label, [&names[0], &names[1], &names[2]], now));
    }
    for (slot, label) in [("requests", "req"), ("tokens", "tok"), ("input-tokens", "in"), ("output-tokens", "out")] {
        let names = [
            format!("anthropic-ratelimit-{slot}-limit"),
            format!("anthropic-ratelimit-{slot}-remaining"),
            format!("anthropic-ratelimit-{slot}-reset"),
        ];
        // both families on one response: the first one read keeps the slot
        if meters.iter().all(|m| m.slot != slot) {
            meters.extend(limit_meter(h, slot, label, [&names[0], &names[1], &names[2]], now));
        }
    }
    if meters.is_empty() || host.is_empty() {
        return None;
    }
    Some(PlanUsage { kind: format!("{LIMITS}{host}"), plan: None, meters, credits: None, seen: now })
}

// ---- the shared state ----

static STORE: Mutex<BTreeMap<String, PlanUsage>> = Mutex::new(BTreeMap::new());
static CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Wakes when a reading is stored (one wake-up is kept for a listener that is not waiting yet).
pub fn changed() -> &'static tokio::sync::Notify {
    &CHANGED
}

/// Keeps a reading on top of what is known of that plan.
pub fn record(u: PlanUsage) {
    {
        let mut s = STORE.lock().unwrap_or_else(|e| e.into_inner());
        match s.get_mut(&u.kind) {
            Some(old) => old.merge(u),
            None => {
                let mut u = u;
                u.meters.sort_by_key(|m| slot_rank(&m.slot));
                s.insert(u.kind.clone(), u);
            }
        }
    }
    CHANGED.notify_one();
}

/// The latest reading of the plan `kind` (`codex`, `copilot`).
pub fn get(kind: &str) -> Option<PlanUsage> {
    STORE.lock().unwrap_or_else(|e| e.into_inner()).get(kind).cloned()
}

/// Every reading kept, plans first.
pub fn all() -> Vec<PlanUsage> {
    let mut v: Vec<PlanUsage> = STORE.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
    v.sort_by_key(PlanUsage::is_limits);
    v
}

/// The host of a URL: the key of the rate limits it answers with.
pub fn host_of(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
}

/// Takes the plan figures and the rate limits out of the headers of a response of `url`, if it had any.
pub fn record_headers(url: &str, h: &HeaderMap) {
    let now = auth::now() as i64;
    let limits = limits_from_headers(&host_of(url), h, now);
    for u in [codex_from_headers(h, now), copilot_from_header(h, now), limits].into_iter().flatten() {
        record(u);
    }
}

/// Takes the plan figures out of a stream event, if it is one.
pub fn record_event(v: &Value) {
    if let Some(u) = codex_from_event(v, auth::now() as i64) {
        record(u);
    }
}

/// What is known of the plan of `cfg`'s login, else of the rate limits its host reported.
pub fn reading_for(cfg: &ProviderConfig) -> Option<PlanUsage> {
    kind_of(cfg).and_then(get).or_else(|| get(&format!("{LIMITS}{}", host_of(&cfg.base_url))))
}

/// Which plan a provider's login opens, if it is one that can be read.
pub fn kind_of(cfg: &ProviderConfig) -> Option<&'static str> {
    match cfg.login.as_deref()? {
        "codex" | "chatgpt" => Some(CODEX),
        "copilot" => Some(COPILOT),
        "xai" => Some(XAI),
        _ => None,
    }
}

// ---- asking for it ----

/// The plan of provider `name` as its stored login sees it. `Ok(None)` when it has none to read: a
/// provider on a key, no login stored, or a subscription whose usage cannot be read.
pub async fn fetch(name: &str, cfg: &ProviderConfig) -> Result<Option<PlanUsage>, ProviderError> {
    fetch_in(&auth::file(), &Endpoints::default(), name, cfg).await
}

pub async fn fetch_in(
    store: &std::path::Path,
    at: &Endpoints,
    name: &str,
    cfg: &ProviderConfig,
) -> Result<Option<PlanUsage>, ProviderError> {
    let Some(kind) = kind_of(cfg) else { return Ok(None) };
    // a key in the environment (or a command, or a file) is what the requests go with: no plan
    if (ProviderConfig { auth: None, ..cfg.clone() }).resolve_key().is_some() {
        return Ok(None);
    }
    let entry = cfg.auth.clone().unwrap_or_else(|| name.to_string());
    if !matches!(auth::read_from(store).get(&entry), Some(Credential::Oauth { .. })) {
        return Ok(None);
    }
    let (cfg, key) = login::prepare_in(store, at, cfg).await?;
    let Some(key) = key else { return Ok(None) };
    let now = auth::now() as i64;
    let usage = match kind {
        CODEX => {
            let base = cfg.base_url.trim_end_matches('/');
            let root = base.strip_suffix("/codex").unwrap_or(base);
            let mut h = crate::http::headers(&cfg, &[("accept", "application/json".to_string())]);
            if let Ok(v) = format!("Bearer {key}").parse() {
                h.insert(reqwest::header::AUTHORIZATION, v);
            }
            let v = crate::http::get_json(&format!("{root}/wham/usage"), h).await?;
            codex_from_usage(&v, now)
        }
        XAI => xai_from_billing(&xai_billing(at, &key).await?, now),
        _ => {
            let v = copilot_user(at, &key).await?;
            copilot_from_user(&v, now)
        }
    };
    if let Some(u) = &usage {
        record(u.clone());
    }
    Ok(usage)
}

async fn copilot_user(at: &Endpoints, token: &str) -> Result<Value, ProviderError> {
    let resp = crate::http::client()?
        .get(format!("{}/copilot_internal/user", at.github_api))
        .bearer_auth(token)
        .header("accept", "application/json")
        .header("X-GitHub-Api-Version", "2025-04-01")
        .send()
        .await
        .map_err(crate::http::net_error)?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(ProviderError::from_http(status, "GitHub did not give the Copilot plan"));
    }
    resp.json::<Value>().await.map_err(crate::http::net_error)
}

/// `GET {chat proxy}/billing?format=credits`, the request the Grok CLI's `/usage` makes.
async fn xai_billing(at: &Endpoints, token: &str) -> Result<Value, ProviderError> {
    let mut h = HeaderMap::new();
    h.insert(reqwest::header::ACCEPT, reqwest::header::HeaderValue::from_static("application/json"));
    if let Ok(v) = format!("Bearer {token}").parse() {
        h.insert(reqwest::header::AUTHORIZATION, v);
    }
    crate::http::get_json(&format!("{}/billing?format=credits", at.xai_billing.trim_end_matches('/')), h).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        h
    }

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn the_chatgpt_headers_give_both_windows() {
        let h = headers(&[
            ("x-codex-primary-used-percent", "37.4"),
            ("x-codex-primary-window-minutes", "300"),
            ("x-codex-primary-reset-at", "1800007200"),
            ("x-codex-secondary-used-percent", "12"),
            ("x-codex-secondary-window-minutes", "10080"),
            ("x-codex-secondary-reset-after-seconds", "345600"),
            ("x-codex-credits-has-credits", "true"),
            ("x-codex-credits-unlimited", "false"),
            ("x-codex-credits-balance", "12.50"),
            ("x-codex-plan-type", "plus"),
        ]);
        let u = codex_from_headers(&h, NOW).unwrap();
        assert_eq!((u.kind.as_str(), u.plan.as_deref()), ("codex", Some("plus")));
        assert_eq!(u.meters.len(), 2);
        assert_eq!(
            (u.meters[0].label.as_str(), u.meters[0].used_pct, u.meters[0].resets_at),
            ("5h", 37.4, Some(NOW + 7200))
        );
        assert_eq!((u.meters[1].label.as_str(), u.meters[1].resets_at), ("wk", Some(NOW + 345_600)));
        assert_eq!(u.credits.as_deref(), Some("12.50"));
        let items: Vec<String> = u.items(NOW, true).into_iter().map(|i| i.text).collect();
        assert_eq!(items, ["5h 63% left ↻2h", "wk 88% left ↻4d"]);
        let plain: Vec<String> = u.items(NOW, false).into_iter().map(|i| i.text).collect();
        assert_eq!(plain, ["5h 63% left", "wk 88% left"]);
    }

    #[test]
    fn a_response_without_the_headers_gives_nothing() {
        assert!(codex_from_headers(&headers(&[("content-type", "text/event-stream")]), NOW).is_none());
        // the backend sending an all-zero window says nothing
        let zero = headers(&[("x-codex-primary-used-percent", "0")]);
        assert!(codex_from_headers(&zero, NOW).is_none());
        // a Plus plan as the backend sends it: one weekly window, the other slot unused
        let h = headers(&[
            ("x-codex-primary-used-percent", "1"),
            ("x-codex-primary-window-minutes", "10080"),
            ("x-codex-primary-reset-after-seconds", "569494"),
            ("x-codex-secondary-used-percent", "0"),
            ("x-codex-secondary-window-minutes", "0"),
            ("x-codex-secondary-reset-after-seconds", "0"),
            ("x-codex-secondary-reset-at", ""),
            ("x-codex-credits-has-credits", "False"),
            ("x-codex-credits-balance", "0"),
            ("x-codex-credits-unlimited", "False"),
        ]);
        let u = codex_from_headers(&h, NOW).unwrap();
        let items = u.items(NOW, true);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "wk 99% left ↻6d14h");
        assert!(u.credits.is_none());
    }

    #[test]
    fn the_stream_event_and_the_usage_endpoint_agree_with_the_headers() {
        let ev = json!({"type": "codex.rate_limits", "plan_type": "plus",
            "rate_limits": {"primary": {"used_percent": 91.0, "window_minutes": 300, "reset_at": NOW + 600},
                            "secondary": {"used_percent": 3.0, "window_minutes": 10080, "reset_at": NOW + 90_000}},
            "credits": {"has_credits": false, "unlimited": false, "balance": null}});
        let u = codex_from_event(&ev, NOW).unwrap();
        assert_eq!(u.plan.as_deref(), Some("plus"));
        assert_eq!(u.meters[0].short(), "5h 9% left");
        assert_eq!(u.meters[1].resets_at, Some(NOW + 90_000));
        assert!(u.credits.is_none());
        assert!(codex_from_event(&json!({"type": "response.created"}), NOW).is_none());

        let wham = json!({"plan_type": "pro", "rate_limit": {"allowed": true, "limit_reached": false,
            "primary_window": {"used_percent": 20, "limit_window_seconds": 18000, "reset_after_seconds": 3600, "reset_at": NOW + 3600},
            "secondary_window": {"used_percent": 55, "limit_window_seconds": 604800, "reset_after_seconds": 100, "reset_at": NOW + 100}},
            "credits": {"has_credits": true, "unlimited": true, "balance": null}});
        let u = codex_from_usage(&wham, NOW).unwrap();
        assert_eq!(u.plan.as_deref(), Some("pro"));
        assert_eq!((u.meters[0].label.as_str(), u.meters[0].window_minutes), ("5h", Some(300)));
        assert_eq!((u.meters[1].label.as_str(), u.meters[1].used_pct), ("wk", 55.0));
        assert_eq!(u.credits.as_deref(), Some("unlimited"));
        // a plan with no limit object still names itself
        let bare = codex_from_usage(&json!({"plan_type": "team", "rate_limit": null}), NOW).unwrap();
        assert!(bare.meters.is_empty() && bare.plan.as_deref() == Some("team"));
    }

    #[test]
    fn copilot_premium_requests_come_from_the_user_endpoint() {
        let v = json!({"copilot_plan": "individual_pro", "quota_reset_date": "2027-01-15",
            "quota_snapshots": {
                "chat": {"quota_id": "chat", "entitlement": 0, "remaining": 0, "unlimited": true, "percent_remaining": 100.0},
                "completions": {"entitlement": 0, "unlimited": true, "percent_remaining": 100.0},
                "premium_interactions": {"quota_id": "premium_interactions", "entitlement": 300, "remaining": 90,
                    "unlimited": false, "overage_count": 0, "overage_permitted": false, "percent_remaining": 30.0}}});
        let u = copilot_from_user(&v, NOW).unwrap();
        assert_eq!(u.plan.as_deref(), Some("individual_pro"));
        assert_eq!(u.meters.len(), 1, "an unlimited chat is no gauge");
        assert_eq!(u.meters[0].short(), "premium 90/300 left");
        assert_eq!(u.meters[0].used_pct, 70.0);
        assert_eq!(u.meters[0].resets_at, Some(1_799_971_200));
        let d = u.detail(1_799_971_200 - 3 * 86_400 - 2 * 3600).join("\n");
        assert!(d.contains("premium: 90 of 300 left (210 used), resets in 3d 2h"), "{d}");
    }

    #[test]
    fn copilot_headers_carry_the_same_figures() {
        let h = headers(&[(
            "x-quota-snapshot-premium_interactions",
            "ent=300&ov=0.0&ovPerm=false&rem=10&rst=2027-01-15T00:00:00Z",
        )]);
        let u = copilot_from_header(&h, NOW).unwrap();
        assert_eq!(u.meters[0].short(), "premium 30/300 left");
        assert_eq!(u.meters[0].resets_at, Some(1_799_971_200));
        assert!(copilot_from_header(&headers(&[]), NOW).is_none());
    }

    #[test]
    fn a_window_that_has_reset_leaves_the_status_bar() {
        let h = headers(&[("x-codex-primary-used-percent", "99"), ("x-codex-primary-reset-at", "1799999000")]);
        let u = codex_from_headers(&h, NOW).unwrap();
        assert!(u.items(NOW, true).is_empty());
        assert!(u.detail(NOW).join("\n").contains("has reset since"));
    }

    #[test]
    fn a_new_reading_replaces_its_slot_and_keeps_the_others() {
        let mut a = PlanUsage { kind: "codex".into(), plan: Some("plus".into()), seen: 1, ..Default::default() };
        let w = |slot: &str, pct: f64| Meter {
            slot: slot.into(),
            label: slot.into(),
            used_pct: pct,
            counts: None,
            window_minutes: None,
            resets_at: None,
        };
        a.meters = vec![w("primary", 10.0), w("secondary", 20.0)];
        a.merge(PlanUsage { kind: "codex".into(), meters: vec![w("primary", 30.0)], seen: 2, ..Default::default() });
        assert_eq!(a.meters.iter().map(|m| m.used_pct).collect::<Vec<_>>(), [30.0, 20.0]);
        assert_eq!((a.plan.as_deref(), a.seen), (Some("plus"), 2));
    }

    #[test]
    fn supergrok_credits_come_from_the_billing_answer() {
        // the answer of the chat proxy, as the Grok CLI's `/usage` reads it
        let v = json!({"config": {
            "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "2027-01-15T00:00:00.000+00:00", "end": "2027-01-22T00:00:00.000+00:00"},
            "creditUsagePercent": 8.0, "productUsage": [{"product": "GrokBot", "usagePercent": 8.0}],
            "isUnifiedBillingUser": true, "prepaidBalance": {"val": 0}}});
        let u = xai_from_billing(&v, NOW).unwrap();
        assert_eq!((u.kind.as_str(), u.meters.len()), ("xai", 1));
        let m = &u.meters[0];
        assert_eq!(
            (m.label.as_str(), m.used_pct, m.window_minutes, m.resets_at),
            ("wk", 8.0, Some(10_080), Some(1_800_576_000))
        );
        let items = u.items(1_800_576_000 - 3 * 86_400, true);
        assert_eq!(items[0].text, "wk 92% left ↻3d");
        let d = u.detail(1_800_576_000 - 3 * 86_400).join("\n");
        assert!(d.starts_with("SuperGrok") && d.contains("wk (7d window): 92% left (8% used), resets in 3d"), "{d}");
        // a monthly period, and an answer with no figure
        let monthly = json!({"creditUsagePercent": 50, "currentPeriod": {"type": "USAGE_PERIOD_TYPE_MONTHLY"}});
        assert_eq!(xai_from_billing(&monthly, NOW).unwrap().meters[0].label, "mo");
        assert!(xai_from_billing(&json!({"config": {"isUnifiedBillingUser": false}}), NOW).is_none());
        assert!(xai_from_billing(&json!({}), NOW).is_none());
    }

    #[tokio::test]
    async fn the_supergrok_plan_is_asked_with_the_stored_login() {
        use httpmock::prelude::*;
        let s = MockServer::start();
        let m = s.mock(|w, t| {
            w.method(GET).path("/v1/billing").query_param("format", "credits").header("authorization", "Bearer xai-tok");
            t.status(200).json_body(json!({"config": {"creditUsagePercent": 37.0,
                "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "2099-01-01T00:00:00Z", "end": "2099-01-08T00:00:00Z"}}}));
        });
        let (_dir, store) = store_with("xai", "xai-tok");
        let mut cfg = ProviderConfig::new("openai-chat", "https://api.x.ai/v1", None);
        cfg.login = Some("xai".into());
        cfg.auth = Some("xai".into());
        let u = fetch_in(&store, &endpoints(&s), "xai", &cfg).await.unwrap().unwrap();
        m.assert();
        assert_eq!(u.meters[0].short(), "wk 63% left");
        assert_eq!(reading_for(&cfg).map(|r| r.kind), Some("xai".to_string()));
        // a refusal is an error the caller can show, not a figure
        let s2 = MockServer::start();
        s2.mock(|w, t| {
            w.method(GET).path("/v1/billing");
            t.status(403).body("{\"error\": \"no\"}");
        });
        assert!(fetch_in(&store, &endpoints(&s2), "xai", &cfg).await.is_err());
    }

    #[test]
    fn the_openai_style_rate_limit_headers_say_what_is_left() {
        let h = headers(&[
            ("x-ratelimit-limit-requests", "60"),
            ("x-ratelimit-remaining-requests", "59"),
            ("x-ratelimit-reset-requests", "1s"),
            ("x-ratelimit-limit-tokens", "150000"),
            ("x-ratelimit-remaining-tokens", "149984"),
            ("x-ratelimit-reset-tokens", "6m0s"),
        ]);
        let u = limits_from_headers("api.example.com", &h, NOW).unwrap();
        assert_eq!(u.kind, "limits:api.example.com");
        assert!(u.is_limits());
        assert_eq!(u.meters.iter().map(Meter::short).collect::<Vec<_>>(), ["req 59/60 left", "tok 149984/150000 left"]);
        assert_eq!((u.meters[0].resets_at, u.meters[1].resets_at), (Some(NOW + 1), Some(NOW + 360)));
        assert_eq!(u.detail(NOW)[0], "api.example.com rate limits");
        // a response with no limit headers, or a remaining figure with no limit to measure it by
        assert!(limits_from_headers("h", &headers(&[("content-type", "text/plain")]), NOW).is_none());
        assert!(limits_from_headers("h", &headers(&[("x-ratelimit-remaining-requests", "5")]), NOW).is_none());
        assert!(limits_from_headers("", &h, NOW).is_none());
    }

    #[test]
    fn the_anthropic_rate_limit_headers_say_what_is_left() {
        let h = headers(&[
            ("anthropic-ratelimit-requests-limit", "50"),
            ("anthropic-ratelimit-requests-remaining", "0"),
            ("anthropic-ratelimit-requests-reset", "2027-01-15T00:00:00Z"),
            ("anthropic-ratelimit-input-tokens-limit", "30000"),
            ("anthropic-ratelimit-input-tokens-remaining", "12000"),
        ]);
        let u = limits_from_headers("api.anthropic.com", &h, NOW).unwrap();
        assert_eq!(u.meters.iter().map(Meter::short).collect::<Vec<_>>(), ["req 0/50 left", "in 12000/30000 left"]);
        assert_eq!(u.meters[0].resets_at, Some(1_799_971_200));
        assert_eq!(u.meters[0].used_pct, 100.0);
    }

    #[test]
    fn durations_read_as_the_apis_write_them() {
        assert_eq!(duration_secs("1s"), Some(1.0));
        assert_eq!(duration_secs("6m0s"), Some(360.0));
        assert_eq!(duration_secs("20ms"), Some(0.02));
        assert_eq!(duration_secs("1h2m3.5s"), Some(3723.5));
        assert_eq!(duration_secs("45"), Some(45.0));
        assert_eq!(duration_secs("soon"), None);
    }

    #[test]
    fn rate_limits_are_kept_per_host_and_a_plan_wins_over_them() {
        let h = headers(&[("x-ratelimit-limit-requests", "10"), ("x-ratelimit-remaining-requests", "4")]);
        record_headers("https://limits-test-a.example/v1/chat", &h);
        let cfg = ProviderConfig::new("openai-chat", "https://limits-test-a.example/v1", None);
        assert_eq!(reading_for(&cfg).map(|u| u.meters[0].short()), Some("req 4/10 left".to_string()));
        let other = ProviderConfig::new("openai-chat", "https://limits-test-b.example/v1", None);
        assert!(reading_for(&other).is_none(), "another host has its own figures");
        assert!(all().iter().any(|u| u.kind == "limits:limits-test-a.example"));
    }

    #[test]
    fn spans_read_short() {
        assert_eq!(short_span(20), "<1m");
        assert_eq!(short_span(45 * 60), "45m");
        assert_eq!(short_span(2 * 3600 - 1), "2h");
        assert_eq!(short_span(2 * 3600 + 14 * 60), "2h14m");
        assert_eq!(short_span(4 * 86_400), "4d");
        assert_eq!(short_span(4 * 86_400 + 3 * 3600), "4d3h");
        assert_eq!(long_span(2 * 3600 + 14 * 60), "2h 14m");
        assert_eq!(long_span(4 * 86_400 + 3 * 3600), "4d 3h");
        assert_eq!(long_span(3 * 3600), "3h");
        assert_eq!(window_label(Some(300), "x"), "5h");
        assert_eq!(window_label(Some(10_080), "x"), "wk");
        assert_eq!(window_label(Some(1440), "x"), "1d");
        assert_eq!(window_label(None, "x"), "x");
    }

    fn endpoints(s: &httpmock::MockServer) -> Endpoints {
        Endpoints { github_api: s.base_url(), xai_billing: s.url("/v1"), ..Endpoints::default() }
    }

    fn store_with(flow: &str, token: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("auth.json");
        let md = BTreeMap::from([("flow".to_string(), flow.to_string())]);
        let cred = Credential::Oauth { access: token.into(), refresh: "r".into(), expires: 0, metadata: md };
        auth::set_in(&store, flow, cred).unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn the_chatgpt_plan_is_asked_with_the_stored_login() {
        use httpmock::prelude::*;
        let s = MockServer::start();
        let m = s.mock(|w, t| {
            w.method(GET).path("/backend-api/wham/usage").header("authorization", "Bearer tok-x").header("chatgpt-account-id", "acc1");
            t.status(200).json_body(json!({"plan_type": "plus", "rate_limit": {
                "primary_window": {"used_percent": 41, "limit_window_seconds": 18000, "reset_after_seconds": 600, "reset_at": 0},
                "secondary_window": null}}));
        });
        let (_dir, store) = store_with("codex", "tok-x");
        let mut cfg = ProviderConfig::new("openai-responses", &s.url("/backend-api/codex"), None);
        cfg.login = Some("codex".into());
        cfg.auth = Some("codex".into());
        cfg.headers.insert("chatgpt-account-id".into(), "acc1".into());
        let u = fetch_in(&store, &endpoints(&s), "codex", &cfg).await.unwrap().unwrap();
        m.assert();
        assert_eq!((u.plan.as_deref(), u.meters.len()), (Some("plus"), 1));
        assert_eq!(u.meters[0].short(), "5h 59% left");
    }

    #[tokio::test]
    async fn the_copilot_plan_is_asked_of_github() {
        use httpmock::prelude::*;
        let s = MockServer::start();
        s.mock(|w, t| {
            w.method(GET).path("/copilot_internal/user").header("authorization", "Bearer gho_x");
            t.status(200).json_body(json!({"copilot_plan": "business", "quota_reset_date": "2027-02-01", "quota_snapshots": {
                "premium_interactions": {"entitlement": 300, "remaining": 60, "unlimited": false, "percent_remaining": 20.0}}}));
        });
        let (_dir, store) = store_with("copilot", "gho_x");
        let mut cfg = ProviderConfig::new("copilot", crate::copilot::BASE_URL, None);
        cfg.login = Some("copilot".into());
        cfg.auth = Some("copilot".into());
        let u = fetch_in(&store, &endpoints(&s), "copilot", &cfg).await.unwrap().unwrap();
        assert_eq!(u.meters[0].short(), "premium 60/300 left");
        assert_eq!(get("copilot").map(|g| g.plan), Some(Some("business".to_string())));
    }

    #[tokio::test]
    async fn a_provider_on_a_key_or_without_a_login_has_no_plan_and_no_request_is_made() {
        use httpmock::prelude::*;
        let s = MockServer::start();
        let any = s.mock(|_, t| {
            t.status(500);
        });
        let (_dir, store) = store_with("codex", "tok-x");
        // a key kept in the store, not a login
        let mut keyed = ProviderConfig::new("openai-chat", "https://api.example", None);
        keyed.auth = Some("deepseek".into());
        assert!(fetch_in(&store, &endpoints(&s), "deepseek", &keyed).await.unwrap().is_none());
        // a login configured but never made
        let mut nologin = ProviderConfig::new("copilot", &s.url("/x"), None);
        nologin.login = Some("copilot".into());
        assert!(fetch_in(&store, &endpoints(&s), "copilot", &nologin).await.unwrap().is_none());
        // SuperGrok configured, no login stored
        let mut xai = ProviderConfig::new("openai-chat", &s.url("/x"), None);
        xai.login = Some("xai".into());
        assert!(fetch_in(&store, &endpoints(&s), "xai", &xai).await.unwrap().is_none());
        any.assert_calls(0);
    }

    #[test]
    fn only_a_subscription_login_has_a_plan() {
        let mut c = ProviderConfig::new("openai-chat", "https://api.example", None);
        assert_eq!(kind_of(&c), None);
        c.login = Some("xai".into());
        assert_eq!(kind_of(&c), Some("xai"));
        c.login = Some("codex".into());
        assert_eq!(kind_of(&c), Some("codex"));
        c.login = Some("copilot".into());
        assert_eq!(kind_of(&c), Some("copilot"));
    }
}
