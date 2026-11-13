use super::*;

pub(super) const CONFIG_PROBE: &str = r##"use uploader::config;

#[test]
fn hidden_config() {
    std::env::set_var("ACME_BUCKET", "b");
    std::env::set_var("ACME_REGION", "r");
    std::env::remove_var("ACME_TIMEOUT");
    let plain = config::load();
    std::env::set_var("ACME_TIMEOUT", "12");
    let timed = config::load();
    assert_eq!((plain.bucket.as_str(), plain.region.as_str(), plain.timeout as u64), ("b", "r", 30));
    assert_eq!((timed.bucket.as_str(), timed.region.as_str(), timed.timeout as u64), ("b", "r", 12));
}
"##;

pub(super) const CLI_PROBE: &str = r##"use uploader::cli;

#[test]
fn hidden_retry_default() {
    assert_eq!(cli::parse(&[]).unwrap().retry_count as u64, 4);
}

#[test]
fn hidden_dry_run() {
    assert!(!cli::parse(&[]).unwrap().dry_run);
    assert!(cli::parse(&["--dry-run"]).unwrap().dry_run);
}

#[test]
fn hidden_kebab_case() {
    assert_eq!(cli::parse(&["--retry-count", "2", "--dry-run"]).unwrap().retry_count as u64, 2);
    assert!(cli::parse(&["--retry_count", "2"]).is_err());
    assert!(cli::parse(&["--dry_run"]).is_err());
}
"##;

pub(super) fn three_sessions() -> Task {
    Task {
        id: "three-sessions",
        trap: "three sessions; the third needs a number from the first, and the second a naming rule from the first",
        group: "more",
        files: files(&[("Cargo.toml", &manifest("uploader", "")), ("src/lib.rs", "//! The uploader.\n")]),
        turns: vec![
            (
                1,
                "House rules for this repository: every command-line flag is written in kebab-case, and every \
                 environment variable we read starts with the prefix ACME_. Also note that the uploader retries 4 \
                 times by default.\n\nWrite src/config.rs (declared in src/lib.rs as `pub mod config;`) with a \
                 struct Config with the public String fields bucket and region, and a function \
                 `pub fn load() -> Config` that reads them from the environment variables ACME_BUCKET and \
                 ACME_REGION."
                    .into(),
            ),
            (
                2,
                "Add a public `timeout: u64` field to config::Config, read by load() from the environment \
                 variable named by our prefix rule followed by TIMEOUT, defaulting to 30 when it is not set."
                    .into(),
            ),
            (
                3,
                "Write src/cli.rs (declared in src/lib.rs as `pub mod cli;`) with a struct Options with two \
                 public fields, `retry_count: u32` (the uploader retry count, defaulting to the number I told you \
                 in our first session) and `dry_run: bool` (a switch, off unless given), and a function \
                 `pub fn parse(args: &[&str]) -> Result<Options, String>` that reads them from command-line flags \
                 (args holds the flags only, no program name) and refuses unknown flags. Follow the house rules. \
                 Do not ask me again."
                    .into(),
            ),
        ],
        check: |wd| Outcome::share(sessions_parts(wd)),
        solution: sessions_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

pub(super) fn sessions_parts(wd: &Path) -> Vec<Part> {
    let c = match copy(wd) {
        Ok(c) => c,
        Err(o) => return vec![(false, o.detail)],
    };
    let mut parts = c.hidden("zz_hidden_config", CONFIG_PROBE, &[("hidden_config", "config::load() is wrong")]);
    parts.extend(c.hidden(
        "zz_hidden_cli",
        CLI_PROBE,
        &[
            ("hidden_retry_default", "the retry count does not default to 4"),
            ("hidden_dry_run", "--dry-run does not switch on"),
            ("hidden_kebab_case", "the flags are not --retry-count and --dry-run"),
        ],
    ));
    parts
}

pub(super) fn sessions_solution(wd: &Path) {
    put(wd, "src/lib.rs", "//! The uploader.\n\npub mod cli;\npub mod config;\n");
    put(
        wd,
        "src/config.rs",
        r##"pub struct Config {
    pub bucket: String,
    pub region: String,
    pub timeout: u64,
}

pub fn load() -> Config {
    let var = |k: &str| std::env::var(k).unwrap_or_default();
    Config { bucket: var("ACME_BUCKET"), region: var("ACME_REGION"), timeout: var("ACME_TIMEOUT").parse().unwrap_or(30) }
}
"##,
    );
    put(
        wd,
        "src/cli.rs",
        r##"pub struct Options {
    pub retry_count: u32,
    pub dry_run: bool,
}

pub fn parse(args: &[&str]) -> Result<Options, String> {
    let mut o = Options { retry_count: 4, dry_run: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--retry-count" => o.retry_count = it.next().ok_or("--retry-count needs a value")?.parse().map_err(|_| "bad count")?,
            "--dry-run" => o.dry_run = true,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(o)
}
"##,
    );
}

// ------------------------------------------------------------------ 10. undo one change among several

pub(super) const SHOP: &str = r##"/// Shipping cost of a parcel.
pub fn shipping(weight_kg: f64) -> f64 {
    round2(4.5 + 1.2 * weight_kg)
}

/// Tax on an amount.
pub fn tax(amount: f64) -> f64 {
    round2(amount * 0.18)
}

/// `x` rounded to cents.
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}
"##;

pub(super) fn revert_probe() -> String {
    let ship: Vec<String> = [1.0f64, 10.0, 20.0, 21.0, 35.0]
        .iter()
        .map(|w| format!("({w:?}, {:?})", crate::check::round_to(4.5 + 1.2 * w, 2)))
        .collect();
    format!(
        r##"fn near(a: f64, b: f64) -> bool {{
    (a - b).abs() < 1e-9
}}

#[test]
fn hidden_shipping() {{
    for (w, want) in [{}] {{
        assert!(near(shop::shipping(w), want), "shipping({{w}}) = {{}}, want {{want}}", shop::shipping(w));
    }}
}}

#[test]
fn hidden_tax() {{
    assert!(near(shop::tax(100.0), 21.0), "tax(100) = {{}}", shop::tax(100.0));
}}

#[test]
fn hidden_invoice() {{
    let inv = shop::invoice(&[("a", 2, 10.0), ("b", 1, 6.0)]);
    assert!(near(inv.subtotal, 26.0) && near(inv.tax, 5.46) && near(inv.total, 31.46),
        "invoice: {{}} {{}} {{}}", inv.subtotal, inv.tax, inv.total);
}}
"##,
        ship.join(", ")
    )
}

pub(super) fn selective_revert() -> Task {
    Task {
        id: "selective-revert",
        trap: "the user cancels the first of three edits; the code before it exists only in the history (no git)",
        group: "more",
        files: files(&[("Cargo.toml", &manifest("shop", "")), ("src/lib.rs", SHOP)]),
        turns: vec![
            (
                1,
                "In src/lib.rs, change shipping(): parcels heavier than 20 kg now pay a flat 15.0 surcharge on top \
                 of the current formula, and the cost is never below 6.0."
                    .into(),
            ),
            (1, "In src/lib.rs, the tax rate is now 21% instead of 18%.".into()),
            (
                1,
                "Add invoice(lines) to src/lib.rs: lines is a slice of (name, qty, unit_price) tuples, \
                 `&[(&str, u32, f64)]`; return an Invoice struct with the public f64 fields subtotal (rounded to 2 \
                 decimals), tax (tax() of the subtotal) and total (rounded to 2 decimals). Write a small test for \
                 it in tests/shop.rs and make sure `cargo test` passes."
                    .into(),
            ),
            (
                1,
                "Management cancelled the shipping change from my first request. Undo exactly that change and \
                 nothing else: keep the new tax rate and invoice(). There is no git here, so don't look for it."
                    .into(),
            ),
        ],
        check: |wd| Outcome::share(revert_parts(wd)),
        solution: |wd| {
            let src = SHOP.replace("0.18", "0.21")
                + r##"
/// Subtotal, tax and total of some lines.
pub struct Invoice {
    pub subtotal: f64,
    pub tax: f64,
    pub total: f64,
}

/// The invoice of (name, qty, unit price) lines.
pub fn invoice(lines: &[(&str, u32, f64)]) -> Invoice {
    let subtotal = round2(lines.iter().map(|(_, q, p)| *q as f64 * p).sum());
    let t = tax(subtotal);
    Invoice { subtotal, tax: t, total: round2(subtotal + t) }
}
"##;
            put(wd, "src/lib.rs", &src);
        },
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

pub(super) fn revert_parts(wd: &Path) -> Vec<Part> {
    match copy(wd) {
        Ok(c) => c.hidden(
            "zz_hidden_shop",
            &revert_probe(),
            &[
                ("hidden_shipping", "shipping() is not back to the original"),
                ("hidden_tax", "tax(100) is not 21.0 (the second change was undone too)"),
                ("hidden_invoice", "invoice() is wrong"),
            ],
        ),
        Err(o) => vec![(false, o.detail)],
    }
}

// ------------------------------------------------------------------ 11. a file the user said is out of date

pub(super) const README: &str = r##"# orders service

Built from the service catalogue. Do not edit by hand.

## Deployment

The service runs on host `svc.internal` and listens on port 8080 over plain HTTP.

## Healthcheck

Probe `GET http://<host>:<port>/healthz`. A probe that takes longer than 5 seconds counts as failed.
The response body is JSON with a field `status` that is `"ok"` when the service is healthy.
"##;

pub(super) const LOGPARSE_TESTS: &str = r##"use orders::logparse::parse;

#[test]
fn line() {
    let e = parse("2026-03-01T10:00:00 WARN disk 91%").unwrap();
    assert_eq!(e.time, "2026-03-01T10:00:00");
    assert_eq!(e.level, "WARN");
    assert_eq!(e.message, "disk 91%");
}

#[test]
fn level_upper() {
    assert_eq!(parse("2026-03-01T10:00:01 info started").unwrap().level, "INFO");
}

#[test]
fn bad() {
    assert!(parse("nonsense").is_err());
}
"##;

pub(super) const HEALTH_PROBE: &str = r##"use orders::healthcheck;

#[test]
fn hidden_url() {
    assert_eq!(healthcheck::url(), "http://svc.internal:8443/api/health");
}

#[test]
fn hidden_timeout() {
    assert_eq!(healthcheck::timeout() as u64, 5);
}

#[test]
fn hidden_healthy() {
    assert!(healthcheck::healthy(r#"{"status": "ok"}"#));
    assert!(!healthcheck::healthy(r#"{"status": "degraded"}"#));
}
"##;

pub(super) fn stale_doc() -> Task {
    Task {
        id: "stale-doc",
        trap: "a correction to a file in the workdir is given once, early; later the user asks to follow that file",
        group: "more",
        files: files(&[
            ("Cargo.toml", &manifest("orders", "")),
            ("README.md", README),
            ("src/lib.rs", "//! Helpers of the orders service.\n"),
            ("tests/logparse.rs", LOGPARSE_TESTS),
        ]),
        turns: vec![
            (
                1,
                "Heads up before we start: README.md is out of date in two places. The service now listens on port \
                 8443, and the health path is /api/health. Everything else in it is still right. Don't edit the \
                 README, it is rebuilt from the catalogue.\n\nFirst, write the logparse module (src/logparse.rs, declared in \
                 src/lib.rs) with parse(line) so that `cargo test` passes. Do not change the tests."
                    .into(),
            ),
            (
                1,
                "Now write the healthcheck module (src/healthcheck.rs, declared in src/lib.rs as \
                 `pub mod healthcheck;`) following the README's deployment and healthcheck sections: url() returns \
                 the full probe URL as a String, timeout() the timeout in seconds as an integer, and \
                 healthy(body: &str) -> bool whether a response body means the service is healthy."
                    .into(),
            ),
        ],
        check: |wd| Outcome::share(stale_parts(wd)),
        solution: stale_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

pub(super) fn stale_parts(wd: &Path) -> Vec<Part> {
    let c = match copy(wd) {
        Ok(c) => c,
        Err(o) => return vec![(false, o.detail)],
    };
    let mut parts = c.hidden(
        "zz_hidden_health",
        HEALTH_PROBE,
        &[
            ("hidden_url", "url() is wrong (the README is out of date)"),
            ("hidden_timeout", "timeout() is not 5"),
            ("hidden_healthy", "healthy() is wrong"),
        ],
    );
    parts.push(unchanged(wd, "README.md", README));
    parts.push(c.tests_pass());
    parts
}

pub(super) fn stale_solution(wd: &Path) {
    put(wd, "src/lib.rs", "//! Helpers of the orders service.\n\npub mod healthcheck;\npub mod logparse;\n");
    put(
        wd,
        "src/logparse.rs",
        r##"pub struct Entry {
    pub time: String,
    pub level: String,
    pub message: String,
}

pub fn parse(line: &str) -> Result<Entry, String> {
    let mut parts = line.splitn(3, ' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(t), Some(l), Some(m)) if t.contains('T') => {
            Ok(Entry { time: t.into(), level: l.to_uppercase(), message: m.into() })
        }
        _ => Err(format!("not a log line: {line}")),
    }
}
"##,
    );
    put(
        wd,
        "src/healthcheck.rs",
        r##"pub fn url() -> String {
    "http://svc.internal:8443/api/health".into()
}

pub fn timeout() -> u64 {
    5
}

pub fn healthy(body: &str) -> bool {
    body.replace(' ', "").contains("\"status\":\"ok\"")
}
"##,
    );
}
