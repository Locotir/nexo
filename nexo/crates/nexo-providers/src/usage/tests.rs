use std::collections::BTreeMap;

use nexo_core::auth::{self, Credential};
use nexo_core::ProviderConfig;
use reqwest::header::HeaderMap;
use serde_json::json;

use super::limits::duration_secs;
use super::*;
use crate::login::Endpoints;
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
        observed_at: None,
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

#[tokio::test]
async fn a_credential_store_that_does_not_parse_is_an_error_not_a_provider_without_a_plan() {
    use httpmock::prelude::*;
    let s = MockServer::start();
    let (_dir, store) = store_with("xai", "xai-tok");
    std::fs::write(&store, "not json").unwrap();
    let mut cfg = ProviderConfig::new("openai-chat", "https://api.x.ai/v1", None);
    cfg.login = Some("xai".into());
    cfg.auth = Some("xai".into());
    let e = fetch_in(&store, &endpoints(&s), "xai", &cfg).await.expect_err("said, not read as signed out");
    assert!(e.to_string().contains("cannot be read"), "{e}");
}

#[test]
fn pace_warning_requires_a_fresh_consistent_window_and_significant_overuse() {
    let now = 1_800_000_000;
    let mut m = Meter {
        slot: "primary".into(),
        label: "5h".into(),
        used_pct: 50.0,
        counts: None,
        window_minutes: Some(300),
        resets_at: Some(now + 3 * 3600),
        observed_at: Some(now),
    };
    let mut p = PlanUsage { kind: "codex".into(), seen: now, meters: vec![m.clone()], ..Default::default() };
    assert!(!p.fast_pace(&m, now)); // exactly the 25% margin
    m.used_pct = 55.0;
    assert!(p.fast_pace(&m, now));
    assert!(!p.fast_pace(&m, now + 3601));
    m.window_minutes = None;
    assert!(!p.fast_pace(&m, now));
    m.window_minutes = Some(300);
    m.resets_at = Some(now + 6 * 3600);
    assert!(!p.fast_pace(&m, now));
    m.resets_at = Some(now - 1);
    assert!(!p.fast_pace(&m, now));
    m.resets_at = Some(now + 3 * 3600);
    p.kind = "limits:example".into();
    assert!(!p.fast_pace(&m, now));
}

#[test]
fn codex_pace_tracks_each_merged_slots_own_observation() {
    let now = 1_800_000_000;
    let event = |primary: Option<f64>, secondary: Option<f64>| {
        let mut limits = serde_json::json!({});
        if let Some(pct) = primary {
            limits["primary"] =
                serde_json::json!({"used_percent": pct, "window_minutes": 300, "reset_at": now + 3 * 3600});
        }
        if let Some(pct) = secondary {
            limits["secondary"] =
                serde_json::json!({"used_percent": pct, "window_minutes": 10080, "reset_at": now + 5 * 86400});
        }
        serde_json::json!({"type": "codex.rate_limits", "rate_limits": limits})
    };
    let mut plan = codex_from_event(&event(Some(80.0), Some(60.0)), now).unwrap();
    assert!(plan.meters.iter().all(|m| plan.fast_pace(m, now)));
    plan.merge(codex_from_event(&event(Some(80.0), None), now + 3601).unwrap());
    assert!(plan.fast_pace(&plan.meters[0], now + 3601));
    assert!(!plan.fast_pace(&plan.meters[1], now + 3601));
    assert_eq!(plan.meters[1].observed_at, Some(now));
    let mut unknown = plan.meters[0].clone();
    unknown.observed_at = None;
    assert!(!plan.fast_pace(&unknown, now + 3601));
    unknown.observed_at = Some(now + 3602);
    assert!(!plan.fast_pace(&unknown, now + 3601));
}
