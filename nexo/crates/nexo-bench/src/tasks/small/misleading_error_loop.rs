use super::*;

pub(super) const PRICING: &str = r##"//! Gross prices.

pub mod constants;

/// The gross total of a net amount, rounded to cents.
pub fn total(net: f64) -> f64 {
    let rate: f64 = constants::vat_rate().parse().expect("invalid VAT rate");
    (net * (1.0 + rate) * 100.0).round() / 100.0
}
"##;

pub(super) const CONSTANTS: &str = r##"//! Values read from settings.ini.

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

pub(super) const PRICING_TESTS: &str = r##"use pricing::total;

#[test]
fn total_adds_vat() {
    assert_eq!(total(100.0), 121.0);
    assert_eq!(total(10.0), 12.1);
}
"##;

pub(super) fn misleading_error() -> Task {
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
