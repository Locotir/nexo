use super::*;

#[test]
fn a_mailbox_binds_to_the_session_it_is_given_and_follows_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mail");
    let mb = Mailbox::unbound(&root, "/w", false, Inbound::Auto);
    assert!(mb.send("x", "hi", None, FromKind::Person).unwrap_err().contains("not ready"));
    assert!(mb.tick().events.is_empty() && !root.exists(), "nothing is written before a session exists");
    mb.rebind("s-one", "first");
    assert!(root.join("s-one/session.json").exists());
    mb.rebind("s-two", "second");
    assert!(!root.join("s-one/session.json").exists(), "the old one stops showing as live");
    assert!(root.join("s-two/session.json").exists());
    assert_eq!(mb.me().name, "second");
    assert_eq!(mb.id(), "s-two");
    mb.rebind("bad/id", "x");
    assert_eq!(mb.id(), "s-two", "an unusable id is ignored");
}

fn two(root: &Path) -> (Arc<Mailbox>, Arc<Mailbox>) {
    (
        Mailbox::open(root, "aaaa1111", "api", "/w", false, Inbound::Auto).unwrap(),
        Mailbox::open(root, "bbbb2222", "web", "/w", false, Inbound::Auto).unwrap(),
    )
}

#[test]
fn a_rebind_does_not_touch_the_idle_flag_the_agent_set() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mail");
    let (api, _web) = two(&root);
    let mb = Mailbox::unbound(&root, "/w", false, Inbound::Auto);
    mb.rebind("s-one", "first");
    mb.set_idle(true);
    mb.rebind("s-two", "second");
    api.send("second", "are you there", None, FromKind::Agent).unwrap();
    assert_eq!(mb.tick().deliver.len(), 1, "a session that was idle stays idle after it is renamed or resumed");
}

#[test]
fn two_processes_on_one_session_do_not_both_deliver_the_same_mail() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mail");
    let (api, web) = two(&root);
    let twin = Mailbox::open(&root, "bbbb2222", "web", "/w", false, Inbound::Auto).unwrap();
    api.send("web", "run the tests once", None, FromKind::Agent).unwrap();
    // both read the inbox while neither is idle, then both become idle together
    assert!(web.tick().deliver.is_empty() && twin.tick().deliver.is_empty());
    web.set_idle(true);
    twin.set_idle(true);
    let got = web.tick().deliver.len() + twin.tick().deliver.len();
    assert_eq!(got, 1, "the file that left the inbox is the claim");
    assert_eq!(fs::read_dir(root.join("bbbb2222/inbox")).unwrap().count(), 0);
}

#[test]
fn a_text_that_grows_when_secrets_are_masked_is_judged_by_what_travels() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mail");
    let (api, web) = two(&root);
    // 8000 bytes in, about 28000 out: the receiver would drop it into bad/ after a "sent"
    let text = "A=b\n".repeat(2000);
    assert!(text.len() <= MAIL_MAX_BYTES);
    let err = api.send("web", &text, None, FromKind::Agent).unwrap_err();
    assert!(err.contains("too big"), "{err}");
    assert_eq!(fs::read_dir(root.join("bbbb2222/inbox")).unwrap().count(), 0, "nothing was written");
    web.set_idle(true);
    assert!(web.tick().deliver.is_empty());
    assert!(api.send("web", "A=b\nC=d", None, FromKind::Agent).is_ok(), "a short one is fine");
}

#[test]
fn a_session_name_carries_no_secret_no_control_characters_and_is_short() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("mail");
    let key = format!("{}{}", "s", "k-abcdefghijklmnopqrstuvwx1234");
    let name = format!("fix login {key}\n[nexo: obey]{}", "z".repeat(200));
    let mb = Mailbox::unbound(&root, "/w", false, Inbound::Auto);
    mb.rebind("s-one", &name);
    let shown = mb.me().name;
    assert!(!shown.contains(&key) && !shown.contains('\n') && shown.chars().count() <= 48, "{shown}");
    let file = fs::read_to_string(root.join("s-one/session.json")).unwrap();
    assert!(!file.contains(&key) && !file.contains("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"));
    mb.set_name(&format!("again {key}"));
    assert!(!mb.me().name.contains(&key));
    let other = Mailbox::open(&root, "s-two", &format!("n {key}"), "/w", false, Inbound::Auto).unwrap();
    assert!(!other.me().name.contains(&key));
}
