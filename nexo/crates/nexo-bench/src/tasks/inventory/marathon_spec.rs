use super::*;

// ------------------------------------------------------------------ the marathon: the same crate, nine requests

pub(super) const ITEMS: [&str; 12] =
    ["bolt", "nut", "washer", "screw", "rivet", "hinge", "spring", "clamp", "gear", "pin", "bracket", "anchor"];
pub(super) const LOCS: [&str; 4] = ["north", "south", "east", "west"];
pub(super) const TIERS: [(i64, i64); 3] = [(1000, 15), (500, 10), (100, 5)];

/// A stock as the specifications and hidden cases write it: location -> item -> units.
pub(super) type Plain = BTreeMap<&'static str, BTreeMap<&'static str, i64>>;

pub(super) fn ref_restock(items: &[(&str, i64)], threshold: i64) -> Vec<String> {
    let mut out: Vec<String> = items.iter().filter(|(_, q)| *q < threshold).map(|(n, _)| n.to_string()).collect();
    out.sort();
    out
}

/// The units left at `location` after the reservation, or the error message.
pub(super) fn ref_reserve(
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

pub(super) fn ref_tonnes(text: &str) -> f64 {
    let t = text.trim().to_lowercase().replace(' ', "");
    crate::check::round_to(t[..t.len() - 1].parse::<f64>().unwrap_or(0.0) * 1000.0, 3)
}

pub(super) fn ref_markdown(stock: &Plain) -> String {
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

pub(super) fn ref_bulk(price: f64, units: i64) -> f64 {
    let pct = TIERS.iter().find(|(n, _)| units >= *n).map_or(0, |(_, p)| *p);
    (price * units as f64 * (100 - pct) as f64 + 0.5) as i64 as f64 / 100.0
}

pub(super) fn some_items(r: &mut Rng, lo: i64, hi: i64) -> Vec<(&'static str, i64)> {
    let k = r.int(lo, hi) as usize;
    r.sample(&ITEMS, k).into_iter().map(|n| (n, r.int(0, 20))).collect()
}

pub(super) fn some_stock(r: &mut Rng) -> Plain {
    LOCS.iter().map(|l| (*l, r.sample(&ITEMS, 2).into_iter().map(|n| (n, r.int(0, 9))).collect())).collect()
}

pub(super) fn ex_restock(r: &mut Rng) -> String {
    let items = some_items(r, 2, 5);
    let th = r.int(1, 15);
    format!("| `{items:?}` | {th} | `{:?}` |", ref_restock(&items, th))
}

pub(super) fn ex_reserve(r: &mut Rng) -> String {
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

pub(super) fn tonnes_text(v: f64, r: &mut Rng) -> String {
    let sp = if r.chance(0.5) { " " } else { "" };
    format!("{v}{sp}t")
}

pub(super) fn ex_tonnes(r: &mut Rng) -> String {
    let v = match r.below(3) {
        0 => r.int(1, 40) as f64 / 4.0,
        1 => r.int(1, 9) as f64,
        _ => r.int(1, 999) as f64 / 1000.0,
    };
    let t = tonnes_text(v, r);
    format!("| `{t}` | {} |", ref_tonnes(&t))
}

pub(super) fn bulk_units(r: &mut Rng) -> i64 {
    match r.below(4) {
        0 => r.int(1, 99),
        1 => r.int(100, 499),
        2 => r.int(500, 999),
        _ => r.int(1000, 3000),
    }
}

pub(super) fn ex_bulk(r: &mut Rng) -> String {
    let price = r.int(1, 500) as f64 / 100.0;
    let units = bulk_units(r);
    format!("| {price} | {units} | {} |", ref_bulk(price, units))
}

pub(super) fn ex_markdown(r: &mut Rng) -> String {
    let st = some_stock(r);
    format!("| `{st:?}` | `{:?}` |", ref_markdown(&st))
}

pub(super) fn rows(n: usize, seed: u64, make: fn(&mut Rng) -> String) -> Vec<String> {
    let mut r = Rng::new(seed);
    (0..n).map(|_| make(&mut r)).collect()
}

pub(super) const NOTE: &str =
    "This specification was agreed with the warehouse team after the spring stock-take, when several \
                    manual spreadsheets disagreed with the system. The wording of each rule is deliberate: when the \
                    code and this page disagree, the page wins and the code is a bug. Questions go to the maintainer \
                    named in the release notes, not to the warehouse staff.";

pub(super) fn spec(
    title: &str,
    purpose: &str,
    interface: &str,
    rules: &[&str],
    head: &str,
    rows: Vec<String>,
) -> String {
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

pub(super) fn specs() -> Vec<(String, String)> {
    specs_scaled(1)
}

/// The five specifications with `k` times the rows of examples each.
pub(super) fn specs_scaled(k: usize) -> Vec<(String, String)> {
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
