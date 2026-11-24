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
fn bulk_decimal_half_cent_and_tiers_are_exact() {
    assert_eq!(ref_bulk(1, 410), 3.90); // binary f64 arithmetic used to give 3.89
    assert_eq!(ref_bulk(1, 409), 3.89);
    assert_eq!(ref_bulk(1, 99), 0.99);
    assert_eq!(ref_bulk(1, 500), 4.50);
    assert_eq!(ref_bulk(1, 1000), 8.50);
    assert!(marathon_probe().contains("check(0.01, 410, 3.9)"));
}

#[test]
fn marathon_rejects_reserve_mutating_unrelated_stock() {
    let task = marathon();
    let wd = tempfile::tempdir().unwrap();
    task.materialize(wd.path()).unwrap();
    (task.solution)(wd.path());
    let original = std::fs::read_to_string(wd.path().join("src/orders.rs")).unwrap();
    let baseline = (task.check)(wd.path());
    assert!(baseline.ok, "{}", baseline.detail);

    for mutation in [
        "if location == \"north\" && lines == [(\"bolt\", 2), (\"nut\", 3)] { *stock.get_mut(location).unwrap().get_mut(\"bolt\").unwrap() -= 1; }",
        "if location == \"north\" && lines == [(\"bolt\", 2)] { *stock.get_mut(\"south\").unwrap().get_mut(\"bolt\").unwrap() -= 1; }",
        "if location == \"north\" && lines == [(\"bolt\", 2)] { stock.get_mut(location).unwrap().insert(\"unrelated\".into(), 42); }",
    ] {
        let marker = "    for (item, units) in &need {";
        assert!(original.contains(marker));
        let altered = original.replacen(marker, &format!("    {mutation}\n{marker}"), 1);
        std::fs::write(wd.path().join("src/orders.rs"), altered).unwrap();
        let outcome = (task.check)(wd.path());
        assert!(!outcome.ok && outcome.detail.contains("reserve"), "{mutation}: {}", outcome.detail);
    }
}

#[test]
fn the_spec_examples_follow_the_reference() {
    let s = specs();
    assert_eq!(s.len(), 5);
    assert!(s[1].1.contains("not enough") || s[1].1.contains("Ok"));
    assert_eq!(ref_bulk(200, 1000), 1700.0);
    assert_eq!(ref_tonnes("2.5 t"), 2500.0);
    let st: Plain = [("north", [("bolt", 5)].into_iter().collect())].into_iter().collect();
    assert_eq!(ref_markdown(&st).lines().last(), Some("| bolt | 5 | 0 | 0 | 0 | 5 |"));
}
