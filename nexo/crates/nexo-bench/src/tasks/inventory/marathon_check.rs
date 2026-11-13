use super::*;

/// A Rust expression that builds `stock` with the crate's own functions.
pub(super) fn stock_expr(stock: &Plain) -> String {
    let mut s = String::from("{ let mut st = empty_stock(); ");
    for (l, items) in stock {
        for (i, n) in items {
            let _ = write!(s, "add_stock(&mut st, {l:?}, {i:?}, {n}).unwrap(); ");
        }
    }
    s + "st }"
}

/// The hidden test of the marathon: cases drawn like the spec examples, with other seeds. Each test
/// lists the cases it got wrong.
pub(super) fn marathon_probe() -> String {
    let mut r = Rng::new(99);
    let (mut restock, mut reserve, mut tonnes, mut bulk, mut markdown) =
        (String::new(), String::new(), String::new(), String::new(), String::new());
    for _ in 0..12 {
        let items = some_items(&mut r, 0, 5);
        let th = r.int(1, 15);
        let _ = writeln!(restock, "    check(&{items:?}, {th}, &{:?});", ref_restock(&items, th));
        let loc = *r.pick(&LOCS);
        let have: BTreeMap<&str, i64> = r.sample(&ITEMS, 3).into_iter().map(|n| (n, r.int(0, 12))).collect();
        let names: Vec<&str> = have.keys().copied().collect();
        let lines: Vec<(&str, i64)> = (0..3).map(|_| (*r.pick(&names), r.int(1, 6))).collect();
        let want = match ref_reserve(&have, loc, &lines) {
            Ok(a) => format!("Ok(vec!{:?})", a.into_iter().collect::<Vec<_>>()),
            Err(e) => format!("Err({e:?}.to_string())"),
        };
        let have_plain: Plain = [(loc, have.clone())].into_iter().collect();
        let _ = writeln!(reserve, "    check({}, {loc:?}, &{lines:?}, {want});", stock_expr(&have_plain));
        let t = tonnes_text(r.int(1, 80) as f64 / 8.0, &mut r);
        let _ = writeln!(tonnes, "    check({t:?}, {:?});", ref_tonnes(&t));
        let price = r.int(1, 500) as f64 / 100.0;
        let units = if r.chance(0.5) { r.int(1, 99) } else { r.int(100, 3000) };
        let _ = writeln!(bulk, "    check({price:?}, {units}, {:?});", ref_bulk(price, units));
        let st = some_stock(&mut r);
        let _ = writeln!(markdown, "    check({}, {:?});", stock_expr(&st), ref_markdown(&st));
    }
    for (t, kg) in [("12 lb", 5.443), ("500 g", 0.5)] {
        let _ = writeln!(tonnes, "    check({t:?}, {kg:?});");
    }
    let _ = writeln!(bulk, "    check(1.0, 100, 95.0);\n    check(2.0, 1000, 1700.0);");
    let _ = writeln!(restock, "    check(&[], 5, &[]);");
    format!(
        r##"#![allow(unused_imports)]
use inventory::orders::*;
use inventory::planning::*;
use inventory::pricing::*;
use inventory::report::*;
use inventory::stock::*;
use inventory::units::*;
use std::cell::RefCell;

thread_local! {{ static BAD: RefCell<Vec<String>> = RefCell::new(Vec::new()); }}

fn bad(case: String) {{
    BAD.with(|b| b.borrow_mut().push(case));
}}

fn verdict() {{
    let b = BAD.with(|b| b.borrow().clone());
    assert!(b.is_empty(), "{{}} wrong; first ones (args, expected, got): {{}}", b.len(), b[..b.len().min(3)].join(" / "));
}}

fn near(a: f64, b: f64) -> bool {{
    (a - b).abs() < 1e-9
}}

#[test]
fn hidden_restock_plan() {{
    let check = |items: &[(&str, i64)], th: i64, want: &[&str]| {{
        let got = restock_plan(items, th);
        if got != want {{
            bad(format!("{{items:?}} {{th}}, {{want:?}}, {{got:?}}"));
        }}
    }};
{restock}    verdict();
}}

#[test]
fn hidden_reserve() {{
    let check = |mut st: Stock, loc: &str, lines: &[(&str, i64)], want: Result<Vec<(&str, i64)>, String>| {{
        let got = reserve(&mut st, loc, lines).map(|_| {{
            want.as_ref().map(|w| w.iter().map(|(i, _)| (*i, units_at(&st, loc, i))).collect::<Vec<_>>()).unwrap_or_default()
        }});
        if got != want {{
            bad(format!("{{loc}} {{lines:?}}, {{want:?}}, {{got:?}}"));
        }}
    }};
{reserve}    verdict();
}}

#[test]
fn hidden_tonnes() {{
    let check = |t: &str, want: f64| {{
        let got = parse_weight(t);
        if !got.as_ref().is_ok_and(|g| near(*g, want)) {{
            bad(format!("{{t:?}}, {{want}}, {{got:?}}"));
        }}
    }};
{tonnes}    verdict();
}}

#[test]
fn hidden_bulk_price() {{
    let check = |price: f64, units: i64, want: f64| {{
        let got = bulk_price(price, units);
        if !near(got, want) {{
            bad(format!("{{price}} {{units}}, {{want}}, {{got}}"));
        }}
    }};
{bulk}    verdict();
}}

#[test]
fn hidden_markdown_table() {{
    let check = |st: Stock, want: &str| {{
        let got = markdown_table(&st);
        if got != want {{
            bad(format!("{{want:?}}, {{got:?}}"));
        }}
    }};
{markdown}    verdict();
}}
"##
    )
}

pub(super) fn marathon_check(wd: &Path) -> Outcome {
    let c = match Copy::of(wd) {
        Ok(c) if wd.is_dir() => c,
        _ => return Outcome::fail("the working directory is gone"),
    };
    let mut parts = c.hidden(
        "zz_hidden_marathon",
        &marathon_probe(),
        &[
            ("hidden_restock_plan", "restock_plan gives wrong results"),
            ("hidden_reserve", "reserve gives wrong results"),
            ("hidden_tonnes", "parse_weight gives wrong results"),
            ("hidden_bulk_price", "bulk_price gives wrong results"),
            ("hidden_markdown_table", "markdown_table gives wrong results"),
        ],
    );
    let untested: Vec<&str> = [
        ("planning.rs", "restock_plan"),
        ("reserve.rs", "reserve"),
        ("tonnes.rs", "parse_weight"),
        ("markdown.rs", "markdown_table"),
        ("bulk.rs", "bulk_price"),
    ]
    .into_iter()
    .filter(|(f, word)| {
        let t = read(wd, &format!("tests/{f}"));
        !t.contains(word) || !t.contains("#[test]")
    })
    .map(|(f, _)| f)
    .collect();
    parts.push((untested.is_empty(), format!("no tests in {untested:?}")));
    parts.extend(long_parts(wd));
    Outcome::all(parts)
}

pub(super) fn marathon_solution(wd: &Path) {
    long_solution(wd);
    append(
        wd,
        "src/orders.rs",
        r##"
/// Holds the units of `lines` at `location`: all of them, or none and an error.
pub fn reserve(stock: &mut Stock, location: &str, lines: &[(&str, i64)]) -> Result<(), String> {
    let mut need: BTreeMap<&str, i64> = BTreeMap::new();
    for (item, units) in lines {
        *need.entry(item).or_insert(0) += units;
    }
    for (item, units) in &need {
        if crate::stock::units_at(stock, location, item) < *units {
            return Err(format!("not enough {item} at {location}"));
        }
    }
    for (item, units) in need {
        *stock.entry(location.to_string()).or_default().entry(item.to_string()).or_insert(0) -= units;
    }
    Ok(())
}
"##,
    );
    edit(
        wd,
        "src/units.rs",
        "    Err(format!(\"unknown unit",
        "    if t.ends_with('t') {\n        return Ok(round_to(number(\"t\")? * 1000.0, 3));\n    }\n    Err(format!(\"unknown unit",
    );
    append(
        wd,
        "src/report.rs",
        r##"
/// The stock table in Markdown.
pub fn markdown_table(stock: &Stock) -> String {
    let mut rows = vec![format!("| item | {} | total |", LOCATIONS.join(" | ")), "|---".repeat(6) + "|"];
    for item in all_items(stock) {
        let c: Vec<i64> = LOCATIONS.iter().map(|l| units_at(stock, l, &item)).collect();
        let cells: Vec<String> = c.iter().map(|n| n.to_string()).collect();
        rows.push(format!("| {item} | {} | {} |", cells.join(" | "), c.iter().sum::<i64>()));
    }
    rows.join("\n")
}
"##,
    );
    append(
        wd,
        "src/pricing.rs",
        r##"
/// The total of `units` units at `price` after the bulk discount, rounded once to the cent.
pub fn bulk_price(price: f64, units: i64) -> f64 {
    let pct = [(1000, 15), (500, 10), (100, 5)].iter().find(|(n, _)| units >= *n).map_or(0, |(_, p)| *p);
    (price * units as f64 * (100 - pct) as f64 + 0.5) as i64 as f64 / 100.0
}
"##,
    );
    for (f, body) in [
        ("reserve", "use inventory::orders::reserve;\nuse inventory::stock::empty_stock;\n\n#[test]\nfn short() {\n    assert!(reserve(&mut empty_stock(), \"north\", &[(\"bolt\", 1)]).is_err());\n}\n"),
        ("tonnes", "use inventory::units::parse_weight;\n\n#[test]\nfn two() {\n    assert_eq!(parse_weight(\"2 t\"), Ok(2000.0));\n}\n"),
        ("markdown", "use inventory::report::markdown_table;\nuse inventory::stock::empty_stock;\n\n#[test]\nfn empty() {\n    assert_eq!(markdown_table(&empty_stock()).lines().count(), 2);\n}\n"),
        ("bulk", "use inventory::pricing::bulk_price;\n\n#[test]\nfn tier() {\n    assert_eq!(bulk_price(1.0, 100), 95.0);\n}\n"),
    ] {
        std::fs::write(wd.join(format!("tests/{f}.rs")), body).expect("a writable working directory");
    }
}
