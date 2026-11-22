use reqwest::header::HeaderMap;

use super::codex::header;
use super::copilot::date_secs;
use super::model::{Meter, PlanUsage, LIMITS};

/// `1s`, `6m0s`, `20ms`, `1h2m3.5s`, or plain seconds.
pub(super) fn duration_secs(s: &str) -> Option<f64> {
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

pub(super) fn limit_meter(h: &HeaderMap, slot: &str, label: &str, names: [&str; 3], now: i64) -> Option<Meter> {
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
        observed_at: None,
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
