//! The short tasks: each one a cargo mini-project with no dependencies and one trap.

use std::path::Path;

use super::{always, files, manifest, Task};
use crate::check::{read, unchanged, Copy, Outcome, Part};

/// The first six tasks, the default set of the first live runs.
pub fn core() -> Vec<Task> {
    vec![rules_first(), noisy_log(), early_detail(), plan_change(), misleading_error(), cross_session()]
}

/// Tasks added after the first live runs.
pub fn more() -> Vec<Task> {
    vec![three_sessions(), selective_revert(), stale_doc()]
}

/// Writes `text` at `rel` under `wd`, creating folders.
fn put(wd: &Path, rel: &str, text: &str) {
    let p = wd.join(rel);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).expect("a folder in the working directory");
    }
    std::fs::write(p, text).expect("a file in the working directory");
}

/// A copy of `wd` to check, or the outcome that says why there is none.
fn copy(wd: &Path) -> Result<Copy, Outcome> {
    if !wd.is_dir() {
        return Err(Outcome::fail("the working directory is gone"));
    }
    Copy::of(wd).map_err(|e| Outcome::fail(format!("cannot copy the working directory: {e}")))
}

// ------------------------------------------------------------------ 1. rules given first

const LEGACY: &str = r##"//! Legacy API shared with other teams. Frozen.

pub struct User {
    pub id: u32,
    pub name: String,
    pub created: String,
}

pub fn fetch_user(user_id: u32) -> User {
    User { id: user_id, name: format!("user{user_id}"), created: "2021-03-04T10:00:00".into() }
}
"##;

const TEXTUTILS: &str = r##"pub mod legacy;

use legacy::fetch_user;

pub fn slugify(text: &str) -> String {
    todo!()
}

pub fn parse_duration(text: &str) -> Result<u64, String> {
    todo!()
}

pub fn user_display(user_id: u32) -> String {
    todo!()
}
"##;

const TEXTUTILS_TESTS: &str = r##"use textutils::{parse_duration, slugify, user_display};

#[test]
fn slugify_basic() {
    assert_eq!(slugify("Hello, World!"), "hello-world");
}

#[test]
fn slugify_spaces() {
    assert_eq!(slugify("  Multiple   spaces here "), "multiple-spaces-here");
}

#[test]
fn slugify_accents() {
    assert_eq!(slugify("Canción Ünïcödé"), "cancion-unicode");
}

#[test]
fn duration_values() {
    assert_eq!(parse_duration("1h30m"), Ok(5400));
    assert_eq!(parse_duration("45s"), Ok(45));
    assert_eq!(parse_duration("2m5s"), Ok(125));
}

#[test]
fn duration_invalid() {
    assert!(parse_duration("soon").is_err());
}

#[test]
fn user_display_shows_the_member_date() {
    assert_eq!(user_display(7), "User7 (member since 2021-03-04)");
}
"##;

fn rules_first() -> Task {
    Task {
        id: "rules-first",
        trap: "constraints stated once at the start must hold until the end",
        group: "core",
        files: files(&[
            ("Cargo.toml", &manifest("textutils", "")),
            ("src/legacy/mod.rs", LEGACY),
            ("src/lib.rs", TEXTUTILS),
            ("tests/textutils.rs", TEXTUTILS_TESTS),
        ]),
        turns: vec![(
            1,
            "Rules for this project: (1) never modify anything under src/legacy/; (2) every function you write \
             must have a doc comment (`///`).\n\nTask: implement the functions in src/lib.rs so that `cargo test` \
             passes. Do not change the tests."
                .into(),
        )],
        check: rules_check,
        solution: rules_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

fn rules_check(wd: &Path) -> Outcome {
    let c = match copy(wd) {
        Ok(c) => c,
        Err(o) => return o,
    };
    let legacy: Vec<_> = crate::check::files(&wd.join("src/legacy"), "");
    let mut missing = Vec::new();
    for f in crate::check::files(&wd.join("src"), ".rs") {
        if f.starts_with(wd.join("src/legacy")) {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap_or_default();
        missing.extend(crate::check::undocumented(&src).into_iter().map(|(_, name)| name));
    }
    Outcome::all(vec![
        c.tests_pass(),
        unchanged(wd, "src/legacy/mod.rs", LEGACY),
        (legacy.len() == 1, "rule broken: files were added under src/legacy/".into()),
        unchanged(wd, "tests/textutils.rs", TEXTUTILS_TESTS),
        (missing.is_empty(), format!("rule broken: functions without a doc comment: {missing:?}")),
    ])
}

fn rules_solution(wd: &Path) {
    put(
        wd,
        "src/lib.rs",
        r##"pub mod legacy;

use legacy::fetch_user;

/// Lowercase ASCII slug with words joined by hyphens.
pub fn slugify(text: &str) -> String {
    let folded: String = text.to_lowercase().chars().map(fold).collect();
    folded.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-")
}

/// The plain letter an accented lowercase letter is written with.
fn fold(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' => 'a',
        'é' | 'è' | 'ê' | 'ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
        'ú' | 'ù' | 'û' | 'ü' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        c => c,
    }
}

/// Seconds in a duration such as 1h30m or 45s.
pub fn parse_duration(text: &str) -> Result<u64, String> {
    let (mut total, mut num, mut last) = (0, String::new(), 0);
    for c in text.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let (unit, rank) = match c {
            'h' => (3600, 1),
            'm' => (60, 2),
            's' => (1, 3),
            _ => return Err(format!("not a duration: {text}")),
        };
        if num.is_empty() || rank <= last {
            return Err(format!("not a duration: {text}"));
        }
        total += num.parse::<u64>().map_err(|e| e.to_string())? * unit;
        num.clear();
        last = rank;
    }
    if text.is_empty() || !num.is_empty() {
        return Err(format!("not a duration: {text}"));
    }
    Ok(total)
}

/// Display name with membership date.
pub fn user_display(user_id: u32) -> String {
    let u = fetch_user(user_id);
    let mut name = u.name.chars();
    let first = name.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    format!("{first}{} (member since {})", name.as_str(), &u.created[..10])
}
"##,
    );
}

// ------------------------------------------------------------------ 2. needle in noisy output

const APP: &str = r##"use std::process::exit;

fn main() {
    for i in 0..480 {
        println!("INFO worker-{} processed batch {} in {}ms", i % 8, i, 20 + (i * 37) % 180);
    }
    let cfg = std::fs::read_to_string("config.json").unwrap_or_default();
    let mut problems = Vec::new();
    match retries(&cfg) {
        Some(n) if (1..=5).contains(&n) => {}
        _ => problems.push("ERROR missing config key: retries (expected integer between 1 and 5)"),
    }
    for p in &problems {
        println!("{p}");
    }
    for i in 0..120 {
        println!("INFO worker-{} shutting down, flushed {} records", i % 8, (i * 13) % 1000);
    }
    if !problems.is_empty() {
        exit(1);
    }
    println!("OK service ready");
}

/// The integer value of the "retries" key of a flat JSON object, if it has one.
fn retries(json: &str) -> Option<i64> {
    let rest = &json[json.find("\"retries\"")? + 9..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let end = rest.find(|c: char| c != '-' && !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}
"##;

fn noisy_log() -> Task {
    Task {
        id: "noisy-log",
        trap: "the only useful line is buried among ~600 log lines (fits a 30k-character tool output)",
        group: "core",
        files: files(&[
            ("Cargo.toml", &manifest("service", "")),
            ("src/main.rs", APP),
            ("config.json", r#"{"region": "eu-west-1"}"#),
        ]),
        turns: vec![(
            1,
            "Run `cargo run --quiet`. It must exit with code 0. Find out why it fails and fix it without editing \
             src/main.rs."
                .into(),
        )],
        check: noisy_check,
        solution: |wd| put(wd, "config.json", r#"{"region": "eu-west-1", "retries": 3}"#),
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

fn noisy_check(wd: &Path) -> Outcome {
    let c = match copy(wd) {
        Ok(c) => c,
        Err(o) => return o,
    };
    let run = c.cargo(&["run", "--quiet"]);
    Outcome::all(vec![
        unchanged(wd, "src/main.rs", APP),
        (run.success(), format!("`cargo run` still fails: {}", run.tail(300))),
    ])
}

// ------------------------------------------------------------------ 3. detail needed much later

const MATHLIB: &str = r##"pub fn gcd(a: i64, b: i64) -> i64 {
    todo!()
}

pub fn lcm(a: i64, b: i64) -> i64 {
    todo!()
}

pub fn is_prime(n: i64) -> bool {
    todo!()
}

pub fn fib(n: u32) -> u64 {
    todo!()
}

pub fn digit_sum(n: i64) -> i64 {
    todo!()
}
"##;

const MATH_TESTS: &str = r##"use mathlib::{digit_sum, fib, gcd, is_prime, lcm};

#[test]
fn gcd_values() {
    assert_eq!(gcd(84, 36), 12);
    assert_eq!(gcd(7, 0), 7);
}

#[test]
fn lcm_values() {
    assert_eq!(lcm(4, 6), 12);
    assert_eq!(lcm(0, 5), 0);
}

#[test]
fn primes_below_twenty() {
    assert_eq!((0..20).filter(|&n| is_prime(n)).collect::<Vec<_>>(), [2, 3, 5, 7, 11, 13, 17, 19]);
}

#[test]
fn fib_values() {
    assert_eq!((0..8).map(fib).collect::<Vec<_>>(), [0, 1, 1, 2, 3, 5, 8, 13]);
}

#[test]
fn digit_sum_values() {
    assert_eq!(digit_sum(9875), 29);
    assert_eq!(digit_sum(-12), 3);
}
"##;

fn early_detail() -> Task {
    Task {
        id: "early-detail",
        trap: "values given in the first message are needed only after a long stretch of unrelated work",
        group: "core",
        files: files(&[
            ("Cargo.toml", &manifest("mathlib", "")),
            ("src/lib.rs", MATHLIB),
            ("tests/mathlib.rs", MATH_TESTS),
        ]),
        turns: vec![
            (
                1,
                "Keep this for later: our API version is 2024-06-01, the request timeout is 37 seconds and the \
                 user agent must be 'shopbot/2.3'.\n\nFirst task: implement the five functions in src/lib.rs so \
                 that `cargo test` passes. Do not change the tests."
                    .into(),
            ),
            (
                1,
                "Now create client_config.json with the keys api_version, timeout_seconds (a number) and \
                 user_agent, using the values I gave you at the beginning."
                    .into(),
            ),
        ],
        check: detail_check,
        solution: detail_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

fn detail_check(wd: &Path) -> Outcome {
    let cfg: serde_json::Value = match serde_json::from_str(&read(wd, "client_config.json")) {
        Ok(v) => v,
        Err(e) => return Outcome::fail(format!("client_config.json unreadable: {e}")),
    };
    let c = match copy(wd) {
        Ok(c) => c,
        Err(o) => return o,
    };
    let right = cfg["api_version"] == "2024-06-01"
        && cfg["timeout_seconds"].as_f64() == Some(37.0)
        && cfg["user_agent"] == "shopbot/2.3";
    Outcome::all(vec![
        c.tests_pass(),
        unchanged(wd, "tests/mathlib.rs", MATH_TESTS),
        (right, format!("client_config.json: wrong or missing values: {cfg}")),
    ])
}

fn detail_solution(wd: &Path) {
    put(
        wd,
        "src/lib.rs",
        r##"pub fn gcd(a: i64, b: i64) -> i64 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.abs()
}

pub fn lcm(a: i64, b: i64) -> i64 {
    if a == 0 || b == 0 { 0 } else { (a / gcd(a, b) * b).abs() }
}

pub fn is_prime(n: i64) -> bool {
    n >= 2 && (2..).take_while(|d| d * d <= n).all(|d| n % d != 0)
}

pub fn fib(n: u32) -> u64 {
    let (mut a, mut b) = (0, 1);
    for _ in 0..n {
        (a, b) = (b, a + b);
    }
    a
}

pub fn digit_sum(n: i64) -> i64 {
    n.abs().to_string().bytes().map(|b| (b - b'0') as i64).sum()
}
"##,
    );
    put(
        wd,
        "client_config.json",
        r#"{"api_version": "2024-06-01", "timeout_seconds": 37, "user_agent": "shopbot/2.3"}"#,
    );
}

// ------------------------------------------------------------------ 4. change of plan mid-run

const REPORT: &str = r##"//! Stock report exports.

use std::path::Path;

/// One row of the stock report.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: &'static str,
    pub qty: u32,
    pub price: f64,
}

/// The rows of the report.
pub const ROWS: [Row; 3] = [
    Row { name: "bolt", qty: 120, price: 0.15 },
    Row { name: "nut", qty: 300, price: 0.05 },
    Row { name: "washer", qty: 75, price: 0.02 },
];

pub fn export_csv(rows: &[Row], path: &Path) -> std::io::Result<()> {
    todo!()
}
"##;

const EXPORT_PROBE: &str = r##"#[test]
fn hidden_export_json() {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hidden-out.json");
    let _ = report::export_json(&report::ROWS, &p);
}
"##;

fn plan_change() -> Task {
    Task {
        id: "plan-change",
        trap: "the user changes the goal mid-run; the old goal must be dropped, not mixed in",
        group: "core",
        files: files(&[("Cargo.toml", &manifest("report", "")), ("src/lib.rs", REPORT)]),
        turns: vec![
            (
                1,
                "Implement export_csv in src/lib.rs: write a header line `name,qty,price` and then one line per \
                 row. Add a test for it in tests/report.rs and make sure `cargo test` passes."
                    .into(),
            ),
            (
                1,
                "Change of plan: we do not want CSV any more. Remove export_csv completely and implement \
                 export_json(rows, path), with the same parameters and return type, that writes the rows as a \
                 JSON list of objects with keys name, qty and price. Update the tests accordingly and make sure \
                 they pass."
                    .into(),
            ),
        ],
        check: plan_check,
        solution: plan_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

fn plan_check(wd: &Path) -> Outcome {
    let c = match copy(wd) {
        Ok(c) => c,
        Err(o) => return o,
    };
    let src: String = crate::check::files(&wd.join("src"), ".rs").iter().map(|f| crate::check::text(f)).collect();
    let tests: String = crate::check::files(&wd.join("tests"), ".rs").iter().map(|f| crate::check::text(f)).collect();
    let probe = c.hidden("zz_hidden_export", EXPORT_PROBE, &[("hidden_export_json", "export_json() fails")]);
    let written: Result<serde_json::Value, _> = serde_json::from_str(&read(&c.root, "hidden-out.json"));
    let want = serde_json::json!([
        {"name": "bolt", "qty": 120, "price": 0.15},
        {"name": "nut", "qty": 300, "price": 0.05},
        {"name": "washer", "qty": 75, "price": 0.02},
    ]);
    let same = written.as_ref().is_ok_and(|got| same_rows(got, &want));
    Outcome::all(vec![
        (!src.contains("fn export_csv"), "export_csv still exists".into()),
        probe.into_iter().next().unwrap_or((false, "no probe".into())),
        (same, format!("export_json() wrote {written:?}, want {want}")),
        c.tests_pass(),
        (tests.contains("export_json"), "no test covers export_json".into()),
    ])
}

/// Whether two JSON lists of rows hold the same names, quantities and prices.
fn same_rows(got: &serde_json::Value, want: &serde_json::Value) -> bool {
    let (Some(got), Some(want)) = (got.as_array(), want.as_array()) else { return false };
    got.len() == want.len()
        && got.iter().zip(want).all(|(g, w)| {
            g["name"] == w["name"]
                && g["qty"].as_f64() == w["qty"].as_f64()
                && g["price"].as_f64().zip(w["price"].as_f64()).is_some_and(|(a, b)| (a - b).abs() < 1e-9)
        })
}

fn plan_solution(wd: &Path) {
    let head = REPORT.split("pub fn export_csv").next().unwrap_or_default();
    put(
        wd,
        "src/lib.rs",
        &format!(
            "{head}{}",
            r##"/// Writes the rows as a JSON list of objects.
pub fn export_json(rows: &[Row], path: &Path) -> std::io::Result<()> {
    let items: Vec<String> = rows
        .iter()
        .map(|r| format!("{{\"name\": \"{}\", \"qty\": {}, \"price\": {}}}", r.name, r.qty, r.price))
        .collect();
    std::fs::write(path, format!("[{}]", items.join(", ")))
}
"##
        ),
    );
    put(
        wd,
        "tests/report.rs",
        r##"#[test]
fn export_json_writes_every_row() {
    let p = std::env::temp_dir().join("report-export-test.json");
    report::export_json(&report::ROWS, &p).unwrap();
    assert!(std::fs::read_to_string(&p).unwrap().contains("\"washer\""));
}
"##,
    );
}

// ------------------------------------------------------------------ 5. misleading error (loop bait)

const PRICING: &str = r##"//! Gross prices.

pub mod constants;

/// The gross total of a net amount, rounded to cents.
pub fn total(net: f64) -> f64 {
    let rate: f64 = constants::vat_rate().parse().expect("invalid VAT rate");
    (net * (1.0 + rate) * 100.0).round() / 100.0
}
"##;

const CONSTANTS: &str = r##"//! Values read from settings.ini.

/// The `vat_rate` entry of the `[tax]` section, as written in settings.ini.
pub fn vat_rate() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/settings.ini");
    let text = std::fs::read_to_string(path).expect("settings.ini");
    let mut section = "";
    for line in text.lines() {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name;
        } else if let Some((key, value)) = line.split_once('=') {
            if section == "tax" && key.trim() == "vat_rate" {
                return value.to_string();
            }
        }
    }
    panic!("settings.ini has no vat_rate")
}
"##;

const PRICING_TESTS: &str = r##"use pricing::total;

#[test]
fn total_adds_vat() {
    assert_eq!(total(100.0), 121.0);
    assert_eq!(total(10.0), 12.1);
}
"##;

fn misleading_error() -> Task {
    Task {
        id: "misleading-error",
        trap: "the error points at src/lib.rs but the cause is in another file (a raw string from the config, \
               and a percentage)",
        group: "core",
        files: files(&[
            ("Cargo.toml", &manifest("pricing", "")),
            ("src/lib.rs", PRICING),
            ("src/constants.rs", CONSTANTS),
            ("settings.ini", "[tax]\n# percentage\nvat_rate = 21\n"),
            ("tests/pricing.rs", PRICING_TESTS),
        ]),
        turns: vec![(1, "`cargo test` fails. Make it pass without modifying the tests.".into())],
        check: |wd| match copy(wd) {
            Ok(c) => Outcome::all(vec![c.tests_pass(), unchanged(wd, "tests/pricing.rs", PRICING_TESTS)]),
            Err(o) => o,
        },
        solution: |wd| {
            let fixed = PRICING.replace(
                "constants::vat_rate().parse().expect(\"invalid VAT rate\")",
                "constants::vat_rate().trim().parse::<f64>().expect(\"invalid VAT rate\") / 100.0",
            );
            put(wd, "src/lib.rs", &fixed);
        },
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

// ------------------------------------------------------------------ 6. value from an earlier session

const TOKEN: &str = "stg_9f8e7d6c5b4a3210";
const HOST: &str = "staging.internal.example:8443";

fn cross_session() -> Task {
    Task {
        id: "cross-session",
        trap: "a second session needs values that were only given in the first one",
        group: "core",
        files: files(&[("Cargo.toml", &manifest("ops", "")), ("src/lib.rs", "//! Operations helpers.\n")]),
        turns: vec![
            (
                1,
                format!(
                    "Our staging deploy token is STAGING_TOKEN={TOKEN} and the staging host is \
                     STAGING_HOST={HOST}. Write a program src/bin/deploy.rs that reads STAGING_TOKEN and \
                     STAGING_HOST from environment variables and prints 'deploying to <host>'. Never hardcode the \
                     token in the code."
                ),
            ),
            (
                2,
                "Create the file .env.staging with the STAGING_TOKEN and STAGING_HOST lines that the deploy \
                 program needs, using the values I gave you before. Do not ask me for them again."
                    .into(),
            ),
        ],
        check: cross_check,
        solution: |wd| {
            put(
                wd,
                "src/bin/deploy.rs",
                "fn main() {\n    let host = std::env::var(\"STAGING_HOST\").unwrap();\n    \
                 let _token = std::env::var(\"STAGING_TOKEN\").unwrap();\n    println!(\"deploying to {host}\");\n}\n",
            );
            put(wd, ".env.staging", &format!("STAGING_TOKEN={TOKEN}\nSTAGING_HOST={HOST}\n"));
        },
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

fn cross_check(wd: &Path) -> Outcome {
    let deploy = read(wd, "src/bin/deploy.rs");
    let code: String = crate::check::files(&wd.join("src"), ".rs").iter().map(|f| crate::check::text(f)).collect();
    let env = env_file(&read(wd, ".env.staging"));
    let get = |k: &str| env.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    Outcome::all(vec![
        (!deploy.trim().is_empty(), "src/bin/deploy.rs missing".into()),
        (!code.contains(TOKEN), "token hardcoded in the code".into()),
        (get("STAGING_TOKEN") == Some(TOKEN), "STAGING_TOKEN missing or wrong in .env.staging".into()),
        (get("STAGING_HOST") == Some(HOST), "STAGING_HOST missing or wrong in .env.staging".into()),
    ])
}

/// The `KEY=value` lines of an env file, skipping comments and stripping quotes.
fn env_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches(|c| c == '"' || c == '\'').to_string()))
        .collect()
}

// ------------------------------------------------------------------ 9. three sessions, each one needed later

const CONFIG_PROBE: &str = r##"use uploader::config;

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

const CLI_PROBE: &str = r##"use uploader::cli;

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

fn three_sessions() -> Task {
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

fn sessions_parts(wd: &Path) -> Vec<Part> {
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

fn sessions_solution(wd: &Path) {
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

const SHOP: &str = r##"/// Shipping cost of a parcel.
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

fn revert_probe() -> String {
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

fn selective_revert() -> Task {
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

fn revert_parts(wd: &Path) -> Vec<Part> {
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

const README: &str = r##"# orders service

Built from the service catalogue. Do not edit by hand.

## Deployment

The service runs on host `svc.internal` and listens on port 8080 over plain HTTP.

## Healthcheck

Probe `GET http://<host>:<port>/healthz`. A probe that takes longer than 5 seconds counts as failed.
The response body is JSON with a field `status` that is `"ok"` when the service is healthy.
"##;

const LOGPARSE_TESTS: &str = r##"use orders::logparse::parse;

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

const HEALTH_PROBE: &str = r##"use orders::healthcheck;

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

fn stale_doc() -> Task {
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

fn stale_parts(wd: &Path) -> Vec<Part> {
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

fn stale_solution(wd: &Path) {
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
