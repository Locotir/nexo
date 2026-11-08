use vault::error::VaultError;
use vault::query::*;
use vault::store::Store;

fn store() -> Store {
    let mut s = Store::new();
    s.insert(1, "Alpha plan", "short", vec!["work".into()], 1);
    s.insert(1, "Holiday photos", &"x".repeat(300), vec!["home".into(), "photos".into()], 2);
    s.insert(2, "Payroll", "confidential", vec!["work".into(), "hr".into()], 3);
    s.insert(2, "Alpha notes", "mine", vec![], 4);
    s
}

#[test]
fn conditions_compare_fields() {
    let s = store();
    assert_eq!(run(&s, "owner = 1").unwrap(), [1, 2]);
    assert_eq!(run(&s, "owner != 1").unwrap(), [3, 4]);
    assert_eq!(run(&s, "title ~ \"alpha\"").unwrap(), [1, 4]);
    assert_eq!(run(&s, "title = \"payroll\"").unwrap(), [3]);
    assert_eq!(run(&s, "body ~ \"conf\"").unwrap(), [3]);
    assert_eq!(run(&s, "size > 100").unwrap(), [2]);
    assert_eq!(run(&s, "size < 4").unwrap(), Vec::<u64>::new());
    assert_eq!(run(&s, "tag = \"work\"").unwrap(), [1, 3]);
    assert_eq!(run(&s, "tag != \"work\"").unwrap(), [2, 4]);
    assert_eq!(run(&s, "tag ~ \"hot\"").unwrap(), [2]);
}

#[test]
fn and_binds_tighter_than_or() {
    let s = store();
    assert_eq!(run(&s, "owner = 1 AND tag = \"home\" OR title ~ \"payroll\"").unwrap(), [2, 3]);
    assert_eq!(run(&s, "title ~ \"payroll\" or owner = 1 and tag = \"work\"").unwrap(), [1, 3]);
    assert_eq!(parse("owner = 1 OR owner = 2 OR owner = 3").unwrap().clause_count(), 3);
}

#[test]
fn bad_filters_are_refused() {
    let s = store();
    for bad in [
        "",
        "owner",
        "owner =",
        "owner = \"1\"",
        "title = 5",
        "title < 5",
        "size ~ 3",
        "color = 1",
        "owner = 1 title",
        "title ~ \"open",
        "owner ? 1",
        "owner = 1 AND",
    ] {
        assert!(run(&s, bad).is_err(), "{bad:?}");
    }
    assert!(matches!(parse("owner = 1 AND"), Err(VaultError::Malformed(_))));
    assert!(matches!(parse("title < 3"), Err(VaultError::Invalid(_))));
}

#[test]
fn a_search_stays_with_the_owner() {
    let s = store();
    assert_eq!(search(&s, 1, "alpha").unwrap(), [1]);
    assert_eq!(search(&s, 2, "alpha").unwrap(), [4]);
    assert_eq!(search(&s, 1, "  holiday ").unwrap(), [2]);
    assert!(search(&s, 1, "payroll").unwrap().is_empty());
    assert_eq!(search_filter(7, "x y").unwrap(), "owner = 7 AND title ~ \"x y\"");
}

#[test]
fn search_terms_are_checked_for_size() {
    let s = store();
    assert!(search(&s, 1, "").is_err());
    assert!(search(&s, 1, &"a".repeat(MAX_TERM + 1)).is_err());
    assert!(search(&s, 1, &"a".repeat(MAX_TERM)).is_ok());
}

#[test]
fn numbers_and_text_keep_their_place() {
    let s = store();
    assert_eq!(run(&s, "owner = -1").unwrap(), Vec::<u64>::new());
    assert_eq!(run(&s, "body ~ \"owner = 1\"").unwrap(), Vec::<u64>::new());
    assert_eq!(run(&s, "  title   ~   \"HOLIDAY\"  ").unwrap(), [2]);
}
