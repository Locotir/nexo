pub(super) const TEST_STOCK: &str = r##"use inventory::stock::*;

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

pub(super) const TEST_PRICING: &str = r##"use inventory::pricing::*;

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

pub(super) const TEST_UNITS: &str = r##"use inventory::units::*;

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

pub(super) const TEST_ORDERS: &str = r##"use inventory::orders::*;
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

pub(super) const TEST_REPORT: &str = r##"use inventory::report::*;
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

pub(super) const TEST_PLANNING: &str = r##"use inventory::planning::*;

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
pub(super) const TESTS: [(&str, &str); 6] = [
    ("tests/stock.rs", TEST_STOCK),
    ("tests/pricing.rs", TEST_PRICING),
    ("tests/units.rs", TEST_UNITS),
    ("tests/orders.rs", TEST_ORDERS),
    ("tests/report.rs", TEST_REPORT),
    ("tests/planning.rs", TEST_PLANNING),
];
