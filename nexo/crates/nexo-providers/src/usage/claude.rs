use serde_json::Value;

use super::copilot::date_secs;
use super::model::{Meter, PlanUsage, CLAUDE};

/// The answer of `GET https://api.anthropic.com/api/oauth/usage` (the endpoint Claude Code's own usage
/// view reads, with `anthropic-beta: oauth-2025-04-20`):
/// `{"five_hour": {"utilization": 6.0, "resets_at": "2026-11-24T18:00:00.000000+00:00"},
///   "seven_day": {"utilization": 35.0, "resets_at": "..."}, "seven_day_opus": null, ...}`.
/// `utilization` is a percentage (0 to 100). A window that is `null` is one the plan does not have.
pub fn claude_from_usage(v: &Value, now: i64) -> Option<PlanUsage> {
    let win = |keys: &[&str], slot: &str, label: &str, minutes: u64| -> Option<Meter> {
        let w = keys.iter().map(|k| &v[*k]).find(|w| w.is_object() && w["utilization"].is_number())?;
        let used_pct = w["utilization"].as_f64().filter(|p| p.is_finite())?;
        Some(Meter {
            slot: slot.into(),
            label: label.into(),
            used_pct,
            counts: None,
            window_minutes: Some(minutes),
            resets_at: w["resets_at"].as_str().and_then(date_secs),
            observed_at: Some(now),
        })
    };
    let meters: Vec<Meter> = [
        win(&["five_hour"], "primary", "5h", 300),
        win(&["seven_day", "seven_day_oauth_apps"], "secondary", "wk", 10_080),
    ]
    .into_iter()
    .flatten()
    .collect();
    if meters.is_empty() {
        return None;
    }
    Some(PlanUsage { kind: CLAUDE.into(), plan: None, meters, credits: None, seen: now })
}
