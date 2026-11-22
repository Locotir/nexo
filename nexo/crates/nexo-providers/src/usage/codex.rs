use reqwest::header::HeaderMap;
use serde_json::Value;

use super::model::{window_label, Meter, PlanUsage, CODEX};

pub(super) fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name)?.to_str().ok().map(str::trim)
}

pub(super) fn window_from_headers(h: &HeaderMap, which: &str, now: i64) -> Option<Meter> {
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
        observed_at: Some(now),
    })
}

pub(super) fn credits_of(has: Option<bool>, unlimited: Option<bool>, balance: Option<&str>) -> Option<String> {
    match (has?, unlimited.unwrap_or(false)) {
        (_, true) => Some("unlimited".into()),
        (true, _) => balance.map(str::trim).filter(|b| !b.is_empty()).map(String::from),
        _ => None,
    }
}

pub(super) fn flag(v: &str) -> Option<bool> {
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
            observed_at: Some(now),
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
            observed_at: Some(now),
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
