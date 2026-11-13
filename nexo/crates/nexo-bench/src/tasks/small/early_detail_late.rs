use super::*;

pub(super) const MATHLIB: &str = r##"pub fn gcd(a: i64, b: i64) -> i64 {
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

pub(super) const MATH_TESTS: &str = r##"use mathlib::{digit_sum, fib, gcd, is_prime, lcm};

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

pub(super) fn early_detail() -> Task {
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

pub(super) fn detail_check(wd: &Path) -> Outcome {
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

pub(super) fn detail_solution(wd: &Path) {
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
