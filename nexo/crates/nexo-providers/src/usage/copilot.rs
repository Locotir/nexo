use reqwest::header::HeaderMap;
use serde_json::{json, Value};

use super::codex::header;
use super::model::{Meter, PlanUsage, COPILOT};

/// A date as Copilot gives it: `2026-12-01`, or a full timestamp. Unix seconds.
pub(super) fn date_secs(s: &str) -> Option<i64> {
    use time::format_description::well_known::Rfc3339;
    if let Ok(t) = time::OffsetDateTime::parse(s, &Rfc3339) {
        return Some(t.unix_timestamp());
    }
    let mut p = s.get(..10)?.split('-');
    let (y, m, d) = (p.next()?.parse().ok()?, p.next()?.parse::<u8>().ok()?, p.next()?.parse().ok()?);
    let date = time::Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()?;
    Some(date.midnight().assume_utc().unix_timestamp())
}

pub(super) fn quota_meter(slot: &str, label: &str, q: &Value, resets_at: Option<i64>) -> Option<Meter> {
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
