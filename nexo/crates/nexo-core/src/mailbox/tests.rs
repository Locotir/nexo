use super::*;
use std::sync::atomic::{AtomicI64, Ordering};

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    now: Arc<AtomicI64>,
    open: std::cell::RefCell<Vec<Arc<Mailbox>>>,
}

impl Rig {
    fn new() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        Rig {
            root: dir.path().join("mail"),
            _dir: dir,
            now: Arc::new(AtomicI64::new(now_ms())),
            open: Default::default(),
        }
    }

    fn session(&self, id: &str, name: &str, bypass: bool, inbound: Inbound) -> Arc<Mailbox> {
        let n = self.now.clone();
        let mb = Mailbox::open_with_clock(
            &self.root,
            id,
            name,
            "/work/x",
            bypass,
            inbound,
            Arc::new(move || n.load(Ordering::SeqCst)),
        )
        .unwrap();
        self.open.borrow_mut().push(mb.clone());
        mb
    }

    /// Moves time on; every session still open beats, as a running one would.
    fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
        for s in self.open.borrow().iter() {
            s.beat();
        }
    }
}

fn pair(rig: &Rig) -> (Arc<Mailbox>, Arc<Mailbox>) {
    (rig.session("aaaa1111", "api", false, Inbound::Auto), rig.session("bbbb2222", "web", false, Inbound::Auto))
}

#[test]
fn two_sessions_exchange_a_message_end_to_end() {
    let rig = Rig::new();
    let (api, web) = pair(&rig);
    // each sees the other, and itself first
    let peers = api.peers();
    assert!(peers[0].me && peers[0].id == "aaaa1111");
    assert!(peers.iter().any(|p| p.id == "bbbb2222" && p.sendable && !p.me));

    let m =
        api.send("web", "please look at the failing test in parser.rs\nsecond line", None, FromKind::Agent).unwrap();
    assert_eq!(m.summary, "please look at the failing test in parser.rs");
    assert!(rig.root.join("bbbb2222/inbox").read_dir().unwrap().count() == 1);
    let ev = api.drain_events();
    assert!(
        matches!(&ev[0], AgentEvent::MailSent { to, to_name, .. } if to == "bbbb2222" && to_name == "web"),
        "{ev:?}"
    );

    // the receiver is mid-turn: the mail arrives and waits, nothing is delivered
    web.set_idle(false);
    let t = web.tick();
    assert!(t.deliver.is_empty(), "no delivery mid-turn");
    assert!(t.events.iter().any(|e| matches!(e, AgentEvent::MailArrived { held: false, .. })), "{:?}", t.events);
    assert_eq!(web.waiting(), 1);
    for _ in 0..3 {
        assert!(web.tick().deliver.is_empty(), "still mid-turn");
    }

    // idle: the mail is handed over once, and the inbox file moves to done/
    web.set_idle(true);
    let t = web.tick();
    assert_eq!(t.deliver.len(), 1);
    assert_eq!(t.deliver[0].id, m.id);
    assert!(web.tick().deliver.is_empty(), "once");
    assert_eq!(rig.root.join("bbbb2222/inbox").read_dir().unwrap().count(), 0);
    assert_eq!(rig.root.join("bbbb2222/done").read_dir().unwrap().count(), 1);
    // the gate closes: the turn that starts is the mail's
    let rendered = render(&t.deliver);
    assert!(rendered.contains("from=\"api\"") && rendered.contains("not from the person"));

    // receipts climb the ladder on the sender's side
    let ev = web.delivered(&t.deliver);
    assert!(matches!(ev, AgentEvent::MailDelivered { ref ids } if ids == &vec![m.id.clone()]));
    let st = api.tick().events;
    assert!(st.iter().any(|e| matches!(e, AgentEvent::MailStage { stage: Stage::Delivered, .. })), "{st:?}");
    web.read(&t.deliver);
    let st = api.tick().events;
    assert!(st.iter().any(|e| matches!(e, AgentEvent::MailStage { stage: Stage::Read, .. })), "{st:?}");
    assert!(api.tick().events.is_empty(), "a stage is reported once");

    // and the answer travels back the same way
    let back = web.send("api", "done, it was a stale fixture", None, FromKind::Agent).unwrap();
    assert_eq!(back.reply_to.as_deref(), Some(m.id.as_str()), "an answer to the mail it got");
    api.set_idle(true);
    assert_eq!(api.tick().deliver.len(), 1);
}

#[test]
fn size_cap_refuses_and_never_cuts() {
    let rig = Rig::new();
    let (api, _web) = pair(&rig);
    let big = "x".repeat(MAIL_MAX_BYTES + 1);
    assert_eq!(api.send("web", &big, None, FromKind::Agent).unwrap_err(), "Not sent: too big (8193 > 8192 bytes)");
    assert!(api.send("web", &"y".repeat(MAIL_MAX_BYTES), None, FromKind::Agent).is_ok(), "the cap itself is fine");
    assert!(api.send("web", "  ", None, FromKind::Agent).is_err());
}

#[test]
fn rate_limit_is_a_bucket_of_three_refilling_six_a_minute() {
    let rig = Rig::new();
    let (api, _web) = pair(&rig);
    for i in 0..3 {
        api.send("web", &format!("message {i}"), None, FromKind::Agent).unwrap();
    }
    let e = api.send("web", "message 3", None, FromKind::Agent).unwrap_err();
    assert!(e.starts_with("Not sent: rate limit, retry in ") && e.ends_with("batch your messages"), "{e}");
    rig.advance(10_000);
    assert!(api.send("web", "message 4", None, FromKind::Agent).is_ok(), "one token came back after 10 s");
    assert!(api.send("web", "message 5", None, FromKind::Agent).is_err());
    // the person's own message is not rate limited
    assert!(api.send("web", "message 6", None, FromKind::Person).is_ok());
}

#[test]
fn identical_text_within_a_minute_is_dropped() {
    let rig = Rig::new();
    let (api, _web) = pair(&rig);
    api.send("web", "same", None, FromKind::Agent).unwrap();
    let e = api.send("web", "same", None, FromKind::Agent).unwrap_err();
    assert!(e.contains("less than a minute ago"), "{e}");
    rig.advance(61_000);
    assert!(api.send("web", "same", None, FromKind::Agent).is_ok());
}

#[test]
fn twenty_sends_an_hour() {
    let rig = Rig::new();
    let api = rig.session("aaaa1111", "api", false, Inbound::Auto);
    let mut others = Vec::new();
    for i in 0..8 {
        others.push(rig.session(&format!("cccc{i}000"), &format!("peer{i}"), false, Inbound::Auto));
    }
    let mut sent = 0;
    let mut err = None;
    for i in 0..30 {
        match api.send(&format!("peer{}", i % 8), &format!("m {i}"), None, FromKind::Agent) {
            Ok(_) => sent += 1,
            Err(e) => {
                err = Some(e);
                break;
            }
        }
        rig.advance(1000);
    }
    assert_eq!(sent, HOURLY_MAX);
    assert!(err.unwrap().contains("in the last hour"));
}

#[test]
fn a_full_inbox_and_an_unknown_session_are_errors_the_model_can_read() {
    let rig = Rig::new();
    let (api, _web) = pair(&rig);
    for i in 0..INBOX_MAX {
        api.send("web", &format!("m{i}"), None, FromKind::Person).unwrap();
    }
    assert_eq!(api.send("web", "one more", None, FromKind::Person).unwrap_err(), "Not sent: inbox of \"web\" is full");
    assert_eq!(
        api.send("nobody", "hi", None, FromKind::Agent).unwrap_err(),
        "Not sent: no live session \"nobody\". Live sessions: web."
    );
    assert_eq!(
        api.send("api", "hi", None, FromKind::Agent).unwrap_err(),
        "Not sent: \"api\" is this session, the message would go to itself. Live sessions: web."
    );
    // the person's failed send (the full inbox) is shown; the model's two are only in its tool result
    let failed: Vec<String> = api
        .drain_events()
        .into_iter()
        .filter_map(|e| match e {
            AgentEvent::MailFailed { to, .. } => Some(to),
            _ => None,
        })
        .collect();
    assert_eq!(failed, ["web"]);
    api.send("nobody", "hi", None, FromKind::Person).unwrap_err();
    assert!(api.drain_events().iter().any(|e| matches!(e, AgentEvent::MailFailed { to, .. } if to == "nobody")));
}

#[test]
fn a_model_s_failed_send_pushes_no_event_for_the_screen() {
    let rig = Rig::new();
    let (api, _web) = pair(&rig);
    let e = api.send("nobody", "hi", None, FromKind::Agent).unwrap_err();
    assert_eq!(e, "Not sent: no live session \"nobody\". Live sessions: web.");
    assert!(api.send("web", "", None, FromKind::Agent).is_err());
    assert!(api.drain_events().is_empty());
}

#[test]
fn an_unknown_name_says_who_is_live_or_that_nobody_is() {
    let rig = Rig::new();
    let api = rig.session("aaaa1111", "api", false, Inbound::Auto);
    assert_eq!(
        api.send("web", "hi", None, FromKind::Agent).unwrap_err(),
        "Not sent: no live session \"web\". No other session is live."
    );
    let _w1 = rig.session("bbbb2222", "web", false, Inbound::Auto);
    let _w2 = rig.session("cccc3333", "web", false, Inbound::Auto);
    let e = api.send("10", "hi", None, FromKind::Agent).unwrap_err();
    assert_eq!(e, "Not sent: no live session \"10\". Live sessions: web [bbbb22], web [cccc33].");
}

#[test]
fn a_dead_or_silent_session_is_not_listed() {
    let rig = Rig::new();
    let (api, web) = pair(&rig);
    assert!(api.peers().iter().any(|p| p.id == "bbbb2222"));
    rig.now.fetch_add(LIVE_MS + 1000, Ordering::SeqCst); // time passes without a beat
    api.tick(); // api beats, web does not
    assert!(!api.peers().iter().any(|p| p.id == "bbbb2222"), "no beat for 20 s");
    web.tick();
    assert!(api.peers().iter().any(|p| p.id == "bbbb2222"));
    web.close();
    assert!(!api.peers().iter().any(|p| p.id == "bbbb2222"), "left");
}

#[test]
fn names_resolve_by_id_prefix_name_and_name_with_ref() {
    let rig = Rig::new();
    let api = rig.session("aaaa1111", "api", false, Inbound::Auto);
    rig.session("3fa9c1aa", "web", false, Inbound::Auto);
    rig.session("77aa01bb", "web", false, Inbound::Auto);
    assert_eq!(api.resolve("3fa9c1aa").unwrap().id, "3fa9c1aa");
    assert_eq!(api.resolve("77aa").unwrap().id, "77aa01bb");
    assert_eq!(api.resolve("web [77aa]").unwrap().id, "77aa01bb");
    let e = api.resolve("web").unwrap_err();
    assert!(e.contains("web [3fa9c1]") && e.contains("web [77aa01]") && e.contains("/work/x"), "{e}");
}

#[test]
fn the_inbound_policy_holds_refuses_and_expires() {
    let rig = Rig::new();
    let api = rig.session("aaaa1111", "api", true, Inbound::Auto);
    let _web = rig.session("bbbb2222", "web", false, Inbound::Auto);
    // a bypass sender to an asking receiver: held
    let m = api.send("web", "run the migration", None, FromKind::Agent).unwrap();
    let t = web.tick();
    assert!(t.events.iter().any(|e| matches!(e, AgentEvent::MailArrived { held: true, .. })));
    web.set_idle(true);
    assert!(web.tick().deliver.is_empty(), "held mail is not delivered");
    assert_eq!(web.held().len(), 1);
    let st = api.tick().events;
    assert!(st.iter().any(|e| matches!(e, AgentEvent::MailStage { stage: Stage::Held, .. })), "{st:?}");
    // the person accepts it
    assert!(web.decide(&m.id, true));
    assert_eq!(web.tick().deliver.len(), 1);

    // denied
    let m2 = api.send("web", "other thing", None, FromKind::Agent).unwrap();
    web.tick();
    assert!(web.decide(&m2.id[..8], false));
    assert!(api.tick().events.iter().any(|e| matches!(e, AgentEvent::MailStage { stage: Stage::Refused, .. })));

    // nobody answers: expired after five minutes
    rig.advance(1000);
    api.send("web", "third", None, FromKind::Agent).unwrap();
    web.tick();
    rig.advance(HOLD_MS + 1000);
    web.tick();
    assert!(web.held().is_empty());
    assert!(api.tick().events.iter().any(|e| matches!(e, AgentEvent::MailStage { stage: Stage::Expired, .. })));

    // refuse everything
    let cold = rig.session("dddd4444", "cold", false, Inbound::Refuse);
    rig.advance(70_000);
    api.send("cold", "hello", None, FromKind::Agent).unwrap();
    cold.set_idle(true);
    assert!(cold.tick().deliver.is_empty());
}

#[test]
fn a_ping_pong_is_cut_after_four_replies() {
    let rig = Rig::new();
    let (api, web) = pair(&rig);
    api.set_idle(true);
    web.set_idle(true);
    let mut refused_at = None;
    for i in 0..12 {
        rig.advance(65_000);
        let (from, to) = if i % 2 == 0 { (&api, &web) } else { (&web, &api) };
        let target = if i % 2 == 0 { "web" } else { "api" };
        match from.send(target, &format!("ping {i}"), None, FromKind::Agent) {
            Ok(_) => {
                to.set_idle(true);
                assert_eq!(to.tick().deliver.len(), 1, "round {i}");
            }
            Err(e) => {
                assert!(e.contains("back and forth"), "{e}");
                refused_at = Some(i);
                break;
            }
        }
    }
    let at = refused_at.expect("the chain is cut");
    assert!((CHAIN_MAX..=CHAIN_MAX + 2).contains(&at), "cut at {at}");
    // the person's message is never cut
    assert!(api.send("web", "are you two stuck?", None, FromKind::Person).is_ok());
}

#[test]
fn secrets_do_not_hop() {
    let rig = Rig::new();
    let (api, web) = pair(&rig);
    let key = format!("{}{}", "s", "k-abcdefghijklmnopqrstuvwx1234");
    let m = api.send("web", &format!("use {key} to log in"), Some(&format!("key {key}")), FromKind::Agent).unwrap();
    assert!(!m.text.contains(&key) && !m.summary.contains(&key));
    let ledger = fs::read_to_string(rig.root.join("aaaa1111/sent.jsonl")).unwrap();
    assert!(!ledger.contains(&key) && ledger.contains(&m.id), "{ledger}");
    assert!(!ledger.contains("log in"), "the text itself is not kept in the ledger");
    let inbox = fs::read_dir(rig.root.join("bbbb2222/inbox")).unwrap().next().unwrap().unwrap().path();
    assert!(!fs::read_to_string(inbox).unwrap().contains(&key));
    web.set_idle(true);
    assert!(!render(&web.tick().deliver).contains(&key));
}

#[test]
fn bad_files_go_to_bad_and_the_poll_reads_eight() {
    let rig = Rig::new();
    let (api, web) = pair(&rig);
    let inbox = rig.root.join("bbbb2222/inbox");
    fs::write(inbox.join("0000000000001-junk.json"), "{not json").unwrap();
    fs::write(inbox.join("0000000000002-old.json"), r#"{"v":9}"#).unwrap();
    for i in 0..10 {
        api.send("web", &format!("n{i}"), None, FromKind::Person).unwrap();
    }
    let t = web.tick();
    // 2 junk files + 6 mails read in the first poll, the rest after
    assert_eq!(t.events.iter().filter(|e| matches!(e, AgentEvent::MailArrived { .. })).count(), 6);
    assert_eq!(rig.root.join("bbbb2222/bad").read_dir().unwrap().count(), 2);
    let t = web.tick();
    assert_eq!(t.events.iter().filter(|e| matches!(e, AgentEvent::MailArrived { .. })).count(), 4);
}

#[cfg(unix)]
#[test]
fn folders_and_files_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new();
    let (api, _web) = pair(&rig);
    api.send("web", "hi", None, FromKind::Agent).unwrap();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&rig.root.join("bbbb2222/inbox")), 0o700);
    assert_eq!(mode(&rig.root.join("aaaa1111/session.json")), 0o600);
    assert_eq!(mode(&rig.root.join("aaaa1111/sent.jsonl")), 0o600);
    let f = fs::read_dir(rig.root.join("bbbb2222/inbox")).unwrap().next().unwrap().unwrap().path();
    assert_eq!(mode(&f), 0o600);
}

#[test]
fn the_ledgers_are_read_once_from_an_offset() {
    let rig = Rig::new();
    let (api, _web) = pair(&rig);
    api.send("web", "one", None, FromKind::Agent).unwrap();
    let mut off = HashMap::new();
    let first = read_ledgers(&rig.root, &mut off);
    assert_eq!(first.len(), 1);
    assert_eq!((first[0].from_name.as_str(), first[0].to_name.as_str(), first[0].bytes), ("api", "web", 3));
    assert!(read_ledgers(&rig.root, &mut off).is_empty());
    rig.advance(1000);
    api.send("web", "two", None, FromKind::Agent).unwrap();
    assert_eq!(read_ledgers(&rig.root, &mut off).len(), 1);
}

fn job(id: u32, kind: crate::jobs::JobKind, label: &str, status: crate::jobs::JobStatus) -> crate::jobs::JobInfo {
    crate::jobs::JobInfo { id, kind, label: label.into(), status, elapsed_ms: 0, lines: 0 }
}

#[test]
fn live_sessions_lists_the_fresh_heartbeats_and_not_the_stale_ones() {
    let rig = Rig::new();
    let _api = rig.session("aaaa1111", "api", false, Inbound::Auto);
    let web = rig.session("bbbb2222", "web", false, Inbound::Auto);
    // a session that stopped beating a minute ago, with its process still there
    let old = rig.now.load(Ordering::SeqCst) - 60_000;
    let _gone =
        Mailbox::open_with_clock(&rig.root, "cccc3333", "gone", "/w", false, Inbound::Auto, Arc::new(move || old))
            .unwrap();
    web.set_idle(true);
    let live = live_sessions(&rig.root, rig.now.load(Ordering::SeqCst));
    let seen: Vec<(&str, Option<bool>)> = live.iter().map(|s| (s.id.as_str(), s.busy)).collect();
    assert_eq!(seen, vec![("aaaa1111", Some(true)), ("bbbb2222", Some(false))]);
    assert_eq!((live[0].name.as_str(), live[0].cwd.as_str()), ("api", "/work/x"));
}

#[test]
fn live_sessions_carry_the_running_jobs_and_nothing_else_of_them() {
    use crate::jobs::{JobKind, JobStatus};
    let rig = Rig::new();
    let api = rig.session("aaaa1111", "api", false, Inbound::Auto);
    let long = "x".repeat(300);
    api.set_jobs(&[
        job(1, JobKind::Shell, "cargo test\n--all", JobStatus::Running),
        job(2, JobKind::Agent, "review the diff", JobStatus::Running),
        job(3, JobKind::Shell, "sleep 1", JobStatus::Done { ok: true, how: "exit 0".into() }),
        job(4, JobKind::Shell, &long, JobStatus::Running),
    ]);
    let live = live_sessions(&rig.root, rig.now.load(Ordering::SeqCst));
    let jobs = &live[0].jobs;
    let ids: Vec<u32> = jobs.iter().map(|j| j.id).collect();
    assert_eq!(ids, vec![1, 2, 4], "an ended job is not listed");
    assert_eq!(
        (jobs[0].kind, jobs[0].state.as_str(), jobs[0].title.as_str()),
        (JobKind::Shell, "running", "cargo test--all")
    );
    assert_eq!(jobs[1].kind, JobKind::Agent);
    assert_eq!(jobs[2].title.chars().count(), 80);
    api.set_jobs(&[]);
    assert!(live_sessions(&rig.root, rig.now.load(Ordering::SeqCst))[0].jobs.is_empty());
}

#[test]
fn a_heartbeat_of_an_older_nexo_is_live_with_no_state() {
    let rig = Rig::new();
    let dir = rig.root.join("dddd4444");
    fs::create_dir_all(&dir).unwrap();
    let now = rig.now.load(Ordering::SeqCst);
    let old = format!(
        r#"{{"v":{MAIL_VERSION},"id":"dddd4444","name":"old","cwd":"/w","pid":{},"started":{now},"beat":{now},"bypass":false}}"#,
        std::process::id()
    );
    fs::write(dir.join("session.json"), old).unwrap();
    let live = live_sessions(&rig.root, now);
    assert_eq!((live.len(), live[0].busy, live[0].jobs.len()), (1, None, 0));
}

/// A heartbeat file's text for `id`, padded with spaces to `width` bytes so two of them can share a size.
fn beat_text(id: &str, pid: u32, beat: i64, width: usize) -> String {
    let t = format!(
        r#"{{"v":{MAIL_VERSION},"id":"{id}","name":"n","cwd":"/w","pid":{pid},"started":{beat},"beat":{beat},"bypass":false}}"#
    );
    format!("{t:<width$}")
}

/// Puts a file's modification time `secs` seconds in the past.
fn age_file(path: &Path, secs: u64) {
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

#[test]
fn a_heartbeat_found_dead_is_not_read_again_until_it_is_written_again() {
    let rig = Rig::new();
    let now = rig.now.load(Ordering::SeqCst);
    let dir = rig.root.join("eeee5555");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("session.json");
    // a process that is gone, as a session that never closed leaves it
    fs::write(&file, beat_text("eeee5555", 0, now, 200)).unwrap();
    age_file(&file, 30);
    let mut scan = LiveScan::default();
    assert!(scan.scan(&rig.root, now).is_empty());
    assert_eq!(scan.skipped(), 1);
    // the same size and time with live text in it: only a read would see it, and none happens
    let stamp = fs::metadata(&file).unwrap().modified().unwrap();
    fs::write(&file, beat_text("eeee5555", std::process::id(), now, 200)).unwrap();
    fs::File::options().write(true).open(&file).unwrap().set_modified(stamp).unwrap();
    assert!(scan.scan(&rig.root, now).is_empty(), "an unchanged stamp is not read again");
    // written again, as a session that resumes does: it is live at once and no longer remembered as dead
    age_file(&file, 0);
    assert_eq!(scan.scan(&rig.root, now).len(), 1);
    assert_eq!(scan.skipped(), 0);
}

#[test]
fn a_heartbeat_written_a_moment_ago_is_read_again_even_when_it_was_dead() {
    let rig = Rig::new();
    let now = rig.now.load(Ordering::SeqCst);
    let dir = rig.root.join("ffff6666");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("session.json"), beat_text("ffff6666", 0, now, 200)).unwrap();
    let mut scan = LiveScan::default();
    assert!(scan.scan(&rig.root, now).is_empty());
    assert_eq!(scan.skipped(), 0, "a file this new could be rewritten under the same stamp");
}

#[test]
fn a_dead_heartbeat_older_than_a_day_is_removed_and_nothing_else_is() {
    let rig = Rig::new();
    let me = rig.session("aaaa1111", "api", false, Inbound::Auto);
    let web = rig.session("bbbb2222", "web", false, Inbound::Auto);
    let now = rig.now.load(Ordering::SeqCst);
    let beat = |sid: &str| rig.root.join(sid).join("session.json");
    let make = |sid: &str, pid: u32, age_secs: u64| {
        fs::create_dir_all(rig.root.join(sid).join("inbox")).unwrap();
        fs::write(beat(sid), beat_text(sid, pid, now, 0)).unwrap();
        fs::write(rig.root.join(sid).join("inbox/m.json"), "{}").unwrap();
        age_file(&beat(sid), age_secs);
    };
    let day = 24 * 3600;
    make("dead-old", 0, day + 600);
    make("dead-new", 0, 600);
    make("alive-old", std::process::id(), day + 600);
    me.purge(now);
    assert!(!beat("dead-old").exists(), "a dead heartbeat a day old goes");
    assert!(beat("dead-new").exists(), "a recent one stays");
    assert!(beat("alive-old").exists(), "one whose process still exists stays");
    assert!(beat("bbbb2222").exists(), "a live session keeps its heartbeat");
    assert!(rig.root.join("dead-old/inbox/m.json").exists(), "the mail is not touched");
}
