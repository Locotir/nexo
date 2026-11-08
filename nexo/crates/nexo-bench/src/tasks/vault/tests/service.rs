mod common;

use vault::error::VaultError;
use vault::users::Role;

fn id(s: &vault::service::Service, name: &str) -> u32 {
    s.users.by_name(name).unwrap().id
}

#[test]
fn accounts_have_unique_names_and_decent_passwords() {
    let mut s = common::service();
    assert_eq!(s.users.len(), 4);
    assert!(s.register("Alice", Role::Member, "another-pass1").is_err());
    assert!(s.register("carol", Role::Member, "short").is_err());
    assert!(s.register("c d", Role::Member, "long-enough1").is_err());
    assert!(s.users.is_admin(id(&s, "root")) && !s.users.is_admin(id(&s, "alice")));
    assert_eq!(s.users.names(), ["root", "alice", "bob", "gary"]);
}

#[test]
fn the_last_admin_stays() {
    let mut s = common::service();
    let (root, alice) = (id(&s, "root"), id(&s, "alice"));
    assert!(s.users.set_role(root, Role::Member).is_err());
    s.users.set_role(alice, Role::Admin).unwrap();
    s.users.set_role(root, Role::Member).unwrap();
    assert_eq!(Role::parse(" Admin "), Some(Role::Admin));
    assert_eq!(Role::Admin.name(), "admin");
}

#[test]
fn an_owner_reads_and_changes_their_documents() {
    let mut s = common::service();
    let alice = id(&s, "alice");
    let doc = s.create(alice, "Plan", "first", vec!["work".into()]).unwrap();
    assert_eq!(s.fetch(alice, doc).unwrap().body, "first");
    s.update(alice, doc, "second").unwrap();
    assert_eq!(s.fetch(alice, doc).unwrap().body, "second");
    assert!(s.store.get(doc).unwrap().modified > s.store.get(doc).unwrap().created);
    assert!(s.store.get(doc).unwrap().has_tag("work"));
}

#[test]
fn somebody_else_cannot_read_or_change_a_document() {
    let mut s = common::service();
    let (alice, bob) = (id(&s, "alice"), id(&s, "bob"));
    let doc = s.create(alice, "Plan", "private", vec![]).unwrap();
    assert!(matches!(s.fetch(bob, doc), Err(VaultError::Denied(_))));
    assert!(matches!(s.update(bob, doc, "mine now"), Err(VaultError::Denied(_))));
    assert_eq!(s.store.get(doc).unwrap().body, "private");
    assert!(matches!(s.fetch(bob, 999), Err(VaultError::NotFound(_))));
}

#[test]
fn admins_reach_every_document() {
    let mut s = common::service();
    let (root, alice) = (id(&s, "root"), id(&s, "alice"));
    let doc = s.create(alice, "Plan", "text", vec![]).unwrap();
    assert_eq!(s.fetch(root, doc).unwrap().owner, alice);
    s.update(root, doc, "edited by root").unwrap();
    assert_eq!(s.list(root).len(), 1);
    let gone = s.delete(root, doc).unwrap();
    assert_eq!(gone.body, "edited by root");
    assert!(s.store.is_empty());
}

#[test]
fn an_owner_deletes_their_own_document() {
    let mut s = common::service();
    let alice = id(&s, "alice");
    let doc = s.create(alice, "Plan", "text", vec![]).unwrap();
    assert!(s.delete(alice, doc).is_ok());
    assert!(matches!(s.delete(alice, doc), Err(VaultError::NotFound(_))));
    assert_eq!(s.quota.used(alice), 0);
}

#[test]
fn guests_cannot_write() {
    let mut s = common::service();
    let gary = id(&s, "gary");
    assert!(matches!(s.create(gary, "Hi", "x", vec![]), Err(VaultError::Denied(_))));
    assert!(matches!(s.create(4242, "Hi", "x", vec![]), Err(VaultError::Unauthenticated(_))));
    assert!(s.list(gary).is_empty());
}

#[test]
fn titles_and_bodies_are_limited() {
    let mut s = common::service();
    let alice = id(&s, "alice");
    assert!(s.create(alice, "  ", "x", vec![]).is_err());
    assert!(s.create(alice, &"t".repeat(121), "x", vec![]).is_err());
    assert!(s.create(alice, "big", &"b".repeat(vault::service::MAX_BODY + 1), vec![]).is_err());
    assert!(s.store.is_empty());
}

#[test]
fn the_quota_counts_creates_updates_and_deletes() {
    let mut s = common::service();
    let alice = id(&s, "alice");
    let a = s.create(alice, "A", &"x".repeat(6000), vec![]).unwrap();
    assert!(matches!(s.create(alice, "B", &"x".repeat(5000), vec![]), Err(VaultError::QuotaExceeded(_))));
    assert_eq!(s.quota.remaining(alice), 4000);
    s.update(alice, a, &"x".repeat(1000)).unwrap();
    assert_eq!(s.quota.used(alice), 1000);
    assert!(s.update(alice, a, &"x".repeat(10_001)).is_err());
    assert_eq!(s.store.bytes_of(alice), 1000);
    assert_eq!(s.quota.limit(), 10_000);
}

#[test]
fn listing_and_searching_stay_with_the_caller() {
    let mut s = common::service();
    let (alice, bob) = (id(&s, "alice"), id(&s, "bob"));
    s.create(alice, "Budget 2026", "a", vec![]).unwrap();
    s.create(bob, "Budget draft", "b", vec![]).unwrap();
    s.create(alice, "Notes", "c", vec![]).unwrap();
    assert_eq!(s.list(alice).len(), 2);
    let found: Vec<&str> = s.search(alice, "budget").unwrap().iter().map(|d| d.title.as_str()).collect();
    assert_eq!(found, ["Budget 2026"]);
    assert!(s.search(alice, "   ").is_err());
}

#[test]
fn the_audit_log_follows_the_actions() {
    let mut s = common::service();
    let alice = id(&s, "alice");
    let doc = s.create(alice, "Plan", "x", vec![]).unwrap();
    s.update(alice, doc, "y").unwrap();
    s.delete(alice, doc).unwrap();
    assert_eq!(s.audit.count("create"), 1);
    assert_eq!(s.audit.last().unwrap().action, "delete");
    assert_eq!(s.audit.for_user(alice).len(), 4);
    assert!(s.audit.render().lines().last().unwrap().ends_with(&format!("delete {doc}")));
}

#[test]
fn the_audit_log_is_bounded_and_one_line_per_entry() {
    let mut log = vault::audit::AuditLog::new(3);
    for n in 0..5 {
        log.record(1, "ping", &format!("n{n}\nforged 9 user=0 login"));
    }
    assert_eq!(log.len(), 3);
    assert_eq!(log.render().lines().count(), 3);
    assert_eq!(log.last().unwrap().seq, 5);
    assert!(!log.is_empty());
}
