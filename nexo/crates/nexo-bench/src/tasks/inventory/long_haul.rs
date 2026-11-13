use super::*;

/// The modules the first request needs changed.
pub(super) const BUGGY: [&str; 3] = ["pricing.rs", "stock.rs", "units.rs"];
/// The tests that fail at the start, sorted.
pub(super) const FAILING: [&str; 5] =
    ["discount", "discounted_price_list", "pounds", "remove_too_many", "transfer_moves_units"];
pub(super) const CODENAME: &str = "amber-falcon";
pub(super) const BUILD: i64 = 4127;
pub(super) const MAINTAINER: &str = "ops@shop.example";

/// The crate as the first request finds it.
pub(super) fn crate_files() -> Vec<(String, String)> {
    let mut out = vec![
        (
            "Cargo.toml".to_string(),
            manifest("inventory", "")
                .replace("[dependencies]", "[[bin]]\nname = \"audit\"\npath = \"tools/audit.rs\"\n\n[dependencies]"),
        ),
        ("src/lib.rs".into(), LIB.into()),
        ("src/stock.rs".into(), STOCK.into()),
        ("src/pricing.rs".into(), PRICING.into()),
        ("src/units.rs".into(), UNITS.into()),
        ("src/orders.rs".into(), ORDERS.into()),
        ("src/report.rs".into(), REPORT.into()),
        ("src/planning.rs".into(), PLANNING.into()),
        ("tools/audit.rs".into(), AUDIT.into()),
    ];
    out.extend(TESTS.iter().map(|(p, t)| (p.to_string(), t.to_string())));
    out
}

pub(super) fn turns() -> [String; 4] {
    [
        format!(
            "Notes for later: the release codename is {CODENAME}, the build number is {BUILD} and the maintainer is \
             {MAINTAINER}.\n\nFirst task: `cargo test` fails. Fix the code in src/ (do not change the tests) until \
             all the tests pass."
        ),
        "Next: add a function `pub fn restock_plan(items: &[(&str, i64)], threshold: i64) -> Vec<String>` to \
         src/planning.rs. items holds (name, qty) pairs; it returns the names of the items whose qty is below \
         threshold, sorted by name. Add a test for it to tests/planning.rs and make sure all the tests pass."
            .into(),
        "Now run `cargo run --quiet --bin audit` and fix every problem it reports, without editing tools/audit.rs, \
         until it exits with code 0. The tests must still pass."
            .into(),
        "Last: write RELEASE.json with the keys codename, build (a number) and maintainer, using the values from my \
         first message; fixed_modules: the sorted list of the file names (like \"name.rs\") of the modules in src/ \
         whose bugs made the tests fail when we started (only the modules that had to change for those failing \
         tests to pass, not files you changed for other reasons); and failing_tests: the sorted names of the test \
         functions that failed in the first test run."
            .into(),
    ]
}

pub(super) fn long_haul() -> Task {
    Task {
        id: "long-haul",
        trap: "four requests in one session, three bugs, an audit whose output is mostly noise, and at the end \
               values and facts from the start: the history outgrows any curated view, so the arms differ in what \
               they send",
        group: "long",
        files: crate_files(),
        turns: turns().into_iter().map(|t| (1, t)).collect(),
        check: |wd| Outcome::all(long_parts(wd)),
        solution: long_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

pub(super) const RESTOCK_PROBE: &str = r##"use inventory::planning::restock_plan;

#[test]
fn hidden_restock_plan() {
    let items = [("nut", 3), ("bolt", 1), ("washer", 9)];
    assert_eq!(restock_plan(&items, 5), vec!["bolt", "nut"]);
    assert!(restock_plan(&items, 1).is_empty());
}
"##;

/// The last segment of a test name as written in RELEASE.json: `stock::remove_too_many (ok)` and
/// `test_remove_too_many` both give `remove_too_many`.
pub(super) fn test_name(raw: &str) -> String {
    let word = raw.split_whitespace().next().unwrap_or("");
    let last = word.rsplit("::").next().unwrap_or(word);
    let last = last.rsplit('.').next().unwrap_or(last);
    last.strip_prefix("test_").unwrap_or(last).to_string()
}

/// Whether RELEASE.json holds the values and facts of the first request, or what is wrong.
pub(super) fn release(wd: &Path) -> Part {
    let rel: serde_json::Value = match serde_json::from_str(&read(wd, "RELEASE.json")) {
        Ok(v) => v,
        Err(e) => return (false, format!("RELEASE.json unreadable: {e}")),
    };
    let names = |key: &str, f: &dyn Fn(&str) -> String| -> Vec<String> {
        let mut v: Vec<String> =
            rel[key].as_array().into_iter().flatten().map(|x| f(x.as_str().unwrap_or(&x.to_string()))).collect();
        v.sort();
        v
    };
    let modules = names("fixed_modules", &|m| m.rsplit(['/', '\\']).next().unwrap_or(m).to_string());
    let tests = names("failing_tests", &test_name);
    let mut wrong = Vec::new();
    if rel["codename"] != CODENAME {
        wrong.push(format!("codename {}", rel["codename"]));
    }
    if rel["build"].as_f64() != Some(BUILD as f64) {
        wrong.push(format!("build {}", rel["build"]));
    }
    if rel["maintainer"] != MAINTAINER {
        wrong.push(format!("maintainer {}", rel["maintainer"]));
    }
    if modules != BUGGY {
        wrong.push(format!("fixed_modules {modules:?}"));
    }
    if tests != FAILING {
        wrong.push(format!("failing_tests {tests:?}"));
    }
    (wrong.is_empty(), format!("RELEASE.json: wrong or missing values: {}", wrong.join("; ")))
}

pub(super) fn long_parts(wd: &Path) -> Vec<Part> {
    let c = match Copy::of(wd) {
        Ok(c) if wd.is_dir() => c,
        _ => return vec![(false, "the working directory is gone".into())],
    };
    let changed: Vec<&str> =
        TESTS.iter().filter(|(p, t)| *p != "tests/planning.rs" && !unchanged(wd, p, t).0).map(|(p, _)| *p).collect();
    let audit = c.cargo(&["run", "--quiet", "--bin", "audit"]);
    let mut parts = vec![(changed.is_empty(), format!("tests were modified: {changed:?}")), c.tests_pass()];
    parts.extend(c.hidden("zz_hidden_restock", RESTOCK_PROBE, &[("hidden_restock_plan", "restock_plan is wrong")]));
    parts.push((read(wd, "tests/planning.rs").contains("restock_plan"), "no test covers restock_plan".into()));
    parts.push(unchanged(wd, "tools/audit.rs", AUDIT));
    parts.push((audit.success(), format!("the audit still fails: {}", audit.tail(300))));
    parts.push(release(wd));
    parts
}

/// Replaces `old` by `new` in the file `rel` under `wd`.
pub(super) fn edit(wd: &Path, rel: &str, old: &str, new: &str) {
    let p = wd.join(rel);
    let src = text(&p);
    assert!(src.contains(old), "{rel} has no {old:?}");
    std::fs::write(p, src.replacen(old, new, 1)).expect("a writable working directory");
}

/// Appends `more` to the file `rel` under `wd`.
pub(super) fn append(wd: &Path, rel: &str, more: &str) {
    let p = wd.join(rel);
    std::fs::write(&p, text(&p) + more).expect("a writable working directory");
}

pub(super) fn long_solution(wd: &Path) {
    edit(
        wd,
        "src/stock.rs",
        "    let have = units_at(stock, location, item);\n",
        "    let have = units_at(stock, location, item);\n    if units > have {\n        return Err(\"not enough stock\".into());\n    }\n",
    );
    edit(wd, "src/pricing.rs", "price * (1.0 - percent)", "price * (1.0 - percent / 100.0)");
    edit(
        wd,
        "src/pricing.rs",
        "pub fn margin",
        "/// The margin of a price over its cost, as a fraction of the price.\npub fn margin",
    );
    edit(wd, "src/units.rs", "KG_PER_LB: f64 = 0.4536;", "KG_PER_LB: f64 = 0.45359237;");
    edit(wd, "src/report.rs", "// TODO: right-align numbers\n", "/// One row of cells, each padded to its width.\n");
    append(
        wd,
        "src/planning.rs",
        r##"
/// The names of the items whose qty is below `threshold`, sorted.
pub fn restock_plan(items: &[(&str, i64)], threshold: i64) -> Vec<String> {
    let mut out: Vec<String> = items.iter().filter(|(_, q)| *q < threshold).map(|(n, _)| n.to_string()).collect();
    out.sort();
    out
}
"##,
    );
    append(
        wd,
        "tests/planning.rs",
        "\n#[test]\nfn restock() {\n    assert_eq!(restock_plan(&[(\"b\", 1), (\"a\", 0)], 2), [\"a\", \"b\"]);\n}\n",
    );
    let rel = serde_json::json!({
        "codename": CODENAME, "build": BUILD, "maintainer": MAINTAINER,
        "fixed_modules": BUGGY, "failing_tests": FAILING,
    });
    std::fs::write(wd.join("RELEASE.json"), rel.to_string()).expect("a writable working directory");
}
