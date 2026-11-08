mod common;

use vault::auth::*;
use vault::error::VaultError;

#[test]
fn the_same_password_under_another_salt_hashes_differently() {
    assert_eq!(hash_password("s1", "pw"), hash_password("s1", "pw"));
    assert_ne!(hash_password("s1", "pw"), hash_password("s2", "pw"));
    assert_ne!(digest(&["ab", "c"]), digest(&["a", "bc"]));
}

#[test]
fn a_token_names_its_user() {
    let s = common::service();
    let alice = s.users.by_name("alice").unwrap().id;
    let token = issue_token("k", alice);
    assert_eq!(verify_token(&s.users, "k", &token), Ok(alice));
    assert!(token.starts_with(&format!("{alice}.")));
}

#[test]
fn a_tampered_or_foreign_token_is_refused() {
    let s = common::service();
    let alice = s.users.by_name("alice").unwrap().id;
    let token = issue_token("k", alice);
    assert!(verify_token(&s.users, "other-secret", &token).is_err());
    let forged = token.replacen(&format!("{alice}."), "1.", 1);
    assert!(matches!(verify_token(&s.users, "k", &forged), Err(VaultError::Unauthenticated(_))));
    for junk in ["", "7", "x.y", "999.0000000000000000", "1."] {
        assert!(verify_token(&s.users, "k", junk).is_err(), "{junk}");
    }
}

#[test]
fn tokens_match_compares_whole_strings() {
    assert!(tokens_match("abc123", "abc123"));
    assert!(!tokens_match("abc123", "abc124"));
    assert!(!tokens_match("abc123", "abc12"));
    assert!(!tokens_match("", "a"));
    assert!(tokens_match("", ""));
}

#[test]
fn login_gives_one_error_for_an_unknown_name_and_a_wrong_password() {
    let s = common::service();
    let token = login(&s.users, "k", "alice", "alice-secret1").unwrap();
    assert!(verify_token(&s.users, "k", &token).is_ok());
    let wrong = login(&s.users, "k", "alice", "nope-nope1").unwrap_err();
    let unknown = login(&s.users, "k", "nobody", "alice-secret1").unwrap_err();
    assert_eq!(wrong, unknown);
    assert_eq!(wrong.status(), 401);
    assert!(login(&s.users, "k", "ALICE", "alice-secret1").is_ok());
}
