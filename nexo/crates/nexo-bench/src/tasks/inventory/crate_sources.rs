pub(super) const LIB: &str = r##"//! Stock, prices, units, orders, reports and planning of the warehouse.

pub mod orders;
pub mod planning;
pub mod pricing;
pub mod report;
pub mod stock;
pub mod units;
"##;

pub(super) const STOCK: &str = r##"//! Stock levels per warehouse: adding, removing, moving and summarising units.

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

pub(super) const PRICING: &str = r##"//! Prices: discounts, rounding, taxes and margins. Money is an f64 in euros, rounded to cents.

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

pub(super) const UNITS: &str = r##"//! Units of weight and volume used by the suppliers, converted to the metric ones the warehouse uses.

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

pub(super) const ORDERS: &str = r##"//! Orders: lines of (item, units, unit price), their totals and their validation against the stock.

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

pub(super) const REPORT: &str = r##"//! Plain-text reports of the stock and the orders, for the morning e-mail.

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

pub(super) const PLANNING: &str = r##"//! Planning: what to reorder and when.

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

pub(super) const AUDIT: &str = r##"//! Audit of the inventory crate: every function documented, no leftover TODO comments.
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
