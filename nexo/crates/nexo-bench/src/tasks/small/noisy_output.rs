use super::*;

pub(super) const APP: &str = r##"use std::process::exit;

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

pub(super) fn noisy_log() -> Task {
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

pub(super) fn noisy_check(wd: &Path) -> Outcome {
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
