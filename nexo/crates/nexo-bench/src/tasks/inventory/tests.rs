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
