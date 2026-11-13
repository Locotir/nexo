use super::*;

pub(super) const REPORT: &str = r##"//! Stock report exports.

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

pub(super) const EXPORT_PROBE: &str = r##"#[test]
fn hidden_export_json() {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hidden-out.json");
    let _ = report::export_json(&report::ROWS, &p);
}
"##;

pub(super) fn plan_change() -> Task {
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

pub(super) fn plan_check(wd: &Path) -> Outcome {
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
pub(super) fn same_rows(got: &serde_json::Value, want: &serde_json::Value) -> bool {
    let (Some(got), Some(want)) = (got.as_array(), want.as_array()) else { return false };
    got.len() == want.len()
        && got.iter().zip(want).all(|(g, w)| {
            g["name"] == w["name"]
                && g["qty"].as_f64() == w["qty"].as_f64()
                && g["price"].as_f64().zip(w["price"].as_f64()).is_some_and(|(a, b)| (a - b).abs() < 1e-9)
        })
}

pub(super) fn plan_solution(wd: &Path) {
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
