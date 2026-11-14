use serde_json::Value;

use super::copilot::date_secs;
use super::model::{window_label, Meter, PlanUsage, XAI};

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
