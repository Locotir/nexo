//! The long tasks: one inventory crate with three bugs, a noisy audit and, for the marathon, five
//! specifications with tables of examples. The history grows well past any curated view, and the
//! last request needs values and facts from the very first one.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use super::{always, manifest, Task};
use crate::check::{read, text, unchanged, Copy, Outcome, Part};
use crate::rng::Rng;

/// The long-haul and marathon tasks, and the marathon with twenty times the examples.
pub fn tasks() -> Vec<Task> {
    vec![long_haul(), marathon(), marathon_heavy(), marathon_noisy()]
}

const LIB: &str = r##"//! Stock, prices, units, orders, reports and planning of the warehouse.

pub mod orders;
pub mod planning;
pub mod pricing;
pub mod report;
pub mod stock;
pub mod units;
"##;

const STOCK: &str = r##"//! Stock levels per warehouse: adding, removing, moving and summarising units.

use std::collections::BTreeMap;

/// The warehouses, in the order reports list them.
pub const LOCATIONS: [&str; 4] = ["north", "south", "east", "west"];

/// Units per item per location: `stock[location][item]`.
pub type Stock = BTreeMap<String, BTreeMap<String, i64>>;

/// A stock table with no items.
pub fn empty_stock() -> Stock {
    LOCATIONS.iter().map(|l| (l.to_string(), BTreeMap::new())).collect()
}

/// Add `units` of `item` at `location`; `units` must not be negative.
pub fn add_stock(stock: &mut Stock, location: &str, item: &str, units: i64) -> Result<(), String> {
    if units < 0 {
        return Err("units must not be negative".into());
    }
    *stock.entry(location.to_string()).or_default().entry(item.to_string()).or_insert(0) += units;
    Ok(())
}

/// Remove `units` of `item` at `location`. Stock never goes below zero: removing more than there is
/// returns an error and leaves the stock as it was.
pub fn remove_stock(stock: &mut Stock, location: &str, item: &str, units: i64) -> Result<(), String> {
    let have = units_at(stock, location, item);
    stock.entry(location.to_string()).or_default().insert(item.to_string(), have - units);
    Ok(())
}

/// Units of `item` at `location` (0 if none).
pub fn units_at(stock: &Stock, location: &str, item: &str) -> i64 {
    stock.get(location).and_then(|items| items.get(item)).copied().unwrap_or(0)
}

/// Units of `item` over all locations.
pub fn total_units(stock: &Stock, item: &str) -> i64 {
    LOCATIONS.iter().map(|l| units_at(stock, l, item)).sum()
}

/// The items with at least one unit at `location`, sorted by name.
pub fn items_at(stock: &Stock, location: &str) -> Vec<String> {
    stock.get(location).map(|items| items.iter().filter(|(_, n)| **n > 0).map(|(i, _)| i.clone()).collect()).unwrap_or_default()
}

/// The locations holding at least one unit of `item`, in the order of LOCATIONS.
pub fn locations_with(stock: &Stock, item: &str) -> Vec<&'static str> {
    LOCATIONS.iter().copied().filter(|l| units_at(stock, l, item) > 0).collect()
}

/// The location with the most units of `item` (the first one in LOCATIONS on a tie), or None.
pub fn largest_location(stock: &Stock, item: &str) -> Option<&'static str> {
    let (mut best, mut most) = (None, 0);
    for l in LOCATIONS {
        let n = units_at(stock, l, item);
        if n > most {
            (best, most) = (Some(l), n);
        }
    }
    best
}

/// Move `units` of `item` from `source` to `target`.
pub fn transfer(stock: &mut Stock, item: &str, source: &str, target: &str, units: i64) -> Result<(), String> {
    remove_stock(stock, source, item, units)?;
    add_stock(stock, target, item, units)
}

/// A copy of the stock that later changes do not touch.
pub fn snapshot(stock: &Stock) -> Stock {
    stock.clone()
}

/// `(location, item) -> difference` for every unit count that differs between two snapshots.
pub fn changes(before: &Stock, after: &Stock) -> BTreeMap<(String, String), i64> {
    let mut out = BTreeMap::new();
    for l in LOCATIONS {
        let mut items: Vec<&String> = before.get(l).into_iter().chain(after.get(l)).flat_map(|m| m.keys()).collect();
        items.sort();
        items.dedup();
        for item in items {
            let d = units_at(after, l, item) - units_at(before, l, item);
            if d != 0 {
                out.insert((l.to_string(), item.clone()), d);
            }
        }
    }
    out
}

/// Every item held anywhere, sorted by name.
pub fn all_items(stock: &Stock) -> Vec<String> {
    let mut out: Vec<String> = LOCATIONS.iter().flat_map(|l| items_at(stock, l)).collect();
    out.sort();
    out.dedup();
    out
}

/// `item -> total units` for every item held anywhere.
pub fn summary(stock: &Stock) -> BTreeMap<String, i64> {
    all_items(stock).into_iter().map(|i| (i.clone(), total_units(stock, &i))).collect()
}
"##;

const PRICING: &str = r##"//! Prices: discounts, rounding, taxes and margins. Money is an f64 in euros, rounded to cents.

use std::collections::BTreeMap;

/// The VAT rate.
pub const VAT: f64 = 0.21;

/// A price as a whole number of cents.
pub fn to_cents(price: f64) -> i64 {
    (price * 100.0).round() as i64
}

/// A whole number of cents as a price in euros.
pub fn from_cents(cents: i64) -> f64 {
    cents as f64 / 100.0
}

/// A price rounded to the cent, halves away from zero.
pub fn round_price(price: f64) -> f64 {
    let sign = if price < 0.0 { -1.0 } else { 1.0 };
    sign * from_cents((price.abs() * 100.0 + 0.5) as i64)
}

/// The price after a discount of `percent` per cent (0 to 100), rounded to the cent.
pub fn apply_discount(price: f64, percent: f64) -> Result<f64, String> {
    if !(0.0..=100.0).contains(&percent) {
        return Err("percent must be between 0 and 100".into());
    }
    Ok(round_price(price * (1.0 - percent)))
}

/// The gross price of a net price.
pub fn with_vat(net: f64) -> f64 {
    round_price(net * (1.0 + VAT))
}

/// The net price of a gross price.
pub fn without_vat(gross: f64) -> f64 {
    round_price(gross / (1.0 + VAT))
}

pub fn margin(cost: f64, price: f64) -> f64 {
    if price == 0.0 {
        return 0.0;
    }
    ((price - cost) / price * 10_000.0).round() / 10_000.0
}

/// `name -> discounted price` for a list of (name, price) pairs.
pub fn price_list(items: &[(&str, f64)], percent: f64) -> Result<BTreeMap<String, f64>, String> {
    items.iter().map(|(name, price)| Ok((name.to_string(), apply_discount(*price, percent)?))).collect()
}

/// The (name, price) pair with the lowest price, or None for no items.
pub fn cheapest<'a>(items: &[(&'a str, f64)]) -> Option<(&'a str, f64)> {
    items.iter().copied().fold(None, |best, it| match best {
        Some((_, p)) if p <= it.1 => best,
        _ => Some(it),
    })
}

/// The sum of a list of prices, exact to the cent.
pub fn total(prices: &[f64]) -> f64 {
    from_cents(prices.iter().map(|p| to_cents(*p)).sum())
}
"##;

const UNITS: &str = r##"//! Units of weight and volume used by the suppliers, converted to the metric ones the warehouse uses.

/// Kilograms in a pound.
pub const KG_PER_LB: f64 = 0.4536;
/// Grams in an ounce.
pub const G_PER_OZ: f64 = 28.349523125;
/// Litres in a US gallon.
pub const L_PER_GAL: f64 = 3.785411784;

/// `x` rounded to `digits` decimals.
fn round_to(x: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (x * f).round() / f
}

/// Pounds to kilograms, rounded to the gram.
pub fn lb_to_kg(lb: f64) -> f64 {
    round_to(lb * KG_PER_LB, 3)
}

/// Kilograms to pounds, rounded to three decimals.
pub fn kg_to_lb(kg: f64) -> f64 {
    round_to(kg / KG_PER_LB, 3)
}

/// Ounces to grams, rounded to a tenth of a gram.
pub fn oz_to_g(oz: f64) -> f64 {
    round_to(oz * G_PER_OZ, 1)
}

/// US gallons to litres, rounded to the millilitre.
pub fn gal_to_l(gal: f64) -> f64 {
    round_to(gal * L_PER_GAL, 3)
}

/// A weight such as "12 lb", "3.5kg" or "500 g" in kilograms, rounded to the gram.
pub fn parse_weight(text: &str) -> Result<f64, String> {
    let t = text.trim().to_lowercase().replace(' ', "");
    let number = |suffix: &str| t[..t.len() - suffix.len()].parse::<f64>().map_err(|_| format!("not a weight: {text:?}"));
    if t.ends_with("kg") {
        return Ok(round_to(number("kg")?, 3));
    }
    if t.ends_with("lb") {
        return Ok(lb_to_kg(number("lb")?));
    }
    if t.ends_with('g') {
        return Ok(round_to(number("g")? * 0.001, 3));
    }
    if t.ends_with("oz") {
        return Ok(round_to(oz_to_g(number("oz")?) / 1000.0, 3));
    }
    Err(format!("unknown unit in {text:?}"))
}

/// The heaviest of a list of weights, as given (the first one on a tie), or None for no weights.
pub fn heaviest<'a>(weights: &[&'a str]) -> Result<Option<&'a str>, String> {
    let mut best: Option<(&str, f64)> = None;
    for w in weights {
        let kg = parse_weight(w)?;
        if best.map_or(true, |(_, b)| kg > b) {
            best = Some((w, kg));
        }
    }
    Ok(best.map(|(w, _)| w))
}
"##;

const ORDERS: &str = r##"//! Orders: lines of (item, units, unit price), their totals and their validation against the stock.

use std::collections::BTreeMap;

use crate::stock::{total_units, Stock};

/// One order line: (item, units, unit price).
pub type Line<'a> = (&'a str, i64, f64);

/// `x` rounded to cents.
fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// The total of one order line, rounded to the cent.
pub fn line_total(line: &Line) -> f64 {
    round2(line.1 as f64 * line.2)
}

/// The total of an order.
pub fn order_total(lines: &[Line]) -> f64 {
    round2(lines.iter().map(line_total).sum())
}

/// `item -> units` of an order, adding up repeated items.
pub fn order_units(lines: &[Line]) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    for (item, units, _) in lines {
        *out.entry(item.to_string()).or_insert(0) += units;
    }
    out
}

/// `item -> units short` for the items the stock cannot cover.
pub fn missing(stock: &Stock, lines: &[Line]) -> BTreeMap<String, i64> {
    order_units(lines)
        .into_iter()
        .map(|(item, units)| {
            let short = units - total_units(stock, &item);
            (item, short)
        })
        .filter(|(_, short)| *short > 0)
        .collect()
}

/// Whether the stock covers the whole order.
pub fn can_fulfil(stock: &Stock, lines: &[Line]) -> bool {
    missing(stock, lines).is_empty()
}

/// The problems of an order as a list of messages (empty if none).
pub fn validate(lines: &[Line]) -> Vec<String> {
    let mut problems = Vec::new();
    for (k, (item, units, price)) in lines.iter().enumerate() {
        let k = k + 1;
        if item.is_empty() {
            problems.push(format!("line {k}: no item"));
        }
        if *units <= 0 {
            problems.push(format!("line {k}: units must be positive"));
        }
        if *price < 0.0 {
            problems.push(format!("line {k}: negative price"));
        }
    }
    problems
}
"##;

const REPORT: &str = r##"//! Plain-text reports of the stock and the orders, for the morning e-mail.

use crate::stock::{all_items, units_at, Stock, LOCATIONS};

// TODO: right-align numbers
pub fn format_row(cells: &[String], widths: &[usize]) -> String {
    cells.iter().zip(widths).map(|(c, w)| format!("{c:<w$}")).collect::<Vec<_>>().join("  ")
}

/// One row per item with its units at every location and the total.
pub fn stock_table(stock: &Stock) -> String {
    let mut head = vec!["item".to_string()];
    head.extend(LOCATIONS.iter().map(|l| l.to_string()));
    head.push("total".into());
    let mut rows = vec![head];
    for item in all_items(stock) {
        let counts: Vec<i64> = LOCATIONS.iter().map(|l| units_at(stock, l, &item)).collect();
        let mut row = vec![item.clone()];
        row.extend(counts.iter().map(|n| n.to_string()));
        row.push(counts.iter().sum::<i64>().to_string());
        rows.push(row);
    }
    let widths: Vec<usize> = (0..rows[0].len()).map(|k| rows.iter().map(|r| r[k].len()).max().unwrap_or(0)).collect();
    rows.iter().map(|r| format_row(r, &widths)).collect::<Vec<_>>().join("\n")
}

/// A one-line summary: how many items and units are held.
pub fn headline(stock: &Stock) -> String {
    let items = all_items(stock);
    let units: i64 = items.iter().flat_map(|i| LOCATIONS.iter().map(move |l| units_at(stock, l, i))).sum();
    format!("{} items, {units} units in {} locations", items.len(), LOCATIONS.len())
}
"##;

const PLANNING: &str = r##"//! Planning: what to reorder and when.

/// Whole days the units last at a daily use (None if nothing is used).
pub fn days_left(units: i64, daily_use: i64) -> Option<i64> {
    if daily_use <= 0 {
        return None;
    }
    Some(units / daily_use)
}

/// The stock level at which an order must be placed to arrive before the stock runs out.
pub fn reorder_point(daily_use: i64, lead_days: i64, safety: i64) -> i64 {
    daily_use * lead_days + safety
}
"##;

const AUDIT: &str = r##"//! Audit of the inventory crate: every function documented, no leftover TODO comments.
//! Prints what it checks (a lot) and exits with 1 if anything is wrong.

use std::path::Path;
use std::process::exit;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut problems = Vec::new();
    for shard in 0..160u64 {
        println!("INFO audit shard {shard:03}: cache warm, {} entries, {} ms", 40 + (shard * 7) % 90, 3 + shard % 11);
    }
    let mut paths: Vec<_> = std::fs::read_dir(&root)
        .expect("the src folder")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    paths.sort();
    for path in paths {
        let src = std::fs::read_to_string(&path).expect("a source file");
        let rel = format!("src/{}", path.file_name().unwrap_or_default().to_string_lossy());
        let lines: Vec<&str> = src.lines().collect();
        let fns: Vec<(usize, String)> =
            lines.iter().enumerate().filter_map(|(k, l)| fn_name(l).map(|n| (k, n))).collect();
        println!("INFO {rel}: {} lines, {} functions", lines.len(), fns.len());
        for (k, name) in &fns {
            let at = k + 1;
            println!("INFO {rel}:{at} {name}: {} lines, {} args, nesting ok", length(&lines, *k), args(lines[*k]));
            println!("INFO {rel}:{at} {name}: names ok, returns ok, no globals written");
            if !documented(&lines, *k) {
                problems.push(format!("WARN {rel}:{at} function {name} has no doc comment"));
            }
        }
        for (k, line) in lines.iter().enumerate() {
            if line.contains("TODO") {
                problems.push(format!("WARN {rel}:{} leftover TODO comment", k + 1));
            }
        }
    }
    for shard in 0..160u64 {
        println!("INFO audit shard {shard:03}: flushed, checksum {:08x}", (shard * 2654435761) % 4294967296);
    }
    for p in &problems {
        println!("{p}");
    }
    for shard in 0..60 {
        println!("INFO audit shard {shard:03}: closed");
    }
    if !problems.is_empty() {
        println!("audit failed: {} problems", problems.len());
        exit(1);
    }
    println!("audit passed");
}

/// The name of the function a line declares, if it starts one.
fn fn_name(line: &str) -> Option<String> {
    let mut t = line.trim_start();
    for prefix in ["pub(crate) ", "pub ", "const "] {
        t = t.strip_prefix(prefix).unwrap_or(t);
    }
    let name: String = t.strip_prefix("fn ")?.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

/// Whether the function on line `k` has a `///` comment above it (attributes allowed between).
fn documented(lines: &[&str], k: usize) -> bool {
    lines[..k].iter().rev().map(|l| l.trim()).find(|l| !l.starts_with("#[")).is_some_and(|l| l.starts_with("///"))
}

/// The lines of the function starting on line `k`, up to its closing brace.
fn length(lines: &[&str], k: usize) -> usize {
    let indent = lines[k].len() - lines[k].trim_start().len();
    let close = format!("{}}}", " ".repeat(indent));
    lines[k..].iter().position(|l| *l == close).map_or(1, |n| n + 1)
}

/// The number of parameters on a signature line.
fn args(line: &str) -> usize {
    let inner = line.split_once('(').map_or("", |(_, r)| r.split(')').next().unwrap_or(""));
    if inner.trim().is_empty() {
        0
    } else {
        inner.matches(',').count() + 1
    }
}
"##;

const TEST_STOCK: &str = r##"use inventory::stock::*;

fn start() -> Stock {
    let mut st = empty_stock();
    add_stock(&mut st, "north", "bolt", 10).unwrap();
    add_stock(&mut st, "south", "bolt", 5).unwrap();
    add_stock(&mut st, "east", "nut", 7).unwrap();
    st
}

#[test]
fn totals() {
    let st = start();
    assert_eq!(total_units(&st, "bolt"), 15);
    assert_eq!(summary(&st), [("bolt".to_string(), 15), ("nut".to_string(), 7)].into_iter().collect());
}

#[test]
fn remove() {
    let mut st = start();
    remove_stock(&mut st, "north", "bolt", 4).unwrap();
    assert_eq!(units_at(&st, "north", "bolt"), 6);
}

#[test]
fn remove_too_many() {
    let mut st = start();
    assert!(remove_stock(&mut st, "south", "bolt", 6).is_err());
    assert_eq!(units_at(&st, "south", "bolt"), 5);
}

#[test]
fn transfer_moves_units() {
    let mut st = start();
    transfer(&mut st, "bolt", "north", "west", 3).unwrap();
    assert_eq!(locations_with(&st, "bolt"), ["north", "south", "west"]);
    assert!(transfer(&mut st, "nut", "east", "west", 8).is_err());
    assert_eq!(units_at(&st, "west", "nut"), 0);
}

#[test]
fn changes_between_snapshots() {
    let mut st = start();
    let before = snapshot(&st);
    add_stock(&mut st, "west", "nut", 2).unwrap();
    assert_eq!(changes(&before, &st), [(("west".to_string(), "nut".to_string()), 2)].into_iter().collect());
}
"##;

const TEST_PRICING: &str = r##"use inventory::pricing::*;

#[test]
fn round() {
    assert_eq!(round_price(2.346), 2.35);
    assert_eq!(round_price(-2.344), -2.34);
}

#[test]
fn discount() {
    assert_eq!(apply_discount(80.0, 25.0), Ok(60.0));
    assert_eq!(apply_discount(19.99, 10.0), Ok(17.99));
    assert_eq!(apply_discount(5.0, 0.0), Ok(5.0));
    assert!(apply_discount(5.0, 120.0).is_err());
}

#[test]
fn discounted_price_list() {
    let got = price_list(&[("bolt", 0.2), ("nut", 0.1)], 50.0).unwrap();
    assert_eq!(got, [("bolt".to_string(), 0.1), ("nut".to_string(), 0.05)].into_iter().collect());
}

#[test]
fn vat() {
    assert_eq!(with_vat(100.0), 121.0);
    assert_eq!(without_vat(121.0), 100.0);
}

#[test]
fn total_is_exact_to_the_cent() {
    assert_eq!(total(&[0.1, 0.2, 0.3]), 0.6);
}
"##;

const TEST_UNITS: &str = r##"use inventory::units::*;

#[test]
fn pounds() {
    assert_eq!(lb_to_kg(100.0), 45.359);
    assert_eq!(kg_to_lb(45.359237), 100.0);
}

#[test]
fn parse() {
    assert_eq!(parse_weight("12 lb"), Ok(5.443));
    assert_eq!(parse_weight("3.5kg"), Ok(3.5));
    assert_eq!(parse_weight("500 g"), Ok(0.5));
    assert_eq!(heaviest(&["1 lb", "450 g", "16 oz"]), Ok(Some("1 lb")));
}

#[test]
fn other() {
    assert_eq!(oz_to_g(1.0), 28.3);
    assert_eq!(gal_to_l(1.0), 3.785);
}
"##;

const TEST_ORDERS: &str = r##"use inventory::orders::*;
use inventory::stock::{add_stock, empty_stock};

#[test]
fn totals() {
    let lines = [("bolt", 10, 0.15), ("nut", 3, 0.05)];
    assert_eq!(order_total(&lines), 1.65);
    let more = [("bolt", 10, 0.15), ("nut", 3, 0.05), ("bolt", 2, 0.15)];
    assert_eq!(order_units(&more), [("bolt".to_string(), 12), ("nut".to_string(), 3)].into_iter().collect());
}

#[test]
fn against_stock() {
    let mut st = empty_stock();
    add_stock(&mut st, "north", "bolt", 5).unwrap();
    assert_eq!(missing(&st, &[("bolt", 8, 0.15)]), [("bolt".to_string(), 3)].into_iter().collect());
    assert!(!can_fulfil(&st, &[("bolt", 8, 0.15)]));
}

#[test]
fn validation() {
    assert_eq!(validate(&[("", 0, -1.0)]), ["line 1: no item", "line 1: units must be positive", "line 1: negative price"]);
}
"##;

const TEST_REPORT: &str = r##"use inventory::report::*;
use inventory::stock::{add_stock, empty_stock};

#[test]
fn table() {
    let mut st = empty_stock();
    add_stock(&mut st, "north", "bolt", 5).unwrap();
    let table = stock_table(&st);
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[1].starts_with("bolt"));
    assert_eq!(headline(&st), "1 items, 5 units in 4 locations");
}
"##;

const TEST_PLANNING: &str = r##"use inventory::planning::*;

#[test]
fn days() {
    assert_eq!(days_left(10, 3), Some(3));
    assert_eq!(days_left(10, 0), None);
}

#[test]
fn reorder() {
    assert_eq!(reorder_point(4, 5, 6), 26);
}
"##;

/// The test files and their original text.
const TESTS: [(&str, &str); 6] = [
    ("tests/stock.rs", TEST_STOCK),
    ("tests/pricing.rs", TEST_PRICING),
    ("tests/units.rs", TEST_UNITS),
    ("tests/orders.rs", TEST_ORDERS),
    ("tests/report.rs", TEST_REPORT),
    ("tests/planning.rs", TEST_PLANNING),
];

/// The modules the first request needs changed.
const BUGGY: [&str; 3] = ["pricing.rs", "stock.rs", "units.rs"];
/// The tests that fail at the start, sorted.
const FAILING: [&str; 5] = ["discount", "discounted_price_list", "pounds", "remove_too_many", "transfer_moves_units"];
const CODENAME: &str = "amber-falcon";
const BUILD: i64 = 4127;
const MAINTAINER: &str = "ops@shop.example";

/// The crate as the first request finds it.
fn crate_files() -> Vec<(String, String)> {
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

fn turns() -> [String; 4] {
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

fn long_haul() -> Task {
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

const RESTOCK_PROBE: &str = r##"use inventory::planning::restock_plan;

#[test]
fn hidden_restock_plan() {
    let items = [("nut", 3), ("bolt", 1), ("washer", 9)];
    assert_eq!(restock_plan(&items, 5), vec!["bolt", "nut"]);
    assert!(restock_plan(&items, 1).is_empty());
}
"##;

/// The last segment of a test name as written in RELEASE.json: `stock::remove_too_many (ok)` and
/// `test_remove_too_many` both give `remove_too_many`.
fn test_name(raw: &str) -> String {
    let word = raw.split_whitespace().next().unwrap_or("");
    let last = word.rsplit("::").next().unwrap_or(word);
    let last = last.rsplit('.').next().unwrap_or(last);
    last.strip_prefix("test_").unwrap_or(last).to_string()
}

/// Whether RELEASE.json holds the values and facts of the first request, or what is wrong.
fn release(wd: &Path) -> Part {
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

fn long_parts(wd: &Path) -> Vec<Part> {
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
fn edit(wd: &Path, rel: &str, old: &str, new: &str) {
    let p = wd.join(rel);
    let src = text(&p);
    assert!(src.contains(old), "{rel} has no {old:?}");
    std::fs::write(p, src.replacen(old, new, 1)).expect("a writable working directory");
}

/// Appends `more` to the file `rel` under `wd`.
fn append(wd: &Path, rel: &str, more: &str) {
    let p = wd.join(rel);
    std::fs::write(&p, text(&p) + more).expect("a writable working directory");
}

fn long_solution(wd: &Path) {
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

// ------------------------------------------------------------------ the marathon: the same crate, nine requests

const ITEMS: [&str; 12] =
    ["bolt", "nut", "washer", "screw", "rivet", "hinge", "spring", "clamp", "gear", "pin", "bracket", "anchor"];
const LOCS: [&str; 4] = ["north", "south", "east", "west"];
const TIERS: [(i64, i64); 3] = [(1000, 15), (500, 10), (100, 5)];

/// A stock as the specifications and hidden cases write it: location -> item -> units.
type Plain = BTreeMap<&'static str, BTreeMap<&'static str, i64>>;

fn ref_restock(items: &[(&str, i64)], threshold: i64) -> Vec<String> {
    let mut out: Vec<String> = items.iter().filter(|(_, q)| *q < threshold).map(|(n, _)| n.to_string()).collect();
    out.sort();
    out
}

/// The units left at `location` after the reservation, or the error message.
fn ref_reserve(
    have: &BTreeMap<&'static str, i64>,
    location: &str,
    lines: &[(&'static str, i64)],
) -> Result<BTreeMap<&'static str, i64>, String> {
    let mut need: BTreeMap<&str, i64> = BTreeMap::new();
    for (item, units) in lines {
        *need.entry(item).or_insert(0) += units;
    }
    for (item, units) in &need {
        if have.get(item).copied().unwrap_or(0) < *units {
            return Err(format!("not enough {item} at {location}"));
        }
    }
    let mut after = have.clone();
    for (item, units) in need {
        *after.entry(item).or_insert(0) -= units;
    }
    Ok(after)
}

fn ref_tonnes(text: &str) -> f64 {
    let t = text.trim().to_lowercase().replace(' ', "");
    crate::check::round_to(t[..t.len() - 1].parse::<f64>().unwrap_or(0.0) * 1000.0, 3)
}

fn ref_markdown(stock: &Plain) -> String {
    let mut items: Vec<&str> =
        stock.values().flat_map(|m| m.iter().filter(|(_, n)| **n > 0).map(|(i, _)| *i)).collect();
    items.sort();
    items.dedup();
    let mut rows =
        vec![format!("| item | {} | total |", LOCS.join(" | ")), format!("{}|", "|---".repeat(LOCS.len() + 2))];
    for i in items {
        let c: Vec<i64> = LOCS.iter().map(|l| stock.get(l).and_then(|m| m.get(i)).copied().unwrap_or(0)).collect();
        let cells: Vec<String> = c.iter().map(|n| n.to_string()).collect();
        rows.push(format!("| {i} | {} | {} |", cells.join(" | "), c.iter().sum::<i64>()));
    }
    rows.join("\n")
}

fn ref_bulk(price: f64, units: i64) -> f64 {
    let pct = TIERS.iter().find(|(n, _)| units >= *n).map_or(0, |(_, p)| *p);
    (price * units as f64 * (100 - pct) as f64 + 0.5) as i64 as f64 / 100.0
}

fn some_items(r: &mut Rng, lo: i64, hi: i64) -> Vec<(&'static str, i64)> {
    let k = r.int(lo, hi) as usize;
    r.sample(&ITEMS, k).into_iter().map(|n| (n, r.int(0, 20))).collect()
}

fn some_stock(r: &mut Rng) -> Plain {
    LOCS.iter().map(|l| (*l, r.sample(&ITEMS, 2).into_iter().map(|n| (n, r.int(0, 9))).collect())).collect()
}

fn ex_restock(r: &mut Rng) -> String {
    let items = some_items(r, 2, 5);
    let th = r.int(1, 15);
    format!("| `{items:?}` | {th} | `{:?}` |", ref_restock(&items, th))
}

fn ex_reserve(r: &mut Rng) -> String {
    let loc = *r.pick(&LOCS);
    let have: BTreeMap<&str, i64> = r.sample(&ITEMS, 3).into_iter().map(|n| (n, r.int(0, 12))).collect();
    let names: Vec<&str> = have.keys().copied().collect();
    let lines: Vec<(&str, i64)> = r.sample(&names, 2).into_iter().map(|n| (n, r.int(1, 6))).collect();
    let after = match ref_reserve(&have, loc, &lines) {
        Ok(a) => format!("{a:?}"),
        Err(e) => format!("Err({e:?})"),
    };
    format!("| {loc} | `{have:?}` | `{lines:?}` | `{after}` |")
}

fn tonnes_text(v: f64, r: &mut Rng) -> String {
    let sp = if r.chance(0.5) { " " } else { "" };
    format!("{v}{sp}t")
}

fn ex_tonnes(r: &mut Rng) -> String {
    let v = match r.below(3) {
        0 => r.int(1, 40) as f64 / 4.0,
        1 => r.int(1, 9) as f64,
        _ => r.int(1, 999) as f64 / 1000.0,
    };
    let t = tonnes_text(v, r);
    format!("| `{t}` | {} |", ref_tonnes(&t))
}

fn bulk_units(r: &mut Rng) -> i64 {
    match r.below(4) {
        0 => r.int(1, 99),
        1 => r.int(100, 499),
        2 => r.int(500, 999),
        _ => r.int(1000, 3000),
    }
}

fn ex_bulk(r: &mut Rng) -> String {
    let price = r.int(1, 500) as f64 / 100.0;
    let units = bulk_units(r);
    format!("| {price} | {units} | {} |", ref_bulk(price, units))
}

fn ex_markdown(r: &mut Rng) -> String {
    let st = some_stock(r);
    format!("| `{st:?}` | `{:?}` |", ref_markdown(&st))
}

fn rows(n: usize, seed: u64, make: fn(&mut Rng) -> String) -> Vec<String> {
    let mut r = Rng::new(seed);
    (0..n).map(|_| make(&mut r)).collect()
}

const NOTE: &str = "This specification was agreed with the warehouse team after the spring stock-take, when several \
                    manual spreadsheets disagreed with the system. The wording of each rule is deliberate: when the \
                    code and this page disagree, the page wins and the code is a bug. Questions go to the maintainer \
                    named in the release notes, not to the warehouse staff.";

fn spec(title: &str, purpose: &str, interface: &str, rules: &[&str], head: &str, rows: Vec<String>) -> String {
    let mut s = format!("# {title}\n\n## Purpose\n\n{purpose}\n\n## Interface\n\n{interface}\n\n## Rules\n\n");
    for (k, r) in rules.iter().enumerate() {
        let _ = writeln!(s, "{}. {r}", k + 1);
    }
    let _ = write!(
        s,
        "\n## Examples\n\nEvery row must hold for the implementation (and is a good source for tests).\n\n{head}\n{}\n\n\
         ## Notes for reviewers\n\n{NOTE}\n",
        rows.join("\n")
    );
    s
}

fn specs() -> Vec<(String, String)> {
    specs_scaled(1)
}

/// The five specifications with `k` times the rows of examples each.
fn specs_scaled(k: usize) -> Vec<(String, String)> {
    vec![
        (
            "docs/spec/01-restock.md".into(),
            spec(
                "Restock plan",
                "Tell the buyers which items to reorder this week.",
                "`pub fn restock_plan(items: &[(&str, i64)], threshold: i64) -> Vec<String>` in `src/planning.rs`. \
                 `items` holds `(name, qty)` pairs; returns a list of names.",
                &[
                    "An item is in the plan when its `qty` is strictly below `threshold`.",
                    "The plan is sorted by name, ascending.",
                    "An empty list of items gives an empty plan.",
                ],
                "| items | threshold | plan |\n|---|---|---|",
                rows(60 * k, 1, ex_restock),
            ),
        ),
        (
            "docs/spec/02-reserve.md".into(),
            spec(
                "Reservations",
                "Hold the units of an order at one location before it is picked.",
                "`pub fn reserve(stock: &mut Stock, location: &str, lines: &[(&str, i64)]) -> Result<(), String>` in \
                 `src/orders.rs`. `stock` is a table like the one `stock::empty_stock()` returns, `lines` a list of \
                 `(item, units)` pairs.",
                &[
                    "All or nothing: if the location cannot cover every line, return an error and leave the stock as \
                     it was.",
                    "Repeated items in `lines` add up before the check.",
                    "The error message is `not enough <item> at <location>` for the first short item in name order.",
                    "On success the units are removed from that location only.",
                ],
                "| location | stock at location | lines | stock at location after |\n|---|---|---|---|",
                rows(60 * k, 2, ex_reserve),
            ),
        ),
        (
            "docs/spec/03-tonnes.md".into(),
            spec(
                "Tonnes",
                "Some suppliers now give weights in metric tonnes.",
                "`parse_weight(text)` in `src/units.rs` accepts the unit `t` (1 t = 1000 kg) besides the units it \
                 already knows, with or without a space, and returns kilograms rounded to three decimals.",
                &[
                    "`t` means metric tonnes.",
                    "The units already supported keep working exactly as before.",
                    "`heaviest` works with tonnes too, since it uses `parse_weight`.",
                ],
                "| text | kilograms |\n|---|---|",
                rows(60 * k, 3, ex_tonnes),
            ),
        ),
        (
            "docs/spec/04-markdown.md".into(),
            spec(
                "Markdown stock table",
                "The morning e-mail is now written in Markdown.",
                "`pub fn markdown_table(stock: &Stock) -> String` in `src/report.rs` returns the table, lines joined \
                 by `\\n`.",
                &[
                    "First line: `| item | north | south | east | west | total |`.",
                    "Second line: `|---|---|---|---|---|---|`.",
                    "Then one line per item held anywhere, sorted by name, like `| bolt | 5 | 0 | 0 | 2 | 7 |`.",
                    "No trailing newline.",
                ],
                "| stock | table |\n|---|---|",
                rows(20 * k, 4, ex_markdown),
            ),
        ),
        (
            "docs/spec/05-bulk.md".into(),
            spec(
                "Bulk prices",
                "Large orders get a discount by quantity.",
                "`pub fn bulk_price(price: f64, units: i64) -> f64` in `src/pricing.rs`: the total for `units` units \
                 at the unit `price`, after the bulk discount, rounded to the cent (halves up).",
                &[
                    "1000 units or more: 15% off.",
                    "500 to 999 units: 10% off.",
                    "100 to 499 units: 5% off.",
                    "Fewer than 100 units: no discount.",
                    "Round the total once, at the end.",
                ],
                "| price | units | total |\n|---|---|---|",
                rows(60 * k, 5, ex_bulk),
            ),
        ),
    ]
}

fn marathon_turns() -> Vec<String> {
    let [first, _, audit, last] = turns();
    vec![
        first,
        "Implement docs/spec/01-restock.md. Add tests for it to tests/planning.rs and make sure all the tests pass."
            .into(),
        audit,
        "Implement docs/spec/02-reserve.md, with tests in a new file tests/reserve.rs; all the tests must pass.".into(),
        "Implement docs/spec/03-tonnes.md, with tests in a new file tests/tonnes.rs; all the tests must pass.".into(),
        "Implement docs/spec/04-markdown.md, with tests in a new file tests/markdown.rs; all the tests must pass."
            .into(),
        "Implement docs/spec/05-bulk.md, with tests in a new file tests/bulk.rs; all the tests must pass.".into(),
        "Run `cargo run --quiet --bin audit` and the tests again and fix whatever they report.".into(),
        last,
    ]
}

fn marathon() -> Task {
    let mut files = crate_files();
    files.extend(specs());
    Task {
        id: "marathon",
        trap: "nine requests in one session: three bugs, an audit, five specifications to read and implement (with \
               tables of examples), then values and facts from the very start; the history grows well past a \
               curated view",
        group: "long",
        files,
        turns: marathon_turns().into_iter().map(|t| (1, t)).collect(),
        check: marathon_check,
        solution: marathon_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

/// The marathon with twenty times the rows of examples in each specification: reading them takes
/// the history well past 200k tokens, so a summary at 100k or 200k falls in the middle of the work
/// and the last request still needs what the first one found.
fn marathon_heavy() -> Task {
    let mut files = crate_files();
    files.extend(specs_scaled(20));
    Task {
        id: "marathon-heavy",
        trap: "the marathon with twenty times the examples in each specification: the history grows past 200k \
               tokens, so a summary falls in the middle of the work",
        group: "heavy",
        files,
        turns: marathon_turns().into_iter().map(|t| (1, t)).collect(),
        check: marathon_check,
        solution: marathon_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

// ------------------------------------------------------------------ the noisy marathon: the heavy one with junk

/// The CI run that really failed, among twelve, and its job id.
const CI_RUN: usize = 7;
const CI_STEPS: [&str; 10] = [
    "checkout",
    "cache-restore",
    "fetch-deps",
    "build",
    "lint",
    "unit-tests",
    "migrate-db",
    "integration",
    "package",
    "upload",
];

/// The job id of CI run `n` (1-based); twelve distinct ids.
fn ci_job(n: usize) -> String {
    let mut rng = Rng::new(9000 + n as u64);
    format!("j-{:06x}", rng.next_u64() % 0xff_ffff)
}

/// One night's CI log: ~500 lines of steps, timings, downloads and retries. Most runs hit errors that a
/// retry fixed; run `CI_RUN` fails one step three times and is aborted.
fn ci_log(n: usize) -> String {
    let mut rng = Rng::new(7000 + n as u64);
    let crates = ["serde", "tokio", "regex", "chrono", "rand", "log", "anyhow", "itertools", "uuid", "csv", "bytes"];
    let tests = ["stock_totals", "price_rounding", "unit_parse", "order_split", "report_rows", "plan_threshold"];
    let job = ci_job(n);
    let mut out = format!("== CI run {n:02} | job {job} | runner ci-{} | branch main ==\n", rng.int(10, 99));
    let (mut h, mut m, mut s) = (2, rng.int(0, 50), 0);
    let mut clock = |rng: &mut Rng| {
        s += rng.int(0, 9);
        m += s / 60;
        s %= 60;
        h += m / 60;
        m %= 60;
        format!("[{h:02}:{m:02}:{s:02}.{:03}]", rng.int(0, 999))
    };
    let flaky = if n == CI_RUN { "migrate-db" } else { CI_STEPS[4 + rng.below(4)] };
    for step in CI_STEPS {
        let _ = writeln!(out, "{} {step:<13}| step started", clock(&mut rng));
        for _ in 0..rng.int(35, 55) {
            let t = clock(&mut rng);
            let line = match rng.below(7) {
                0 => format!(
                    "Downloaded {} v0.{}.{} ({} KB)",
                    rng.pick(&crates),
                    rng.int(1, 40),
                    rng.int(0, 9),
                    rng.int(8, 900)
                ),
                1 => format!("Compiling {} v1.{}.{}", rng.pick(&crates), rng.int(0, 30), rng.int(0, 9)),
                2 => format!("test {} ... ok ({} ms)", rng.pick(&tests), rng.int(1, 4000)),
                3 => format!("WARN slow request to cache.internal: {} ms, retrying (attempt 1/3)", rng.int(900, 9000)),
                4 => format!(
                    "cache key inventory-{:08x} restored ({} MB)",
                    rng.next_u64() % 0xffff_ffff,
                    rng.int(1, 300)
                ),
                5 => format!("WARN deprecated runner image ubuntu-{}.04, will be removed", rng.int(18, 22)),
                _ => format!("debug: env CI_NODE={} CI_SHARD={}/8 RUST_LOG=info", rng.int(1, 40), rng.int(1, 8)),
            };
            let _ = writeln!(out, "{t} {step:<13}| {line}");
        }
        if step == flaky {
            let attempts = if n == CI_RUN { 3 } else { 1 };
            for a in 1..=attempts {
                let _ = writeln!(
                    out,
                    "{} {step:<13}| ERROR step failed: exit code {} (attempt {a}/3)",
                    clock(&mut rng),
                    rng.int(1, 3)
                );
            }
            if n == CI_RUN {
                let _ = writeln!(
                    out,
                    "{} {step:<13}| job {job} aborted: step {step} failed after 3 attempts",
                    clock(&mut rng)
                );
                for rest in CI_STEPS.iter().skip_while(|s| **s != step).skip(1) {
                    let _ = writeln!(out, "{} {rest:<13}| skipped", clock(&mut rng));
                }
                return out;
            }
            let _ = writeln!(out, "{} {step:<13}| step passed (attempt 2/3)", clock(&mut rng));
        } else {
            let _ = writeln!(out, "{} {step:<13}| step passed", clock(&mut rng));
        }
    }
    let _ = writeln!(out, "{} job {job} finished: success", clock(&mut rng));
    out
}

/// A code generator owned by another team: on every change in src/ it prints ~150 notes that have
/// nothing to do with the work, so each build after an edit adds them to the history.
fn codegen_build() -> String {
    let mut rng = Rng::new(4242);
    let cols = ["sku", "qty", "location", "unit", "price", "batch", "expiry", "supplier", "lot"];
    let mut s = String::from(
        "// Generated-table checks of the warehouse schema. Owned by the platform team; do not edit.\nfn main() {\n    \
         println!(\"cargo:rerun-if-changed=src\");\n    for note in NOTES {\n        \
         println!(\"cargo:warning=codegen: {note}\");\n    }\n}\n\nconst NOTES: &[&str] = &[\n",
    );
    for i in 0..150 {
        let note = match rng.below(4) {
            0 => format!(
                "table t{i:03} column `{}` is deprecated since schema v{}.{}; regenerate with --schema {:08x}",
                rng.pick(&cols),
                rng.int(2, 9),
                rng.int(0, 20),
                rng.next_u64() % 0xffff_ffff
            ),
            1 => format!(
                "table t{i:03} has {} rows without an index on `{}` (informational)",
                rng.int(100, 99_999),
                rng.pick(&cols)
            ),
            2 => format!("mapping t{i:03}.{} -> legacy_{} kept for compatibility", rng.pick(&cols), rng.pick(&cols)),
            _ => format!(
                "table t{i:03} checksum {:016x} differs from the cached one, using the cached one",
                rng.next_u64()
            ),
        };
        let _ = writeln!(s, "    {note:?},");
    }
    s + "];\n"
}

fn noisy_turns() -> Vec<(u32, String)> {
    let mut t = marathon_turns();
    t.insert(
        1,
        "Next: last night's CI logs are in ci/ (run-01.log to run-12.log). Most runs show errors that a retry fixed; \
         exactly one run really failed. Find it: I will need its run number and its job id at the end. Leave \
         build.rs as it is, it belongs to another team."
            .into(),
    );
    let last = t.len() - 1;
    t[last] += " Also add the keys ci_run (the number of the CI run that really failed, as a number) and ci_job (that \
                run's job id, like \"j-1a2b3c\").";
    t.into_iter().map(|t| (1, t)).collect()
}

fn marathon_noisy() -> Task {
    let mut files = crate_files();
    files.extend(specs_scaled(20));
    files.push(("build.rs".into(), codegen_build()));
    files.extend((1..=12).map(|n| (format!("ci/run-{n:02}.log"), ci_log(n))));
    Task {
        id: "marathon-noisy",
        trap: "the heavy marathon plus junk: a build script that prints 150 useless notes after every edit and \
               twelve long CI logs with one fact in them, asked for at the very end; the history passes 200k \
               tokens early and most of it is noise",
        group: "heavy",
        files,
        turns: noisy_turns(),
        check: noisy_check,
        solution: noisy_solution,
        setup: None,
        sealed: false,
        mask: &[],
        ready: always,
    }
}

fn noisy_check(wd: &Path) -> Outcome {
    let out = marathon_check(wd);
    if !out.ok {
        return out;
    }
    let rel: serde_json::Value = serde_json::from_str(&read(wd, "RELEASE.json")).unwrap_or_default();
    let run = rel["ci_run"].as_f64().or_else(|| {
        rel["ci_run"].as_str().and_then(|s| s.chars().filter(char::is_ascii_digit).collect::<String>().parse().ok())
    });
    let job = rel["ci_job"].as_str().unwrap_or("").trim().to_string();
    Outcome::all(vec![
        unchanged(wd, "build.rs", &codegen_build()),
        (run == Some(CI_RUN as f64), format!("RELEASE.json: ci_run {}", rel["ci_run"])),
        (job == ci_job(CI_RUN), format!("RELEASE.json: ci_job {}", rel["ci_job"])),
    ])
}

fn noisy_solution(wd: &Path) {
    marathon_solution(wd);
    let mut rel: serde_json::Value =
        serde_json::from_str(&read(wd, "RELEASE.json")).expect("the solution's RELEASE.json");
    rel["ci_run"] = CI_RUN.into();
    rel["ci_job"] = ci_job(CI_RUN).into();
    std::fs::write(wd.join("RELEASE.json"), rel.to_string()).expect("a writable working directory");
}

/// A Rust expression that builds `stock` with the crate's own functions.
fn stock_expr(stock: &Plain) -> String {
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
fn marathon_probe() -> String {
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

fn marathon_check(wd: &Path) -> Outcome {
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

fn marathon_solution(wd: &Path) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_test_names_are_normalized() {
        assert_eq!(test_name("stock::remove_too_many (FAILED)"), "remove_too_many");
        assert_eq!(test_name("test_discount"), "discount");
        assert_eq!(test_name("tests/units.rs::pounds"), "pounds");
    }

    #[test]
    fn release_lists_exactly_the_tests_that_fail_at_the_start() {
        let wd = tempfile::tempdir().unwrap();
        long_haul().materialize(wd.path()).unwrap();
        let out = Copy::of(wd.path()).unwrap().cargo(&["test", "--no-fail-fast"]);
        let mut failed: Vec<&str> =
            out.stdout.lines().filter_map(|l| l.strip_prefix("test ")?.strip_suffix(" ... FAILED")).collect();
        failed.sort();
        assert_eq!(failed, FAILING, "{}", out.tail(400));
    }

    #[test]
    fn the_spec_examples_follow_the_reference() {
        let s = specs();
        assert_eq!(s.len(), 5);
        assert!(s[1].1.contains("not enough") || s[1].1.contains("Ok"));
        assert_eq!(ref_bulk(2.0, 1000), 1700.0);
        assert_eq!(ref_tonnes("2.5 t"), 2500.0);
        let st: Plain = [("north", [("bolt", 5)].into_iter().collect())].into_iter().collect();
        assert_eq!(ref_markdown(&st).lines().last(), Some("| bolt | 5 | 0 | 0 | 0 | 5 |"));
    }
}

#[cfg(test)]
mod heavy {
    use super::*;

    #[test]
    fn the_heavy_specs_hold_twenty_times_the_examples() {
        let bytes = |s: Vec<(String, String)>| s.iter().map(|(_, t)| t.len()).sum::<usize>();
        let (one, eight) = (bytes(specs()), bytes(specs_scaled(20)));
        eprintln!("spec bytes: {one} and {eight}");
        assert!(eight > 15 * one, "{one} {eight}");
        assert!(specs_scaled(20)[0].1.starts_with(&specs()[0].1[..200]));
    }

    /// Slow: builds the crate (run with `cargo test -p nexo-bench -- --ignored noisy`).
    #[test]
    #[ignore]
    fn the_noisy_checker_accepts_its_solution_and_rejects_the_start() {
        let task = marathon_noisy();
        let wd = tempfile::tempdir().unwrap();
        task.materialize(wd.path()).unwrap();
        assert!(!(task.check)(wd.path()).ok);
        (task.solution)(wd.path());
        let got = (task.check)(wd.path());
        assert!(got.ok, "{}", got.detail);
        let t = Copy::of(wd.path()).unwrap().cargo(&["build"]);
        assert!(t.stdout.contains("codegen:") || t.stderr.contains("codegen:"));
    }

    #[test]
    fn one_ci_run_really_failed_and_the_build_script_is_noisy() {
        let logs: Vec<String> = (1..=12).map(ci_log).collect();
        let aborted: Vec<usize> = (1..=12).filter(|n| logs[n - 1].contains("aborted")).collect();
        assert_eq!(aborted, [CI_RUN]);
        assert!(logs[CI_RUN - 1].contains(&format!("job {} aborted", ci_job(CI_RUN))));
        let mut ids: Vec<String> = (1..=12).map(ci_job).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 12);
        let bytes: usize = logs.iter().map(String::len).sum();
        eprintln!("ci bytes: {bytes}, build.rs bytes: {}", codegen_build().len());
        assert!(bytes > 250_000, "{bytes}");
        assert_eq!(codegen_build().lines().filter(|l| l.starts_with("    \"")).count(), 150);
    }
}
